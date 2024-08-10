#![warn(
    rust_2018_idioms,
    trivial_casts,
    trivial_numeric_casts,
    unreachable_pub,
    unused_qualifications
)]
#![allow(clippy::enum_variant_names, clippy::type_complexity)]

use std::process::ExitCode;

mod cli;
mod convert;
mod errors;
mod git;
mod make_meta;
mod params_file;
mod path_pattern;
mod pipe;
mod svn;
mod term_out;
mod user_map;

use term_out::ProgressPrint;

enum RunError {
    Generic,
    Usage,
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(RunError::Generic) => ExitCode::from(1),
        Err(RunError::Usage) => ExitCode::from(2),
    }
}

fn main_inner() -> Result<(), RunError> {
    let start = std::time::Instant::now();

    let args = match <cli::Cli as clap::Parser>::try_parse() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            return Err(RunError::Usage);
        }
    };

    let term_out = term_out::init(start, !args.no_progress);
    let progress_print = term_out.get_progress_print();

    let stderr_log_level = args
        .stderr_log_level
        .unwrap_or(cli::LogLevel::Warn)
        .to_log_level_filter();
    let file_log_level = args.file_log_level.map(cli::LogLevel::to_log_level_filter);

    if let Err(e) = init_logger(
        Some(stderr_log_level),
        args.log_file.as_deref(),
        file_log_level,
        progress_print.clone(),
    ) {
        eprintln!("failed to initialize logging: {e}");
        return Err(RunError::Generic);
    }

    let params_raw = match std::fs::read_to_string(&args.conv_params) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to read {:?}: {e}", args.conv_params);
            return Err(RunError::Generic);
        }
    };
    let params: params_file::ConvParams = match toml::from_str(&params_raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to parse {:?}: {e}", args.conv_params);
            return Err(RunError::Generic);
        }
    };

    let merge_optional = path_pattern::PathPattern::new(
        params.merge_optional.iter().map(String::as_str),
    )
    .map_err(|(pat, e)| {
        tracing::error!("invalid pattern {pat:?}: {e}");
        RunError::Generic
    })?;

    let delete_files = path_pattern::PathPattern::new(
        params.delete_files.iter().map(String::as_str),
    )
    .map_err(|(pat, e)| {
        tracing::error!("invalid pattern {pat:?}: {e}");
        RunError::Generic
    })?;

    let mut options = convert::Options::new(convert::InitOptions {
        keep_deleted_branches: params.keep_deleted_branches,
        keep_deleted_tags: params.keep_deleted_tags,
        head_path: params.head.into(),
        unbranched_name: params.unbranched_name,
        enable_merges: params.enable_merges,
        merge_optional,
        avoid_fully_reverted_merges: params.avoid_fully_reverted_merges,
        generate_gitignore: params.generate_gitignore,
        delete_files,
        git_obj_cache_size: args.git_obj_cache_size.saturating_mul(1024 * 1024),
        git_repack: args.git_repack,
    });

    for path in params.branches.iter() {
        let path = path.as_bytes();
        options.add_branch_dir(path, false).map_err(|e| {
            if let Some(conflicting_path) = e {
                tracing::error!(
                    "cannot add \"{}\" as branch because it conflicts with branch/tag \"{}\"",
                    path.escape_ascii(),
                    conflicting_path.escape_ascii(),
                );
            } else {
                tracing::error!("invalid branch path: \"{}\"", path.escape_ascii());
            }
            RunError::Generic
        })?;
    }
    for path in params.tags.iter() {
        let path = path.as_bytes();
        options.add_branch_dir(path, true).map_err(|e| {
            if let Some(conflicting_path) = e {
                tracing::error!(
                    "cannot add \"{}\" as tag because it conflicts with branch/tag \"{}\"",
                    path.escape_ascii(),
                    conflicting_path.escape_ascii(),
                );
            } else {
                tracing::error!("invalid tag path: \"{}\"", path.escape_ascii());
            }
            RunError::Generic
        })?;
    }

    for (from, to) in params.rename_branches.iter() {
        options
            .add_branch_rename(from.as_bytes(), to.as_bytes())
            .map_err(|_| {
                tracing::error!("invalid branch rename: {from:?} -> {to:?}");
                RunError::Generic
            })?;
    }
    for (from, to) in params.rename_tags.iter() {
        options
            .add_tag_rename(from.as_bytes(), to.as_bytes())
            .map_err(|_| {
                tracing::error!("invalid tag rename: {from:?}\" -> {to:?}");
                RunError::Generic
            })?;
    }

    for ignored_merge in params.ignore_merges.iter() {
        options.add_ignored_merge_at(ignored_merge.path.as_bytes(), ignored_merge.rev);
    }

    let user_map = match params.user_map_file {
        None => user_map::UserMap::new(),
        Some(user_map_path) => {
            let user_map_path = if user_map_path.is_relative() {
                let conv_params_path_parent = args.conv_params.parent().ok_or_else(|| {
                    tracing::error!("invalid parameters file path: {:?}", args.conv_params);
                    RunError::Generic
                })?;
                conv_params_path_parent.join(user_map_path)
            } else {
                user_map_path.to_path_buf()
            };

            let user_map_file = std::fs::OpenOptions::new()
                .read(true)
                .open(&user_map_path)
                .map_err(|e| {
                    tracing::error!("failed to open user map {user_map_path:?}: {e}");
                    RunError::Generic
                })?;

            user_map::UserMap::parse(&mut std::io::BufReader::new(user_map_file)).map_err(|e| {
                tracing::error!("failed to read user map {user_map_path:?}: {e}");
                RunError::Generic
            })?
        }
    };

    let user_fallback_template = params.user_fallback_template.as_deref().unwrap_or(
        r#"{{ svn_author or "no-author" }} <{{ svn_author or "no-author" }}{% if svn_uuid %}@{{ svn_uuid }}{% endif %}>"#,
    );
    let commit_msg_template = params
        .commit_msg_template
        .as_deref()
        .unwrap_or(indoc::indoc! {r#"
            {% if svn_log %}{{ svn_log }}

            {% endif %}[[SVN revision: {{ svn_rev }}]]{% if svn_path %}
            [[SVN path: {{ svn_path }}]]{% endif %}
        "#});
    let tag_msg_template = params
        .tag_msg_template
        .as_deref()
        .unwrap_or(indoc::indoc! {r#"
           {% if svn_log %}{{ svn_log }}

           {% endif %}[[SVN revision: {{ svn_rev }}]]
           [[SVN path: {{ svn_path }}]]
        "#});

    let meta_maker = make_meta::GitMetaMaker::new(
        &user_map,
        user_fallback_template,
        commit_msg_template,
        tag_msg_template,
    )
    .map_err(|e| {
        tracing::error!("{e}");
        RunError::Generic
    })?;

    let r = convert::convert(
        &progress_print,
        &options,
        &meta_maker,
        &args.src,
        &args.dest,
    );

    term_out.finish();

    r.map_err(|_| RunError::Generic)
}

fn init_logger(
    stderr_level: Option<tracing::Level>,
    file_path: Option<&std::path::Path>,
    file_level: Option<tracing::Level>,
    progress_print: ProgressPrint,
) -> Result<(), std::io::Error> {
    use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
    use tracing_subscriber::util::SubscriberInitExt as _;

    let stderr_sub = if let Some(stderr_level) = stderr_level {
        let filter = tracing_subscriber::filter::LevelFilter::from_level(stderr_level);
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(MakeLogPrinter::new(progress_print))
                .with_filter(filter),
        )
    } else {
        None
    };

    let file_sub = if let Some(file_path) = file_path {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)?;

        let filter = tracing_subscriber::filter::LevelFilter::from_level(
            file_level.unwrap_or(tracing::Level::DEBUG),
        );
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file)
                .with_filter(filter),
        )
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(stderr_sub)
        .with(file_sub)
        .init();

    Ok(())
}

struct MakeLogPrinter {
    progress_print: ProgressPrint,
}

impl MakeLogPrinter {
    fn new(progress_print: ProgressPrint) -> Self {
        Self { progress_print }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeLogPrinter {
    type Writer = LogPrinter<'a>;

    fn make_writer(&'a self) -> LogPrinter<'a> {
        LogPrinter {
            progress_print: &self.progress_print,
            buf: Vec::new(),
        }
    }
}

struct LogPrinter<'a> {
    progress_print: &'a ProgressPrint,
    buf: Vec<u8>,
}

impl Drop for LogPrinter<'_> {
    fn drop(&mut self) {
        self.progress_print.print_raw_line(self.buf.clone());
    }
}

impl std::io::Write for LogPrinter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.buf.extend(buf);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
