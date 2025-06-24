use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use super::options::{DirClass, Options};
use super::{git_wrap, meta, ConvertError};
use crate::svn;
use crate::term_out::ProgressPrint;

pub(super) enum Head {
    Branch(usize),
    Unbranched,
}

pub(super) struct Output {
    pub(super) svn_uuid: Option<uuid::Uuid>,
    pub(super) root_rev_data: Vec<RootCommitData>,
    pub(super) unbranched_rev_data: Vec<UnbranchedRevData>,
    pub(super) branch_data: Vec<BranchData>,
    pub(super) branch_rev_data: Vec<BranchRevData>,
    pub(super) head_branch: Head,
}

pub(super) fn run(
    progress_print: &ProgressPrint,
    options: &Options,
    src_path: &std::path::Path,
    git_import: &mut git_wrap::Importer,
) -> Result<Output, ConvertError> {
    tracing::info!("Stage 1: import SVN repository");

    let mut svn_dump_src = match svn::source::DumpSource::open(src_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("failed to open SVN dump source: {e}");
            return Err(ConvertError);
        }
    };

    let svn_dump_reader = match svn::dump::DumpReader::new(svn_dump_src.stream()) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("failed to read SVN dump: {e}");
            return Err(ConvertError);
        }
    };

    let r = Stage {
        progress_print,
        options,
        svn_dump_reader,
        git_import,
        tree_builder: git_wrap::TreeBuilder::new(),
        svn_uuid: None,
        root_rev_data: Vec::new(),
        svn_rev_map: HashMap::new(),
        unbranched_rev_data: Vec::new(),
        branch_data: Vec::new(),
        branch_rev_data: Vec::new(),
        head_branch: None,
        live_branches: HashMap::new(),
        path_to_branch: HashMap::new(),
        branch_path_commits: HashMap::new(),
    }
    .run()?;

    if let Err(e) = svn_dump_src.close() {
        tracing::error!("SVN dump error: {e}");
        return Err(ConvertError);
    }

    Ok(r)
}

#[derive(Clone, Debug)]
struct RootNodeOp {
    path: Vec<u8>,
    action: RootNodeAction,
}

#[derive(Clone, Debug)]
enum RootNodeAction {
    DelFile,
    ModFile(gix_object::tree::EntryMode, gix_hash::ObjectId),
    DelDir(gix_hash::ObjectId),
    AddDir,
    CopyDir(bool, usize, Vec<u8>),
    ModDir(bool),
}

#[derive(Clone, Debug)]
struct BranchOps {
    is_tag: bool,
    delete: bool,
    create: bool,
    create_from: Option<(usize, Vec<u8>)>,
    modify: bool,
    root_meta: bool,
    required_in_mergeinfo: bool,
}

impl BranchOps {
    fn new(is_tag: bool) -> Self {
        Self {
            is_tag,
            delete: false,
            create: false,
            create_from: None,
            modify: false,
            root_meta: false,
            required_in_mergeinfo: false,
        }
    }
}

#[derive(Clone, Debug)]
struct UnbranchedNodeOp {
    path: Vec<u8>,
    action: UnbranchedNodeAction,
}

#[derive(Clone, Debug)]
enum UnbranchedNodeAction {
    DelFile,
    ModFile,
    DelDir,
    AddDir,
    CopyDir(bool, usize, Vec<u8>),
    ModDir(bool),
}

struct Stage<'a> {
    progress_print: &'a ProgressPrint,
    options: &'a Options,
    svn_dump_reader: svn::dump::DumpReader<'a>,
    git_import: &'a mut git_wrap::Importer,
    tree_builder: git_wrap::TreeBuilder,
    svn_uuid: Option<uuid::Uuid>,
    root_rev_data: Vec<RootCommitData>,
    svn_rev_map: HashMap<u32, usize>,
    unbranched_rev_data: Vec<UnbranchedRevData>,
    branch_data: Vec<BranchData>,
    branch_rev_data: Vec<BranchRevData>,
    head_branch: Option<Head>,
    live_branches: HashMap<Vec<u8>, usize>,
    path_to_branch: HashMap<Vec<u8>, Vec<usize>>,
    branch_path_commits: HashMap<Vec<u8>, Vec<(usize, usize)>>,
}

pub(super) struct RootCommitData {
    pub(super) svn_rev: u32,
    pub(super) svn_rev_props: HashMap<Vec<u8>, Vec<u8>>,
    pub(super) meta_tree_oid: gix_hash::ObjectId,
    pub(super) files_tree_oid: gix_hash::ObjectId,
}

pub(super) struct UnbranchedRevData {
    pub(super) root_rev: usize,
    pub(super) tree_oid: gix_hash::ObjectId,
}

pub(super) struct BranchData {
    pub(super) svn_path: Vec<u8>,
    pub(super) is_tag: bool,
    pub(super) deleted: bool,
    pub(super) tip_commit: Option<usize>,
    pub(super) first_root_rev: usize,
    pub(super) last_root_rev: usize,
    /// maps `root_commit <-> branch_commit`
    pub(super) rev_map: Vec<(usize, usize)>,
}

pub(super) struct BranchRevData {
    pub(super) branch: usize,
    pub(super) parent: Option<usize>,
    pub(super) tail: usize,
    pub(super) root_rev: usize,
    pub(super) required_in_mergeinfo: bool,
    pub(super) added_svn_merges: BTreeSet<usize>,
    pub(super) removed_svn_merges: BTreeSet<usize>,
    pub(super) ignore_merges: bool,
    pub(super) fully_reverted_merges_in: BTreeSet<usize>,
    pub(super) tree_oid: gix_hash::ObjectId,
}

#[derive(Debug)]
enum SpecialHandling {
    None,
    Remove,
    CustomReplace,
}

impl Stage<'_> {
    fn run(mut self) -> Result<Output, ConvertError> {
        self.run_inner()?;

        let head_branch = self.head_branch.ok_or_else(|| {
            tracing::error!(
                "head \"{}\" not found",
                self.options.head_path.escape_ascii(),
            );
            ConvertError
        })?;

        if let Head::Branch(head_branch) = head_branch {
            if self.branch_data[head_branch].is_tag {
                tracing::error!(
                    "head \"{}\" is a tag",
                    self.options.head_path.escape_ascii(),
                );
                return Err(ConvertError);
            }

            if !self.options.keep_deleted_branches && self.branch_data[head_branch].deleted {
                tracing::error!(
                    "head \"{}\" has been removed",
                    self.options.head_path.escape_ascii(),
                );
                return Err(ConvertError);
            }
        }

        Ok(Output {
            svn_uuid: self.svn_uuid,
            root_rev_data: self.root_rev_data,
            unbranched_rev_data: self.unbranched_rev_data,
            branch_data: self.branch_data,
            branch_rev_data: self.branch_rev_data,
            head_branch,
        })
    }

    fn parse_svn_path(&self, path: &[u8]) -> Result<Vec<u8>, ConvertError> {
        if path.is_empty() {
            return Ok(vec![]);
        }

        let mut result = Vec::with_capacity(path.len());
        let backslash = false;
        for (i, component) in path
            .split(|&c| c == b'/' || (backslash && c == b'\\'))
            .enumerate()
        {
            if component.is_empty() || component == b".svn" || component == b".git" {
                tracing::error!(
                    "invalid path component name: \"{}\"",
                    component.escape_ascii(),
                );
                return Err(ConvertError);
            }
            if i != 0 {
                result.push(b'/');
            }
            result.extend(component);
        }
        Ok(result)
    }

    fn file_special_handling(&self, path: &[u8]) -> SpecialHandling {
        if self.options.delete_files.is_match(path) {
            SpecialHandling::Remove
        } else {
            let last_component = path
                .iter()
                .rposition(|&c| c == b'/')
                .map_or(path, |i| &path[(i + 1)..]);

            if self.options.generate_gitignore && last_component == b".gitignore" {
                SpecialHandling::CustomReplace
            } else {
                SpecialHandling::None
            }
        }
    }

    fn mod_file_required_in_mergeinfo(&self, path: &[u8]) -> bool {
        !self.options.merge_optional.is_match(path)
    }

    fn get_next_svn_dump_record(&mut self) -> Result<Option<svn::dump::Record>, ConvertError> {
        match self.svn_dump_reader.next_record() {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::error!("failed to read SVN dump record: {e}");
                Err(ConvertError)
            }
        }
    }

    fn run_inner(&mut self) -> Result<(), ConvertError> {
        let mut next_record = self.get_next_svn_dump_record()?;
        while let Some(record) = next_record {
            match record {
                svn::dump::Record::Uuid(uuid) => {
                    if self.svn_uuid.is_some() {
                        tracing::error!("more than one UUID record in SVN dump");
                        return Err(ConvertError);
                    }
                    tracing::info!("SVN repository UUID: {uuid}");
                    self.svn_uuid = Some(uuid);

                    next_record = self.get_next_svn_dump_record()?;
                }
                svn::dump::Record::Rev(rev_record) => {
                    // If the SVN repository is a mirror, pick the UUID of
                    // the original repository, which is present as a property
                    // of revision 0.
                    if let Some(raw_uuid) = rev_record
                        .properties
                        .as_ref()
                        .filter(|_| rev_record.rev_no == 0)
                        .and_then(|props| props.get(b"svn:sync-from-uuid".as_slice()))
                    {
                        let Some(uuid) = std::str::from_utf8(raw_uuid)
                            .ok()
                            .and_then(|raw_uuid| uuid::Uuid::parse_str(raw_uuid).ok())
                        else {
                            tracing::error!(
                                "invalid UUID in svn:sync-from-uuid property: \"{}\"",
                                raw_uuid.escape_ascii()
                            );
                            return Err(ConvertError);
                        };
                        tracing::info!("original SVN repository UUID: {uuid}");
                        self.svn_uuid = Some(uuid);
                    }
                    next_record = self.handle_svn_rev(rev_record)?;
                }
                svn::dump::Record::Node(_) => {
                    tracing::error!("SVN dump has a node record before first revision node");
                    return Err(ConvertError);
                }
            }
        }

        Ok(())
    }

    fn handle_svn_rev(
        &mut self,
        rev_record: svn::dump::RevRecord,
    ) -> Result<Option<svn::dump::Record>, ConvertError> {
        let svn_rev = rev_record.rev_no;

        tracing::debug!("importing SVN revision {svn_rev}");
        self.progress_print
            .set_progress(format!("importing SVN revision {svn_rev}"));
        if self
            .root_rev_data
            .last()
            .is_some_and(|prev| svn_rev <= prev.svn_rev)
        {
            tracing::error!("non monotonic increasing SVN revision numbers");
            return Err(ConvertError);
        }

        let (svn_rev_props, next_record, root_node_ops, root_meta_tree_oid) =
            self.read_svn_rev_meta(rev_record)?;
        let root_files_tree_oid =
            self.make_root_files_tree(svn_rev, &root_node_ops, root_meta_tree_oid)?;

        self.progress_print.set_progress(format!(
            "importing SVN revision {svn_rev} - splitting branches",
        ));
        let (unbranched_ops, branches_ops) = self.split_branches(&root_node_ops)?;

        let root_commit = self.root_rev_data.len();
        self.svn_rev_map.insert(svn_rev, root_commit);
        self.root_rev_data.push(RootCommitData {
            svn_rev,
            svn_rev_props,
            meta_tree_oid: root_meta_tree_oid,
            files_tree_oid: root_files_tree_oid,
        });

        if let Some(unbranched_ops) = unbranched_ops {
            self.make_unbranched_tree(svn_rev, &unbranched_ops)?;
        }

        for (i, (branch_path, branch_ops)) in branches_ops.iter().enumerate() {
            self.progress_print.set_progress(format!(
                "importing SVN revision {svn_rev} - preparing branch {} / {}",
                i + 1,
                branches_ops.len(),
            ));
            assert_ne!(branch_path, b"");
            self.make_branch_rev_data(branch_path, branch_ops)?;
        }

        Ok(next_record)
    }

    fn read_svn_rev_meta(
        &mut self,
        rev_record: svn::dump::RevRecord,
    ) -> Result<
        (
            HashMap<Vec<u8>, Vec<u8>>,
            Option<svn::dump::Record>,
            Vec<RootNodeOp>,
            gix_hash::ObjectId,
        ),
        ConvertError,
    > {
        if let Some(prev_rev_data) = self.root_rev_data.last() {
            self.tree_builder.reset(prev_rev_data.meta_tree_oid);
        } else {
            self.tree_builder.clear();
        }

        let svn_rev = rev_record.rev_no;
        assert!(!self.svn_rev_map.contains_key(&svn_rev));

        let meta = rev_record.properties.unwrap_or_default();
        let mut node_ops = Vec::new();

        if self.root_rev_data.is_empty() {
            let raw_empty_meta = meta::DirMetadata::default().serialize();
            self.tree_builder.mod_inline(
                b".svn",
                gix_object::tree::EntryKind::Blob.into(),
                raw_empty_meta,
                None,
                self.git_import,
            )?;
        }

        let mut next_record = None;
        let mut node_no = 0usize;
        while let Some(record) = self.get_next_svn_dump_record()? {
            let svn::dump::Record::Node(mut node_record) = record else {
                next_record = Some(record);
                break;
            };

            node_no += 1;
            self.progress_print.set_progress(format!(
                "importing SVN revision {svn_rev} - root meta - node {node_no}",
            ));

            let node_path = self.parse_svn_path(&node_record.path)?;
            let node_action = node_record.action;
            let node_kind = node_record.kind;
            let mut copy_from = node_record
                .copy_from
                .as_ref()
                .map(|copy_from| {
                    let rev = *self.svn_rev_map.get(&copy_from.rev).ok_or_else(|| {
                        tracing::error!(
                            "attempted to copy from non-existent SVN rev {}",
                            copy_from.rev
                        );
                        ConvertError
                    })?;
                    let path = self.parse_svn_path(&copy_from.path)?;
                    Ok((rev, path))
                })
                .transpose()?;

            tracing::trace!(
                "SVN dump node record: path=\"{}\", kind={node_kind:?}, action={node_action:?}",
                node_path.escape_ascii(),
            );

            let mut props = node_record.properties.as_ref();

            if node_action == svn::dump::NodeAction::Replace {
                let (prev_mode, prev_hash) = self
                    .tree_builder
                    .ls(&node_path, self.git_import)?
                    .ok_or_else(|| {
                        tracing::error!(
                            "attempted to replace non-existent path \"{}\"",
                            node_path.escape_ascii(),
                        );
                        ConvertError
                    })?;

                self.tree_builder.rm(&node_path, self.git_import)?;
                node_ops.push(RootNodeOp {
                    path: node_path.clone(),
                    action: if prev_mode.is_tree() {
                        RootNodeAction::DelDir(prev_hash)
                    } else {
                        RootNodeAction::DelFile
                    },
                });
            }

            match node_action {
                svn::dump::NodeAction::Delete => {
                    let (prev_mode, prev_hash) = self
                        .tree_builder
                        .ls(&node_path, self.git_import)?
                        .ok_or_else(|| {
                            tracing::error!(
                                "attempted to delete non-existent path \"{}\"",
                                node_path.escape_ascii(),
                            );
                            ConvertError
                        })?;

                    self.tree_builder.rm(&node_path, self.git_import)?;
                    node_ops.push(RootNodeOp {
                        path: node_path.clone(),
                        action: if prev_mode.is_tree() {
                            RootNodeAction::DelDir(prev_hash)
                        } else {
                            RootNodeAction::DelFile
                        },
                    });
                }
                svn::dump::NodeAction::Change
                | svn::dump::NodeAction::Add
                | svn::dump::NodeAction::Replace => match node_kind {
                    None => {
                        tracing::error!("missing Node-kind in SVN dump node record");
                        return Err(ConvertError);
                    }
                    Some(svn::dump::NodeKind::File) => {
                        let mut orig_mode = None;
                        let mut orig_blob = None;

                        if let Some((copy_from_rev, copy_from_path)) = copy_from.take() {
                            if node_action == svn::dump::NodeAction::Change {
                                tracing::error!("unexpected copy-from in change node");
                                return Err(ConvertError);
                            }

                            let (mode, blob) = self
                                .git_import
                                .ls(
                                    self.root_rev_data[copy_from_rev].meta_tree_oid,
                                    &copy_from_path,
                                )?
                                .ok_or_else(|| {
                                    tracing::error!(
                                        "attempted to copy from non-existent path \"{}\" at rev {}",
                                        copy_from_path.escape_ascii(),
                                        self.root_rev_data[copy_from_rev].svn_rev,
                                    );
                                    ConvertError
                                })?;
                            orig_mode = Some(mode);
                            orig_blob = Some(blob);
                        } else if node_action == svn::dump::NodeAction::Change {
                            let (mode, blob) = self
                                .tree_builder
                                .ls(&node_path, self.git_import)?
                                .ok_or_else(|| {
                                tracing::error!(
                                    "attempted to change non-existent path \"{}\"",
                                    node_path.escape_ascii(),
                                );
                                ConvertError
                            })?;
                            orig_mode = Some(mode);
                            orig_blob = Some(blob);
                        }

                        let mut props_mode = None::<gix_object::tree::EntryMode>;
                        if let Some(props) = props.take() {
                            let special_prop = props.properties.get(b"svn:special".as_slice());
                            let executable_prop =
                                props.properties.get(b"svn:executable".as_slice());
                            match (special_prop, executable_prop) {
                                (Some(Some(_)), _) => {
                                    // "svn:special" present, it is a symlink regardless
                                    // of what happens with "svn:executable"
                                    props_mode = Some(gix_object::tree::EntryKind::Link.into());
                                }
                                (Some(None), _) => {
                                    // "svn:special" removed, which is not allowed
                                    tracing::error!("unexpected change of symlink/non-symlink");
                                    return Err(ConvertError);
                                }
                                (None, Some(Some(_))) => {
                                    // "svn:executable" added
                                    // In delta mode, "svn:special" might be present
                                    if !props.is_delta || !orig_mode.is_some_and(|m| m.is_link()) {
                                        props_mode = Some(
                                            gix_object::tree::EntryKind::BlobExecutable.into(),
                                        );
                                    }
                                }
                                (None, Some(None)) => {
                                    // "svn:executable" removed
                                    // In delta mode, "svn:special" might be present
                                    if props.is_delta && orig_mode.is_some_and(|m| m.is_link()) {
                                        // "svn:special" is present
                                        // keep `orig_mode`
                                    } else {
                                        // "svn:special" not present
                                        props_mode = Some(gix_object::tree::EntryKind::Blob.into());
                                    }
                                }
                                (None, None) => {
                                    if props.is_delta {
                                        // neither "svn:special" nor "svn:executable" are changed
                                        // keep `orig_mode`
                                    } else {
                                        // neither "svn:special" nor "svn:executable" present
                                        props_mode = Some(gix_object::tree::EntryKind::Blob.into());
                                    }
                                }
                            }
                        }

                        let new_mode = props_mode
                            .or(orig_mode)
                            .unwrap_or(gix_object::tree::EntryKind::Blob.into());
                        if let Some(orig_mode) = orig_mode {
                            if new_mode.is_link() != orig_mode.is_link() {
                                tracing::error!("unexpected change of symlink/non-symlink");
                                return Err(ConvertError);
                            }
                        }

                        let file_blob_oid;
                        if let Some(node_text) = node_record.text.take() {
                            if node_text.is_delta {
                                let source = if let Some(orig_blob) = orig_blob {
                                    let mut source = self.git_import.get_blob(orig_blob)?;
                                    if new_mode.is_link() {
                                        source.splice(0..0, b"link ".iter().copied());
                                    }
                                    source
                                } else {
                                    Vec::new()
                                };

                                let delta_len =
                                    usize::try_from(self.svn_dump_reader.remaining_text_len())
                                        .unwrap();
                                let mut delta = vec![0; delta_len];
                                self.svn_dump_reader.read_text(&mut delta).map_err(|e| {
                                    tracing::error!("failed to read SVN node text: {e}");
                                    ConvertError
                                })?;

                                let mut result_data = Vec::new();
                                if let Err(e) = svn::diff::apply(
                                    delta.as_slice(),
                                    source.as_slice(),
                                    &mut result_data,
                                ) {
                                    tracing::error!("failed to apply SVN delta: {e}");
                                    return Err(ConvertError);
                                };

                                let mut blob_data = result_data;
                                if new_mode.is_link() {
                                    if blob_data.starts_with(b"link ") {
                                        blob_data.splice(0..5, []);
                                    } else {
                                        tracing::error!(
                                            "invalid symlink at \"{}\" in SVN dump",
                                            node_path.escape_ascii(),
                                        );
                                        return Err(ConvertError);
                                    }
                                }

                                file_blob_oid = self.tree_builder.mod_inline(
                                    &node_path,
                                    new_mode,
                                    blob_data,
                                    orig_blob,
                                    self.git_import,
                                )?;
                            } else {
                                if new_mode.is_link() {
                                    // strip the "link " prefix in symlinks
                                    if self.svn_dump_reader.remaining_text_len() < 5 {
                                        tracing::error!(
                                            "invalid symlink at \"{}\" in SVN dump",
                                            node_path.escape_ascii(),
                                        );
                                        return Err(ConvertError);
                                    }
                                    let mut link_prefix = [0; 5];
                                    self.svn_dump_reader.read_text(&mut link_prefix).map_err(
                                        |e| {
                                            tracing::error!("failed to read SVN node text: {e}");
                                            ConvertError
                                        },
                                    )?;
                                    if link_prefix != *b"link " {
                                        tracing::error!(
                                            "invalid symlink at \"{}\" in SVN dump",
                                            node_path.escape_ascii(),
                                        );
                                        return Err(ConvertError);
                                    }
                                }

                                let data_len =
                                    usize::try_from(self.svn_dump_reader.remaining_text_len())
                                        .unwrap();
                                let mut blob_data = vec![0; data_len];
                                self.svn_dump_reader
                                    .read_text(&mut blob_data)
                                    .map_err(|e| {
                                        tracing::error!("failed to read SVN node text: {e}");
                                        ConvertError
                                    })?;

                                file_blob_oid = self.tree_builder.mod_inline(
                                    &node_path,
                                    new_mode,
                                    blob_data,
                                    orig_blob,
                                    self.git_import,
                                )?;
                            }
                        } else if let Some(blob) = orig_blob {
                            self.tree_builder.mod_oid(
                                &node_path,
                                new_mode,
                                blob,
                                self.git_import,
                            )?;
                            file_blob_oid = blob;
                        } else {
                            tracing::error!("missing file content in SVN dump node");
                            return Err(ConvertError);
                        }

                        node_ops.push(RootNodeOp {
                            path: node_path.clone(),
                            action: RootNodeAction::ModFile(new_mode, file_blob_oid),
                        });
                    }
                    Some(svn::dump::NodeKind::Dir) => {
                        let is_empty_new =
                            node_action != svn::dump::NodeAction::Change && copy_from.is_none();
                        let mut prev_meta_blob_oid = None;

                        let props = props.take();

                        let mut raw_meta = None;
                        if let Some(props) = props {
                            if node_action == svn::dump::NodeAction::Change {
                                let meta_path = concat_path(&node_path, b".svn");
                                let (_, meta_blob_oid) = self
                                    .tree_builder
                                    .ls(&meta_path, self.git_import)?
                                    .ok_or_else(|| {
                                        tracing::error!(
                                            "missing directory metadata for \"{}\"",
                                            node_path.escape_ascii(),
                                        );
                                        ConvertError
                                    })?;
                                prev_meta_blob_oid = Some(meta_blob_oid);
                            } else if let Some((copy_from_rev, ref copy_from_path)) = copy_from {
                                let meta_path = concat_path(copy_from_path, b".svn");
                                let (_, meta_blob_oid) = self
                                    .git_import
                                    .ls(
                                        self.root_rev_data[copy_from_rev].meta_tree_oid,
                                        &meta_path,
                                    )?
                                    .ok_or_else(|| {
                                        tracing::error!(
                                            "missing directory metadata for copy-from directory \"{}\" at rev {}",
                                            copy_from_path.escape_ascii(),
                                            self.root_rev_data[copy_from_rev].svn_rev,
                                        );
                                        ConvertError
                                    })?;
                                prev_meta_blob_oid = Some(meta_blob_oid);
                            }

                            let mut prev_meta = None;
                            if let Some(prev_meta_blob_oid) =
                                prev_meta_blob_oid.filter(|_| props.is_delta)
                            {
                                let raw_prev_meta = self.git_import.get_blob(prev_meta_blob_oid)?;
                                prev_meta = Some(
                                    meta::DirMetadata::deserialize(&raw_prev_meta).ok_or_else(
                                        || {
                                            tracing::error!(
                                                "failed to deserialize directory metadata"
                                            );
                                            ConvertError
                                        },
                                    )?,
                                );
                            }

                            let meta = meta::DirMetadata::from_props(&props.properties, prev_meta);
                            raw_meta = Some(meta.serialize());
                        } else if is_empty_new {
                            raw_meta = Some(meta::DirMetadata::default().serialize());
                        } else {
                            // preserve metadata from existing or copied directory
                            // (keep `raw_meta` as `None`)
                        }

                        if node_action == svn::dump::NodeAction::Change {
                            node_ops.push(RootNodeOp {
                                path: node_path.clone(),
                                action: RootNodeAction::ModDir(raw_meta.is_some()),
                            });
                        } else if let Some((copy_from_rev, copy_from_path)) = copy_from.take() {
                            let (copy_from_mode, copy_from_oid) = self
                                .git_import
                                .ls(
                                    self.root_rev_data[copy_from_rev].meta_tree_oid,
                                    &copy_from_path,
                                )?
                                .ok_or_else(|| {
                                    tracing::error!(
                                        "attempted to copy from non-existent path \"{}\" at rev {}",
                                        copy_from_path.escape_ascii(),
                                        self.root_rev_data[copy_from_rev].svn_rev,
                                    );
                                    ConvertError
                                })?;

                            if !copy_from_mode.is_tree() {
                                tracing::error!(
                                    "\"{}\" at rev {} is expected to be a directory",
                                    copy_from_path.escape_ascii(),
                                    self.root_rev_data[copy_from_rev].svn_rev,
                                );
                                return Err(ConvertError);
                            }
                            self.tree_builder.mod_oid(
                                &node_path,
                                copy_from_mode,
                                copy_from_oid,
                                self.git_import,
                            )?;

                            node_ops.push(RootNodeOp {
                                path: node_path.clone(),
                                action: RootNodeAction::CopyDir(
                                    raw_meta.is_some(),
                                    copy_from_rev,
                                    copy_from_path,
                                ),
                            });
                        } else {
                            node_ops.push(RootNodeOp {
                                path: node_path.clone(),
                                action: RootNodeAction::AddDir,
                            });
                        }

                        if let Some(raw_meta) = raw_meta {
                            let meta_path = concat_path(&node_path, b".svn");
                            self.tree_builder.mod_inline(
                                &meta_path,
                                gix_object::tree::EntryKind::Blob.into(),
                                raw_meta,
                                prev_meta_blob_oid,
                                self.git_import,
                            )?;
                        }
                    }
                },
            }

            if node_record.text.is_some() {
                tracing::error!("SVN dump node record has unused text content");
                return Err(ConvertError);
            }
            if copy_from.is_some() {
                tracing::error!("SVN dump node record has unused copy-from");
                return Err(ConvertError);
            }
            if props.is_some() {
                tracing::error!("SVN dump node record has unused props content");
                return Err(ConvertError);
            }
        }

        let meta_tree_oid = self.tree_builder.materialize(self.git_import)?;

        Ok((meta, next_record, node_ops, meta_tree_oid))
    }

    fn make_root_files_tree(
        &mut self,
        svn_rev: u32,
        node_ops: &[RootNodeOp],
        meta_tree_oid: gix_hash::ObjectId,
    ) -> Result<gix_hash::ObjectId, ConvertError> {
        if let Some(prev_rev_data) = self.root_rev_data.last() {
            self.tree_builder.reset(prev_rev_data.files_tree_oid);
        } else {
            self.tree_builder.clear();
        }

        for (node_no, node_op) in node_ops.iter().enumerate() {
            self.progress_print.set_progress(format!(
                "importing SVN revision {svn_rev} - root files - node {} / {}",
                node_no + 1,
                node_ops.len(),
            ));

            let mut update_dir_metadata = false;
            match node_op.action {
                RootNodeAction::DelFile => {
                    let mut do_delete = true;
                    match self.file_special_handling(&node_op.path) {
                        SpecialHandling::None => {}
                        SpecialHandling::Remove | SpecialHandling::CustomReplace => {
                            do_delete = false;
                        }
                    }
                    if do_delete {
                        self.tree_builder.rm(&node_op.path, self.git_import)?;
                    }
                }
                RootNodeAction::ModFile(mode, blob) => match self
                    .file_special_handling(&node_op.path)
                {
                    SpecialHandling::None => {
                        self.tree_builder
                            .mod_oid(&node_op.path, mode, blob, self.git_import)?;
                    }
                    SpecialHandling::Remove => {}
                    SpecialHandling::CustomReplace => {}
                },
                RootNodeAction::DelDir(_) => {
                    self.tree_builder.rm(&node_op.path, self.git_import)?;
                }
                RootNodeAction::AddDir => {
                    update_dir_metadata = true;
                }
                RootNodeAction::CopyDir(has_meta, copy_from_rev, ref copy_from_path) => {
                    if let Some((copy_from_mode, copy_from_oid)) = self.git_import.ls(
                        self.root_rev_data[copy_from_rev].files_tree_oid,
                        copy_from_path,
                    )? {
                        if !copy_from_mode.is_tree() {
                            tracing::error!(
                                "\"{}\" from rev {} is expected to be a directory",
                                copy_from_path.escape_ascii(),
                                self.root_rev_data[copy_from_rev].svn_rev,
                            );
                            return Err(ConvertError);
                        }
                        self.tree_builder.mod_oid(
                            &node_op.path,
                            copy_from_mode,
                            copy_from_oid,
                            self.git_import,
                        )?;
                    }

                    update_dir_metadata = has_meta;
                }
                RootNodeAction::ModDir(has_meta) => {
                    update_dir_metadata = has_meta;
                }
            }

            if update_dir_metadata {
                let meta = self.get_dir_meta(meta_tree_oid, &node_op.path)?;

                if self.options.generate_gitignore {
                    let mut gitignore_data = Vec::<u8>::new();

                    let from_svnignore = meta::svnignore_to_gitignore(&meta.ignores, false);
                    if !from_svnignore.is_empty() {
                        gitignore_data.extend(b"# ignores from svn:ignore\n");
                        gitignore_data.extend(from_svnignore);
                    }

                    let from_svnglobalignore =
                        meta::svnignore_to_gitignore(&meta.global_ignores, true);
                    if !from_svnglobalignore.is_empty() {
                        if !gitignore_data.is_empty() {
                            gitignore_data.push(b'\n');
                        }
                        gitignore_data.extend(b"# ignores from svn:global-ignores\n");
                        gitignore_data.extend(from_svnglobalignore);
                    }

                    let gitignore_path = concat_path(&node_op.path, b".gitignore");
                    if gitignore_data.is_empty() {
                        self.tree_builder.rm(&gitignore_path, self.git_import)?;
                    } else {
                        self.tree_builder.mod_inline(
                            &gitignore_path,
                            gix_object::tree::EntryKind::Blob.into(),
                            gitignore_data,
                            None,
                            self.git_import,
                        )?;
                    }
                }
            }
        }

        let files_tree_oid = self.tree_builder.materialize(self.git_import)?;

        Ok(files_tree_oid)
    }

    fn split_branches(
        &mut self,
        node_ops: &[RootNodeOp],
    ) -> Result<(Option<Vec<UnbranchedNodeOp>>, BTreeMap<Vec<u8>, BranchOps>), ConvertError> {
        let mut pending: VecDeque<_> = node_ops.iter().cloned().collect();

        let mut branches_ops = BTreeMap::<Vec<u8>, BranchOps>::new();
        let mut unbranched_ops = None::<Vec<UnbranchedNodeOp>>;

        while let Some(node_op) = pending.pop_front() {
            match node_op.action {
                RootNodeAction::DelFile => {
                    let dir_path = get_path_base_dir(&node_op.path);
                    match self.options.classify_dir(dir_path) {
                        DirClass::Unbranched | DirClass::BranchParent => {
                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::DelFile,
                                });
                        }
                        DirClass::Branch(branch_path, is_tag, _) => {
                            let branch_ops = branches_ops
                                .entry(branch_path.to_vec())
                                .or_insert_with(|| BranchOps::new(is_tag));
                            branch_ops.modify = true;
                            branch_ops.required_in_mergeinfo = true;
                        }
                    }
                }
                RootNodeAction::ModFile(_, _) => {
                    let dir_path = get_path_base_dir(&node_op.path);
                    match self.options.classify_dir(dir_path) {
                        DirClass::Unbranched | DirClass::BranchParent => {
                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::ModFile,
                                });
                        }
                        DirClass::Branch(branch_path, is_tag, _) => {
                            let branch_ops = branches_ops
                                .entry(branch_path.to_vec())
                                .or_insert_with(|| BranchOps::new(is_tag));
                            branch_ops.modify = true;
                            if !branch_ops.required_in_mergeinfo
                                && self.mod_file_required_in_mergeinfo(&node_op.path)
                            {
                                branch_ops.required_in_mergeinfo = true;
                            }
                        }
                    }
                }
                RootNodeAction::DelDir(tree_oid) => {
                    match self.options.classify_dir(&node_op.path) {
                        DirClass::Unbranched => {
                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::DelDir,
                                });
                        }
                        DirClass::Branch(branch_path, is_tag, subdir) => {
                            let branch_ops = branches_ops
                                .entry(branch_path.to_vec())
                                .or_insert_with(|| BranchOps::new(is_tag));
                            if subdir == b"" {
                                branch_ops.delete = true;
                            } else {
                                branch_ops.modify = true;
                                branch_ops.required_in_mergeinfo = true;
                            }
                        }
                        DirClass::BranchParent => {
                            let dir_tree = self.git_import.get::<gix_object::Tree>(tree_oid)?;
                            for dir_entry in dir_tree.entries.iter() {
                                if dir_entry.filename.as_slice() == b".svn" {
                                    continue;
                                }

                                let item_path = concat_path(&node_op.path, &dir_entry.filename);
                                let (item_mode, item_hash) = self
                                    .git_import
                                    .ls(
                                        self.root_rev_data.last().unwrap().meta_tree_oid,
                                        &item_path,
                                    )?
                                    .ok_or_else(|| {
                                        tracing::error!(
                                            "missing path \"{}\" in meta tree",
                                            item_path.escape_ascii(),
                                        );
                                        ConvertError
                                    })?;
                                pending.push_front(RootNodeOp {
                                    path: item_path,
                                    action: if item_mode.is_tree() {
                                        RootNodeAction::DelDir(item_hash)
                                    } else {
                                        RootNodeAction::DelFile
                                    },
                                });
                            }

                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::DelDir,
                                });
                        }
                    }
                }
                RootNodeAction::AddDir => match self.options.classify_dir(&node_op.path) {
                    DirClass::Unbranched | DirClass::BranchParent => {
                        unbranched_ops
                            .get_or_insert_with(Vec::new)
                            .push(UnbranchedNodeOp {
                                path: node_op.path,
                                action: UnbranchedNodeAction::AddDir,
                            });
                    }
                    DirClass::Branch(branch_path, is_tag, subdir) => {
                        let branch_ops = branches_ops
                            .entry(branch_path.to_vec())
                            .or_insert_with(|| BranchOps::new(is_tag));
                        if subdir == b"" {
                            branch_ops.create = true;
                            branch_ops.root_meta = true;
                        } else {
                            branch_ops.modify = true;
                        }
                        branch_ops.required_in_mergeinfo = true;
                    }
                },
                RootNodeAction::CopyDir(has_meta, copy_from_rev, copy_from_path) => {
                    match self.options.classify_dir(&node_op.path) {
                        DirClass::Unbranched => {
                            match self.options.classify_dir(&copy_from_path) {
                                DirClass::Branch(copy_from_branch, _, b"") => {
                                    tracing::warn!(
                                        "copying branch \"{}\" to non-branch/tag \"{}\"",
                                        copy_from_branch.escape_ascii(),
                                        node_op.path.escape_ascii(),
                                    );
                                }
                                DirClass::BranchParent => {
                                    tracing::warn!(
                                        "copying branch/tag-container \"{}\" to non-branch/tag-container \"{}\"",
                                        copy_from_path.escape_ascii(),
                                        node_op.path.escape_ascii(),
                                    );
                                }
                                _ => {}
                            }

                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::CopyDir(
                                        has_meta,
                                        copy_from_rev,
                                        copy_from_path,
                                    ),
                                });
                        }
                        DirClass::Branch(branch_path, is_tag, subdir) => {
                            let branch_ops = branches_ops
                                .entry(branch_path.to_vec())
                                .or_insert_with(|| BranchOps::new(is_tag));
                            if subdir == b"" {
                                branch_ops.create = true;
                                branch_ops.create_from = Some((copy_from_rev, copy_from_path));
                                branch_ops.root_meta = true;
                                branch_ops.required_in_mergeinfo |= has_meta;
                            } else {
                                branch_ops.modify = true;
                                branch_ops.required_in_mergeinfo = true;
                            }
                        }
                        DirClass::BranchParent => {
                            if let DirClass::Branch(copy_from_branch, _, b"") =
                                self.options.classify_dir(&copy_from_path)
                            {
                                tracing::warn!(
                                    "copying branch \"{}\" to non-branch \"{}\"",
                                    copy_from_branch.escape_ascii(),
                                    node_op.path.escape_ascii(),
                                );
                            }

                            let (_, copy_from_tree_oid) = self
                                .git_import
                                .ls(
                                    self.root_rev_data[copy_from_rev].meta_tree_oid,
                                    &copy_from_path,
                                )?
                                .ok_or_else(|| {
                                    tracing::error!(
                                        "missing path \"{}\" at rev {}",
                                        copy_from_path.escape_ascii(),
                                        self.root_rev_data[copy_from_rev].svn_rev,
                                    );
                                    ConvertError
                                })?;

                            let dir_tree = self
                                .git_import
                                .get::<gix_object::Tree>(copy_from_tree_oid)?;
                            for dir_entry in dir_tree.entries.iter() {
                                if dir_entry.filename.as_slice() == b".svn" {
                                    continue;
                                }

                                let item_src_path =
                                    concat_path(&copy_from_path, &dir_entry.filename);
                                let item_dst_path = concat_path(&node_op.path, &dir_entry.filename);

                                if dir_entry.mode.is_tree() {
                                    pending.push_front(RootNodeOp {
                                        path: item_dst_path,
                                        action: RootNodeAction::CopyDir(
                                            has_meta,
                                            copy_from_rev,
                                            item_src_path,
                                        ),
                                    });
                                } else {
                                    pending.push_front(RootNodeOp {
                                        path: item_dst_path,
                                        action: RootNodeAction::ModFile(
                                            dir_entry.mode,
                                            dir_entry.oid,
                                        ),
                                    });
                                }
                            }

                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::AddDir,
                                });
                        }
                    }
                }
                RootNodeAction::ModDir(has_meta) => {
                    match self.options.classify_dir(&node_op.path) {
                        DirClass::Unbranched => {
                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::ModDir(has_meta),
                                });
                        }
                        DirClass::Branch(branch_path, is_tag, subdir) => {
                            let branch_ops = branches_ops
                                .entry(branch_path.to_vec())
                                .or_insert_with(|| BranchOps::new(is_tag));
                            if subdir == b"" {
                                branch_ops.root_meta |= has_meta;
                            } else {
                                branch_ops.modify = true;
                            }
                            // a directory change without metadata changes is a no-op.
                            branch_ops.required_in_mergeinfo |= has_meta;
                        }
                        DirClass::BranchParent => {
                            unbranched_ops
                                .get_or_insert_with(Vec::new)
                                .push(UnbranchedNodeOp {
                                    path: node_op.path,
                                    action: UnbranchedNodeAction::ModDir(has_meta),
                                });
                        }
                    }
                }
            }
        }

        Ok((unbranched_ops, branches_ops))
    }

    fn make_unbranched_tree(
        &mut self,
        svn_rev: u32,
        ops: &[UnbranchedNodeOp],
    ) -> Result<(), ConvertError> {
        if let Some(prev_rev_data) = self.unbranched_rev_data.last() {
            self.tree_builder.reset(prev_rev_data.tree_oid);
        } else {
            self.tree_builder.clear();
        }

        let root_rev = self.root_rev_data.len() - 1;

        for (op_no, op) in ops.iter().enumerate() {
            self.progress_print.set_progress(format!(
                "importing SVN revision {svn_rev} - unbranched - node {} / {}",
                op_no + 1,
                ops.len(),
            ));

            let mut update_dir_metadata = false;
            match op.action {
                UnbranchedNodeAction::DelFile => {
                    let mut do_delete = true;
                    match self.file_special_handling(&op.path) {
                        SpecialHandling::None => {}
                        SpecialHandling::Remove | SpecialHandling::CustomReplace => {
                            do_delete = false;
                        }
                    }
                    if do_delete {
                        self.tree_builder.rm(&op.path, self.git_import)?;
                    }
                }
                UnbranchedNodeAction::ModFile => match self.file_special_handling(&op.path) {
                    SpecialHandling::None => {
                        let (mode, blob) = self
                            .git_import
                            .ls(self.root_rev_data[root_rev].meta_tree_oid, &op.path)?
                            .ok_or_else(|| {
                                tracing::error!(
                                    "missing path \"{}\" in meta tree",
                                    op.path.escape_ascii(),
                                );
                                ConvertError
                            })?;
                        self.tree_builder
                            .mod_oid(&op.path, mode, blob, self.git_import)?;
                    }
                    SpecialHandling::Remove => {}
                    SpecialHandling::CustomReplace => {}
                },
                UnbranchedNodeAction::DelDir => {
                    self.tree_builder.rm(&op.path, self.git_import)?;
                }
                UnbranchedNodeAction::AddDir => {
                    update_dir_metadata = true;
                }
                UnbranchedNodeAction::CopyDir(has_meta, copy_from_rev, ref copy_from_path) => {
                    if let Some((copy_from_mode, copy_from_oid)) = self.git_import.ls(
                        self.root_rev_data[copy_from_rev].files_tree_oid,
                        copy_from_path,
                    )? {
                        if !copy_from_mode.is_tree() {
                            tracing::error!(
                                "\"{}\" at rev {} is expected to be a directory",
                                copy_from_path.escape_ascii(),
                                self.root_rev_data[copy_from_rev].svn_rev,
                            );
                            return Err(ConvertError);
                        }
                        self.tree_builder.mod_oid(
                            &op.path,
                            copy_from_mode,
                            copy_from_oid,
                            self.git_import,
                        )?;
                    }

                    update_dir_metadata = has_meta;
                }
                UnbranchedNodeAction::ModDir(has_meta) => {
                    update_dir_metadata = has_meta;
                }
            }

            if update_dir_metadata && self.options.generate_gitignore {
                let gitignore_path = concat_path(&op.path, b".gitignore");
                if let Some((mode, blob)) = self
                    .git_import
                    .ls(self.root_rev_data[root_rev].files_tree_oid, &gitignore_path)?
                {
                    self.tree_builder
                        .mod_oid(&gitignore_path, mode, blob, self.git_import)?;
                } else {
                    self.tree_builder.rm(&gitignore_path, self.git_import)?;
                }
            }
        }

        if self.options.head_path.is_empty() {
            self.head_branch = Some(Head::Unbranched);
        }

        let tree_oid = self.tree_builder.materialize(self.git_import)?;
        self.unbranched_rev_data
            .push(UnbranchedRevData { root_rev, tree_oid });

        tracing::debug!("committed on unbranched branch");

        Ok(())
    }

    fn make_branch_rev_data(
        &mut self,
        branch_path: &[u8],
        branch_ops: &BranchOps,
    ) -> Result<(), ConvertError> {
        let root_commit = self.root_rev_data.len() - 1;

        if branch_ops.delete {
            if branch_ops.create {
                tracing::warn!(
                    "branch/tag \"{}\" is deleted and re-created in the same commit",
                    branch_path.escape_ascii(),
                );
            }

            tracing::debug!("deleting branch/tag \"{}\"", branch_path.escape_ascii());
            let branch = self.live_branches.remove(branch_path).unwrap();
            self.branch_data[branch].deleted = true;
        }

        let mut branch = None;
        if branch_ops.create {
            if self.live_branches.contains_key(branch_path) {
                tracing::error!(
                    "branch/tag \"{}\" already exists",
                    branch_path.escape_ascii(),
                );
                return Err(ConvertError);
            } else {
                let mut is_tag = branch_ops.is_tag;
                let mut tip_commit = None;
                if let Some((from_rev, ref from_path)) = branch_ops.create_from {
                    if from_path != b""
                        && matches!(
                            self.options.classify_dir(from_path),
                            DirClass::Branch(_, _, b"")
                        )
                    {
                        tracing::debug!(
                            "creating branch/tag \"{}\" from \"{}\"",
                            branch_path.escape_ascii(),
                            from_path.escape_ascii(),
                        );
                        let branch_path_commits = &self.branch_path_commits[from_path];
                        let branch_path_commit = match branch_path_commits
                            .binary_search_by_key(&from_rev, |&(c, _)| c)
                        {
                            Ok(i) => &branch_path_commits[i],
                            Err(i) => &branch_path_commits[i - 1],
                        };
                        tip_commit = Some(branch_path_commit.1);
                    } else if is_tag {
                        tracing::warn!(
                            "creating tag \"{}\" from non-branch/tag \"{}\"",
                            branch_path.escape_ascii(),
                            from_path.escape_ascii(),
                        );
                        is_tag = false;
                    } else {
                        tracing::warn!(
                            "creating branch \"{}\" from non-branch/tag \"{}\"",
                            branch_path.escape_ascii(),
                            from_path.escape_ascii(),
                        );
                    }
                } else if is_tag {
                    tracing::warn!(
                        "creating tag \"{}\" with new directory",
                        branch_path.escape_ascii(),
                    );
                    is_tag = false;
                } else {
                    tracing::debug!(
                        "creating branch \"{}\" with new directory",
                        branch_path.escape_ascii(),
                    );
                }

                let new_branch = self.branch_data.len();

                self.branch_data.push(BranchData {
                    svn_path: branch_path.to_vec(),
                    is_tag,
                    deleted: false,
                    tip_commit,
                    first_root_rev: root_commit,
                    last_root_rev: root_commit,
                    rev_map: Vec::new(),
                });
                self.live_branches.insert(branch_path.to_vec(), new_branch);
                self.path_to_branch
                    .entry(branch_path.to_vec())
                    .or_default()
                    .push(new_branch);

                branch = Some(new_branch);
            }
        } else if !branch_ops.delete {
            tracing::debug!(
                "modification on branch/tag \"{}\"",
                branch_path.escape_ascii(),
            );
            branch = Some(self.live_branches[branch_path]);
        }

        if let Some(branch) = branch {
            let parent_commit = self.branch_data[branch].tip_commit;

            let branch_rev = self.branch_rev_data.len();

            let (added_svn_merges, removed_svn_merges) = if !self.options.enable_merges {
                (BTreeSet::new(), BTreeSet::new())
            } else if let Some(parent_commit) = parent_commit {
                if branch_ops.root_meta {
                    self.gather_svn_merges(branch, branch_rev, parent_commit)?
                } else {
                    (BTreeSet::new(), BTreeSet::new())
                }
            } else {
                (BTreeSet::new(), BTreeSet::new())
            };

            let ignore_merges = self
                .options
                .ignore_merges_at
                .get(&self.root_rev_data[root_commit].svn_rev)
                .is_some_and(|ign| ign.contains(branch_path));

            let branch_data = &mut self.branch_data[branch];
            if branch_data.is_tag {
                if !branch_ops.create {
                    tracing::warn!(
                        "tag \"{}\" has more than one commit",
                        branch_path.escape_ascii(),
                    );
                    branch_data.is_tag = false;
                } else if branch_ops.modify {
                    tracing::warn!(
                        "tag \"{}\" is created with modifications",
                        branch_path.escape_ascii(),
                    );
                    branch_data.is_tag = false;
                }
            }

            branch_data.tip_commit = Some(branch_rev);
            branch_data.last_root_rev = root_commit;
            branch_data.rev_map.push((root_commit, branch_rev));

            let tail_commit = parent_commit.map_or(branch_rev, |p| self.branch_rev_data[p].tail);
            let tree_oid = if let Some((mode, tree_oid)) = self
                .git_import
                .ls(self.root_rev_data[root_commit].files_tree_oid, branch_path)?
            {
                if !mode.is_tree() {
                    tracing::error!("branch root is not a tree");
                    return Err(ConvertError);
                }
                tree_oid
            } else {
                self.git_import.empty_tree_oid()
            };

            self.branch_rev_data.push(BranchRevData {
                branch,
                parent: parent_commit,
                tail: tail_commit,
                root_rev: root_commit,
                required_in_mergeinfo: branch_ops.required_in_mergeinfo,
                added_svn_merges,
                removed_svn_merges,
                ignore_merges,
                fully_reverted_merges_in: BTreeSet::new(),
                tree_oid,
            });

            self.branch_path_commits
                .entry(branch_path.to_vec())
                .or_default()
                .push((root_commit, branch_rev));

            if branch_path == self.options.head_path {
                self.head_branch = Some(Head::Branch(branch));
            }
        }

        Ok(())
    }

    fn gather_svn_merges(
        &mut self,
        branch: usize,
        branch_rev: usize,
        branch_tip_commit: usize,
    ) -> Result<(BTreeSet<usize>, BTreeSet<usize>), ConvertError> {
        let meta = self.get_dir_meta(
            self.root_rev_data.last().unwrap().meta_tree_oid,
            &self.branch_data[branch].svn_path,
        )?;
        let svn_mergeinfo = meta::parse_mergeinfo(&meta.mergeinfo, &meta.svnmerge_integrated);

        let mut commit_history = Vec::new();
        let mut history_commit = Some(branch_tip_commit);
        while let Some(some_commit) = history_commit {
            commit_history.push(some_commit);
            history_commit = self.branch_rev_data[some_commit].parent;
        }

        let mut prev_svn_merges = BTreeSet::new();
        for &history_commit in commit_history.iter().rev() {
            for &removed_svn_merge in self.branch_rev_data[history_commit]
                .removed_svn_merges
                .iter()
            {
                prev_svn_merges.remove(&removed_svn_merge);
            }
            prev_svn_merges.extend(&self.branch_rev_data[history_commit].added_svn_merges);
        }

        let mut current_svn_merges = BTreeSet::new();
        for (merged_svn_path, merged_svn_revs) in svn_mergeinfo.iter() {
            let Some(branch_list) = self.path_to_branch.get(merged_svn_path).map(Vec::as_slice)
            else {
                // merge from non-branch
                continue;
            };

            for &merged_branch in branch_list.iter() {
                if merged_branch == branch {
                    // skip merges from itself
                    continue;
                }

                let branch_first_root_rev = self.branch_data[merged_branch].first_root_rev;
                let branch_last_root_rev = self.branch_data[merged_branch].last_root_rev;

                let branch_first_svn_rev = self.root_rev_data[branch_first_root_rev].svn_rev;
                let branch_last_svn_rev = self.root_rev_data[branch_last_root_rev].svn_rev;

                for &(mut start_svn_rev, mut end_svn_rev, non_inheritable) in merged_svn_revs.iter()
                {
                    if non_inheritable {
                        continue;
                    }

                    if start_svn_rev > branch_last_svn_rev || end_svn_rev < branch_first_svn_rev {
                        // range does not include any commit made in this branch
                        continue;
                    }

                    start_svn_rev = start_svn_rev.max(branch_first_svn_rev);
                    end_svn_rev = end_svn_rev.min(branch_last_svn_rev);

                    let start_root_rev = loop {
                        if let Some(&r) = self.svn_rev_map.get(&start_svn_rev) {
                            break r;
                        } else {
                            // At some point it will reach `branch_last_svn_rev`
                            start_svn_rev += 1;
                        }
                    };

                    let end_root_rev = loop {
                        if let Some(&r) = self.svn_rev_map.get(&end_svn_rev) {
                            break r;
                        } else {
                            // At some point it will reach `branch_first_svn_rev`
                            end_svn_rev -= 1;
                        }
                    };

                    if start_root_rev > branch_last_root_rev || end_root_rev < branch_first_root_rev
                    {
                        // range does not include any commit made in this branch
                        continue;
                    }

                    let start_merged_root_rev = branch_first_root_rev.max(start_root_rev);
                    let end_merged_root_rev = branch_last_root_rev.min(end_root_rev);
                    for merged_root_rev in start_merged_root_rev..=end_merged_root_rev {
                        if let Ok(i) = self.branch_data[merged_branch]
                            .rev_map
                            .binary_search_by_key(&merged_root_rev, |&(c, _)| c)
                        {
                            current_svn_merges.insert(self.branch_data[merged_branch].rev_map[i].1);
                        }
                    }
                }
            }
        }

        let added_svn_merges: BTreeSet<usize> = current_svn_merges
            .difference(&prev_svn_merges)
            .copied()
            .collect();

        let removed_svn_merges: BTreeSet<usize> = prev_svn_merges
            .difference(&current_svn_merges)
            .copied()
            .collect();
        if !removed_svn_merges.is_empty() {
            tracing::debug!("reverted {} SVN merge(s)", removed_svn_merges.len());

            let mut history_commit = Some(branch_tip_commit);
            while let Some(some_commit) = history_commit {
                if !self.branch_rev_data[some_commit]
                    .added_svn_merges
                    .is_empty()
                {
                    if removed_svn_merges
                        .is_superset(&self.branch_rev_data[some_commit].added_svn_merges)
                    {
                        tracing::debug!(
                            "fully reverted merge made in \"{}\"@{}",
                            self.branch_data[self.branch_rev_data[some_commit].branch]
                                .svn_path
                                .escape_ascii(),
                            self.root_rev_data[self.branch_rev_data[some_commit].root_rev].svn_rev,
                        );
                        self.branch_rev_data[some_commit]
                            .fully_reverted_merges_in
                            .insert(branch_rev);
                    } else if removed_svn_merges
                        .is_subset(&self.branch_rev_data[some_commit].added_svn_merges)
                    {
                        tracing::debug!(
                            "partially reverted merge made in \"{}\"@{}",
                            self.branch_data[self.branch_rev_data[some_commit].branch]
                                .svn_path
                                .escape_ascii(),
                            self.root_rev_data[self.branch_rev_data[some_commit].root_rev].svn_rev,
                        );
                    }
                }
                history_commit = self.branch_rev_data[some_commit].parent;
            }
        }

        Ok((added_svn_merges, removed_svn_merges))
    }

    fn get_dir_meta(
        &self,
        meta_tree_oid: gix_hash::ObjectId,
        dir_path: &[u8],
    ) -> Result<meta::DirMetadata, ConvertError> {
        let meta_path = concat_path(dir_path, b".svn");
        let (_, meta_blob_oid) =
            self.git_import
                .ls(meta_tree_oid, &meta_path)?
                .ok_or_else(|| {
                    tracing::error!(
                        "missing directory metadata for \"{}\"",
                        dir_path.escape_ascii(),
                    );
                    ConvertError
                })?;
        let raw_meta = self.git_import.get_blob(meta_blob_oid)?;
        meta::DirMetadata::deserialize(&raw_meta).ok_or_else(|| {
            tracing::error!("failed to deserialize directory metadata");
            ConvertError
        })
    }
}

pub(crate) fn concat_path(a: &[u8], b: &[u8]) -> Vec<u8> {
    assert!(!b.is_empty());
    assert!(!a.ends_with(b"/"));
    assert!(!a.starts_with(b"/"));
    assert!(!b.ends_with(b"/"));
    assert!(!b.starts_with(b"/"));

    if a.is_empty() {
        b.to_vec()
    } else {
        let mut r = Vec::with_capacity(a.len() + 1 + b.len());
        r.extend(a);
        r.push(b'/');
        r.extend(b);
        r
    }
}

fn get_path_base_dir(path: &[u8]) -> &[u8] {
    if let Some(sep_pos) = path.iter().rposition(|&c| c == b'/') {
        &path[..sep_pos]
    } else {
        b""
    }
}
