use std::path::PathBuf;

#[derive(clap::Parser)]
pub(crate) struct Cli {
    #[arg(
        long = "stderr-log-level",
        value_name = "LEVEL",
        value_enum,
        help = "Maximum stderr log level (warn by default)"
    )]
    pub(crate) stderr_log_level: Option<LogLevel>,
    #[arg(
        long = "log-file",
        value_name = "PATH",
        help = "File to write logs (besides stderr)"
    )]
    pub(crate) log_file: Option<PathBuf>,
    #[arg(
        long = "file-log-level",
        value_name = "LEVEL",
        value_enum,
        help = "Maximum file log level (debug by default)"
    )]
    pub(crate) file_log_level: Option<LogLevel>,
    #[arg(long = "no-progress", help = "Do not print progress")]
    pub(crate) no_progress: bool,
    #[arg(
        long = "src",
        short = 's',
        value_name = "PATH",
        help = "Source Subversion repository"
    )]
    pub(crate) src: PathBuf,
    #[arg(
        long = "remote-svn",
        help = "Source Subversion repository is remote (use svnrdump)"
    )]
    pub(crate) remote_svn: bool,
    #[arg(
        long = "dest",
        short = 'd',
        value_name = "PATH",
        help = "Destination where the new Git repository will be created"
    )]
    pub(crate) dest: PathBuf,
    #[arg(
        long = "conv-params",
        short = 'P',
        value_name = "FILE",
        help = "Conversion parameters"
    )]
    pub(crate) conv_params: PathBuf,
    #[arg(
        long = "obj-cache-size",
        value_name = "SIZE",
        help = "size (in MiB) of in-memory git object cache",
        default_value_t = 384
    )]
    pub(crate) git_obj_cache_size: usize,
    #[arg(
        long = "git-repack",
        help = "run \"git repack\" at the end of conversion"
    )]
    pub(crate) git_repack: bool,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub(crate) enum LogLevel {
    #[value(name = "error")]
    Error,
    #[value(name = "warn")]
    Warn,
    #[value(name = "info")]
    Info,
    #[value(name = "debug")]
    Debug,
    #[value(name = "trace")]
    Trace,
}

impl LogLevel {
    pub(crate) fn to_log_level_filter(self) -> tracing::Level {
        match self {
            Self::Error => tracing::Level::ERROR,
            Self::Warn => tracing::Level::WARN,
            Self::Info => tracing::Level::INFO,
            Self::Debug => tracing::Level::DEBUG,
            Self::Trace => tracing::Level::TRACE,
        }
    }
}
