use super::ConvertError;
use crate::git;

pub(super) struct Importer {
    importer: git::Importer,
}

impl Importer {
    pub(super) fn init(
        path: &std::path::Path,
        obj_cache_size: usize,
    ) -> Result<Self, ConvertError> {
        match git::Importer::init(path, obj_cache_size) {
            Ok(importer) => Ok(Self { importer }),
            Err(e) => {
                tracing::error!("failed to initialize git import: {e}");
                Err(ConvertError)
            }
        }
    }

    pub(super) fn abort(self) {
        self.importer.abort();
    }

    pub(super) fn finish(
        self,
        progress_cb: impl FnMut(git::ImportFinishProgress),
    ) -> Result<(), ConvertError> {
        match self.importer.finish(progress_cb) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::error!("failed to finalize git import: {e}");
                Err(ConvertError)
            }
        }
    }

    #[inline]
    pub(super) fn empty_tree_oid(&self) -> gix_hash::ObjectId {
        self.importer.empty_tree_oid()
    }

    pub(super) fn put(
        &mut self,
        object: impl gix_object::WriteTo,
        delta_base: Option<gix_hash::ObjectId>,
    ) -> Result<gix_hash::ObjectId, ConvertError> {
        match self.importer.put(object, delta_base) {
            Ok(id) => Ok(id),
            Err(e) => {
                tracing::error!("failed to put object: {e}");
                Err(ConvertError)
            }
        }
    }

    pub(super) fn get<T: TryFrom<gix_object::Object, Error = gix_object::Object>>(
        &self,
        id: gix_hash::ObjectId,
    ) -> Result<T, ConvertError> {
        match self.importer.get(id) {
            Ok(obj) => Ok(obj),
            Err(e) => {
                tracing::error!("failed to get object {id}: {e}");
                Err(ConvertError)
            }
        }
    }

    pub(super) fn get_blob(&self, id: gix_hash::ObjectId) -> Result<Vec<u8>, ConvertError> {
        match self.importer.get_blob(id) {
            Ok(blob) => Ok(blob),
            Err(e) => {
                tracing::error!("failed to get object {id}: {e}");
                Err(ConvertError)
            }
        }
    }

    pub(super) fn ls(
        &self,
        root_oid: gix_hash::ObjectId,
        path: &[u8],
    ) -> Result<Option<(gix_object::tree::EntryMode, gix_hash::ObjectId)>, ConvertError> {
        match self.importer.ls(root_oid, path) {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::error!(
                    "failed to ls \"{}\" at {root_oid}: {e}",
                    path.escape_ascii()
                );
                Err(ConvertError)
            }
        }
    }

    pub(crate) fn set_head(&mut self, head_ref: &str) {
        self.importer.set_head(head_ref);
    }

    pub(super) fn set_ref(&mut self, ref_name: &str, commit_oid: gix_hash::ObjectId) {
        self.importer.set_ref(ref_name, commit_oid);
    }
}

pub(super) struct TreeBuilder {
    tree_builder: git::TreeBuilder,
}

impl TreeBuilder {
    pub(super) fn new() -> Self {
        Self {
            tree_builder: git::TreeBuilder::new(),
        }
    }

    pub(super) fn reset(&mut self, base: gix_hash::ObjectId) {
        self.tree_builder.reset(base);
    }

    pub(super) fn clear(&mut self) {
        self.tree_builder.clear();
    }

    pub(super) fn mod_oid(
        &mut self,
        path: &[u8],
        mode: gix_object::tree::EntryMode,
        oid: gix_hash::ObjectId,
        importer: &mut Importer,
    ) -> Result<(), ConvertError> {
        match self
            .tree_builder
            .mod_oid(path, mode, oid, &mut importer.importer)
        {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::error!(
                    "failed to set tree entry at \"{}\": {e}",
                    path.escape_ascii(),
                );
                Err(ConvertError)
            }
        }
    }

    pub(super) fn mod_inline(
        &mut self,
        path: &[u8],
        mode: gix_object::tree::EntryMode,
        blob: Vec<u8>,
        delta_base: Option<gix_hash::ObjectId>,
        importer: &mut Importer,
    ) -> Result<gix_hash::ObjectId, ConvertError> {
        self.tree_builder
            .mod_inline(path, mode, blob, delta_base, &mut importer.importer)
            .map_err(|e| {
                tracing::error!(
                    "failed to set tree entry at \"{}\": {e}",
                    path.escape_ascii(),
                );
                ConvertError
            })
    }

    pub(super) fn rm(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<bool, ConvertError> {
        match self.tree_builder.rm(path, &mut importer.importer) {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::error!(
                    "failed to remove tree entry at \"{}\": {e}",
                    path.escape_ascii(),
                );
                Err(ConvertError)
            }
        }
    }

    pub(crate) fn ls(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<Option<(gix_object::tree::EntryMode, gix_hash::ObjectId)>, ConvertError> {
        match self.tree_builder.ls(path, &mut importer.importer) {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::error!("failed to ls \"{}\": {e}", path.escape_ascii());
                Err(ConvertError)
            }
        }
    }

    pub(super) fn materialize(
        &mut self,
        importer: &mut Importer,
    ) -> Result<gix_hash::ObjectId, ConvertError> {
        match self.tree_builder.materialize(&mut importer.importer) {
            Ok(id) => Ok(id),
            Err(e) => {
                tracing::error!("failed to materialize tree: {e}");
                Err(ConvertError)
            }
        }
    }
}
