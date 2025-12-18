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
        let importer = git::Importer::init(path, obj_cache_size).map_err(|e| {
            tracing::error!("failed to initialize git import: {e}");
            ConvertError
        })?;
        Ok(Self { importer })
    }

    #[inline]
    pub(super) fn inner(&mut self) -> &mut git::Importer {
        &mut self.importer
    }

    pub(super) fn abort(self) {
        self.importer.abort();
    }

    pub(super) fn finish(
        self,
        progress_cb: impl FnMut(git::ImportFinishProgress),
    ) -> Result<(), ConvertError> {
        self.importer.finish(progress_cb).map_err(|e| {
            tracing::error!("failed to finalize git import: {e}");
            ConvertError
        })
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
        self.importer.put(object, delta_base).map_err(|e| {
            tracing::error!("failed to put object: {e}");
            ConvertError
        })
    }

    pub(crate) fn put_blob(
        &mut self,
        data: Vec<u8>,
        delta_base: Option<gix_hash::ObjectId>,
    ) -> Result<gix_hash::ObjectId, ConvertError> {
        self.importer.put_blob(data, delta_base).map_err(|e| {
            tracing::error!("failed to put object: {e}");
            ConvertError
        })
    }

    pub(crate) fn get_raw(
        &self,
        id: gix_hash::ObjectId,
    ) -> Result<(gix_object::Kind, Vec<u8>), ConvertError> {
        self.importer.get_raw(id).map_err(|e| {
            tracing::error!("failed to get object {id}: {e}");
            ConvertError
        })
    }

    pub(super) fn get<T: TryFrom<gix_object::Object, Error = gix_object::Object>>(
        &self,
        id: gix_hash::ObjectId,
    ) -> Result<T, ConvertError> {
        self.importer.get(id).map_err(|e| {
            tracing::error!("failed to get object {id}: {e}");
            ConvertError
        })
    }

    pub(super) fn get_blob(&self, id: gix_hash::ObjectId) -> Result<Vec<u8>, ConvertError> {
        self.importer.get_blob(id).map_err(|e| {
            tracing::error!("failed to get object {id}: {e}");
            ConvertError
        })
    }

    pub(super) fn ls(
        &self,
        root_oid: gix_hash::ObjectId,
        path: &[u8],
    ) -> Result<Option<(gix_object::tree::EntryMode, gix_hash::ObjectId)>, ConvertError> {
        self.importer.ls(root_oid, path).map_err(|e| {
            tracing::error!(
                "failed to ls \"{}\" at {root_oid}: {e}",
                path.escape_ascii(),
            );
            ConvertError
        })
    }

    pub(crate) fn set_head(&mut self, head_ref: &str) {
        self.importer.set_head(head_ref);
    }

    pub(super) fn set_ref(&mut self, ref_name: &str, commit_oid: gix_hash::ObjectId) {
        self.importer.set_ref(ref_name, commit_oid);
    }
}
