use crate::term_out::ProgressPrint;
use crate::{FHashMap, git};

mod bin_ser_de;
mod git_wrap;
mod meta;
mod options;
mod stage1;
mod stage2;
mod svn_tree;
mod tree_builder;

pub(crate) use options::{InitOptions, Options};

pub(crate) struct ConvertError;

pub(crate) struct GitCommitMeta {
    pub(crate) author: gix_actor::Signature,
    pub(crate) committer: gix_actor::Signature,
    pub(crate) message: String,
}

pub(crate) struct GitTagMeta {
    pub(crate) tagger: Option<gix_actor::Signature>,
    pub(crate) message: String,
}

pub(crate) trait GitMetaMaker {
    fn make_git_commit_meta(
        &self,
        svn_uuid: Option<&uuid::Uuid>,
        svn_rev_no: u32,
        svn_path: Option<&[u8]>,
        svn_rev_props: &FHashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<GitCommitMeta, String>;

    fn make_git_tag_meta(
        &self,
        svn_uuid: Option<&uuid::Uuid>,
        svn_rev_no: u32,
        svn_path: &[u8],
        svn_rev_props: &FHashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<GitTagMeta, String>;
}

pub(crate) fn convert(
    progress_print: &ProgressPrint,
    options: &Options,
    makedata_meta: &dyn GitMetaMaker,
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Result<(), ConvertError> {
    progress_print.set_progress("initializing git import".into());

    let mut git_import = git_wrap::Importer::init(dst_path, options.git_obj_cache_size)?;

    let mut run_stages = || {
        let stage1_out = stage1::run(progress_print, options, src_path, &mut git_import)?;
        stage2::run(
            progress_print,
            options,
            makedata_meta,
            &mut git_import,
            &stage1_out,
        )?;
        Ok(())
    };

    match run_stages() {
        Ok(()) => {}
        Err(ConvertError) => {
            git_import.abort();
            return Err(ConvertError);
        }
    }

    progress_print.set_progress("finalizing git import".into());

    tracing::info!("finalizing git import");
    git_import.finish(|progress| match progress {
        git::ImportFinishProgress::Gather(n, total) => {
            progress_print.set_progress(format!(
                "finalizing git import - gathering objects - {n} / {total}",
            ));
        }
        git::ImportFinishProgress::Sort(total) => {
            progress_print
                .set_progress(format!("finalizing git import - sorting objects ({total})",));
        }
        git::ImportFinishProgress::Write(n, total) => {
            progress_print.set_progress(format!(
                "finalizing git import - writing objects - {n} / {total}",
            ));
        }
        git::ImportFinishProgress::MakeIndex => {
            progress_print.set_progress("finalizing git import - generating pack index".into());
        }
    })?;

    progress_print.set_progress("finalizing".into());
    progress_print.freeze_progress();

    if options.git_repack {
        tracing::info!("running git repack");
        let mut repack_child = std::process::Command::new("git")
            .arg("repack")
            .arg("-a") // repack already-packed objects
            .arg("-d") // delete old packs
            .arg("-f") // compute deltas from scratch
            .current_dir(dst_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| {
                tracing::error!("failed to spawn \"git repack\": {e:?}");
                ConvertError
            })?;
        let repack_exit_code = repack_child.wait().map_err(|e| {
            tracing::error!("failed to wait for \"git repack\": {e:?}");
            ConvertError
        })?;
        if !repack_exit_code.success() {
            tracing::error!("git repack exited with code {repack_exit_code}");
            return Err(ConvertError);
        }
    }

    Ok(())
}
