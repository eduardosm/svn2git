use gix_hash::ObjectId;
use gix_object::tree::EntryKind;

use super::{ConvertError, git_wrap};
use crate::FHashMap;

pub(super) const METADATA_FILE_NAME: &[u8] = b".";

pub(super) struct TreeBuilder {
    root: TreeBuilderRoot,
}

impl TreeBuilder {
    pub(super) fn new(root_metadata: ObjectId) -> Self {
        Self {
            root: TreeBuilderRoot::Loaded(TreeBuilderNode::empty(root_metadata)),
        }
    }

    pub(super) fn with_base(base: ObjectId) -> Self {
        Self {
            root: TreeBuilderRoot::Stored(base),
        }
    }

    pub(super) fn mod_oid(
        &mut self,
        path: &[u8],
        kind: EntryKind,
        oid: ObjectId,
        importer: &mut git_wrap::Importer,
    ) -> Result<(), ConvertError> {
        if path.is_empty() {
            tracing::error!("attempted to modify root directory");
            return Err(ConvertError);
        }

        let Some((node, entry_name)) = self.find_entry(path, true, importer)? else {
            tracing::error!(
                "attempted to modify entry \"{}\" at non-existing parent",
                path.escape_ascii(),
            );
            return Err(ConvertError);
        };

        node.entries
            .insert(entry_name.to_vec(), TreeBuilderEntry::Stored(kind, oid));
        Ok(())
    }

    pub(super) fn mod_inline(
        &mut self,
        path: &[u8],
        kind: EntryKind,
        blob: Vec<u8>,
        delta_base: Option<ObjectId>,
        importer: &mut git_wrap::Importer,
    ) -> Result<ObjectId, ConvertError> {
        let blob_oid = importer.put_blob(blob, delta_base)?;
        self.mod_oid(path, kind, blob_oid, importer)?;
        Ok(blob_oid)
    }

    pub(super) fn mkdir(
        &mut self,
        path: &[u8],
        metadata: ObjectId,
        importer: &mut git_wrap::Importer,
    ) -> Result<(), ConvertError> {
        if path.is_empty() {
            tracing::error!("attempted to create root directory");
            return Err(ConvertError);
        }

        let Some((node, entry_name)) = self.find_entry(path, true, importer)? else {
            tracing::error!(
                "attempted to create directory \"{}\" at non-existing parent",
                path.escape_ascii(),
            );
            return Err(ConvertError);
        };
        match node.entries.entry(entry_name.to_vec()) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(TreeBuilderEntry::Loaded(TreeBuilderNode::empty(metadata)));
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                tracing::error!(
                    "attempted to create directory \"{}\" at existing path",
                    path.escape_ascii(),
                );
                Err(ConvertError)
            }
        }
    }

    pub(super) fn rm(
        &mut self,
        path: &[u8],
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<(EntryKind, ObjectId)>, ConvertError> {
        if path.is_empty() {
            tracing::error!("attempted to remove root directory");
            return Err(ConvertError);
        }

        if let Some((node, entry_name)) = self.find_entry(path, true, importer)? {
            Ok(node
                .entries
                .remove(entry_name)
                .and_then(|entry| match entry {
                    TreeBuilderEntry::Stored(kind, oid) => Some((kind, oid)),
                    TreeBuilderEntry::Loaded(sub_node) => {
                        sub_node.base_oid.map(|oid| (EntryKind::Tree, oid))
                    }
                }))
        } else {
            Ok(None)
        }
    }

    pub(super) fn ls_file(
        &mut self,
        path: &[u8],
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<(EntryKind, ObjectId)>, ConvertError> {
        if path.is_empty() {
            return Ok(None);
        }

        if let Some((node, entry_name)) = self.find_entry(path, false, importer)? {
            if let Some(entry) = node.entries.get_mut(entry_name) {
                match *entry {
                    TreeBuilderEntry::Stored(EntryKind::Tree, _) => Ok(None),
                    TreeBuilderEntry::Stored(kind, oid) => Ok(Some((kind, oid))),
                    TreeBuilderEntry::Loaded(_) => Ok(None),
                }
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    pub(super) fn ls_metadata(
        &mut self,
        path: &[u8],
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<ObjectId>, ConvertError> {
        if let Some(node) = self.find_node(path, false, importer)? {
            Ok(Some(node.metadata))
        } else {
            Ok(None)
        }
    }

    pub(super) fn mod_metadata(
        &mut self,
        path: &[u8],
        oid: ObjectId,
        importer: &mut git_wrap::Importer,
    ) -> Result<(), ConvertError> {
        let Some(node) = self.find_node(path, true, importer)? else {
            tracing::error!(
                "attempted to modify metadata of non-existing directory \"{}\"",
                path.escape_ascii(),
            );
            return Err(ConvertError);
        };
        node.metadata = oid;
        Ok(())
    }

    fn find_node<'a>(
        &'a mut self,
        path: &[u8],
        modify: bool,
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<&'a mut TreeBuilderNode>, ConvertError> {
        if path.is_empty() {
            self.find_node_impl(std::iter::empty(), modify, importer)
        } else {
            self.find_node_impl(path.split(|&c| c == b'/'), modify, importer)
        }
    }

    fn find_entry<'a, 'b>(
        &'a mut self,
        path: &'b [u8],
        modify: bool,
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<(&'a mut TreeBuilderNode, &'b [u8])>, ConvertError> {
        assert!(!path.is_empty());

        let mut components = path.split(|&c| c == b'/');
        let last_component = components.next_back().unwrap();

        let Some(node) = self.find_node_impl(components, modify, importer)? else {
            return Ok(None);
        };
        node.modified |= modify;

        Ok(Some((node, last_component)))
    }

    fn find_node_impl<'a, 'b>(
        &'a mut self,
        components: impl IntoIterator<Item = &'b [u8]>,
        modify: bool,
        importer: &mut git_wrap::Importer,
    ) -> Result<Option<&'a mut TreeBuilderNode>, ConvertError> {
        if let TreeBuilderRoot::Stored(tree_oid) = self.root {
            self.root = TreeBuilderRoot::Loaded(Self::read_tree(tree_oid, importer)?);
        }

        let mut cur_node = match self.root {
            TreeBuilderRoot::Loaded(ref mut node) => node,
            TreeBuilderRoot::Stored(_) => unreachable!(),
        };

        for component in components {
            cur_node.modified |= modify;
            if cur_node.entries.contains_key(component) {
                let entry = cur_node.entries.get_mut(component).unwrap();
                match *entry {
                    TreeBuilderEntry::Loaded(ref mut sub_node) => {
                        cur_node = sub_node;
                    }
                    TreeBuilderEntry::Stored(EntryKind::Tree, oid) => {
                        *entry = TreeBuilderEntry::Loaded(Self::read_tree(oid, importer)?);
                        cur_node = match *entry {
                            TreeBuilderEntry::Loaded(ref mut sub_node) => sub_node,
                            TreeBuilderEntry::Stored(..) => unreachable!(),
                        };
                    }
                    TreeBuilderEntry::Stored(..) => {
                        return Ok(None);
                    }
                }
            } else {
                return Ok(None);
            }
        }

        cur_node.modified |= modify;
        Ok(Some(cur_node))
    }

    fn read_tree(
        tree_oid: ObjectId,
        importer: &mut git_wrap::Importer,
    ) -> Result<TreeBuilderNode, ConvertError> {
        let (obj_kind, raw_obj) = importer.get_raw(tree_oid)?;
        assert_eq!(
            obj_kind,
            gix_object::Kind::Tree,
            "unexpected object kind for {tree_oid}",
        );
        let tree = gix_object::TreeRef::from_bytes(&raw_obj).unwrap_or_else(|_| {
            panic!("failed to parse object {tree_oid}");
        });

        let base_oid = (!tree.entries.is_empty()).then_some(tree_oid);
        let mut metadata = None;
        let mut entries =
            FHashMap::with_capacity_and_hasher(tree.entries.len(), Default::default());
        for entry in tree.entries {
            if entry.filename == METADATA_FILE_NAME {
                metadata = Some(entry.oid.into());
            } else {
                entries.insert(
                    entry.filename.to_vec(),
                    TreeBuilderEntry::Stored(entry.mode.kind(), entry.oid.into()),
                );
            }
        }

        Ok(TreeBuilderNode {
            modified: false,
            base_oid,
            metadata: metadata.expect("missing directory metadata"),
            entries,
        })
    }

    pub(super) fn build(
        self,
        importer: &mut git_wrap::Importer,
        mut cb: impl FnMut(
            ObjectId,
            &gix_object::Tree,
            Option<ObjectId>,
            &mut git_wrap::Importer,
        ) -> Result<(), ConvertError>,
    ) -> Result<ObjectId, ConvertError> {
        match self.root {
            TreeBuilderRoot::Loaded(ref node) => Self::build_node(node, importer, &mut cb),
            TreeBuilderRoot::Stored(tree_oid) => Ok(tree_oid),
        }
    }

    fn build_node(
        node: &TreeBuilderNode,
        importer: &mut git_wrap::Importer,
        cb: &mut impl FnMut(
            ObjectId,
            &gix_object::Tree,
            Option<ObjectId>,
            &mut git_wrap::Importer,
        ) -> Result<(), ConvertError>,
    ) -> Result<ObjectId, ConvertError> {
        if !node.modified {
            if let Some(base_oid) = node.base_oid {
                return Ok(base_oid);
            }
        }

        let mut entries = Vec::new();
        entries.push(gix_object::tree::Entry {
            mode: EntryKind::Blob.into(),
            filename: METADATA_FILE_NAME.into(),
            oid: node.metadata,
        });

        for (k, v) in node.entries.iter() {
            match *v {
                TreeBuilderEntry::Loaded(ref sub_node) => {
                    let sub_tree_oid = Self::build_node(sub_node, importer, cb)?;
                    entries.push(gix_object::tree::Entry {
                        mode: EntryKind::Tree.into(),
                        filename: k.as_slice().into(),
                        oid: sub_tree_oid,
                    });
                }
                TreeBuilderEntry::Stored(kind, oid) => {
                    entries.push(gix_object::tree::Entry {
                        mode: kind.into(),
                        filename: k.as_slice().into(),
                        oid,
                    });
                }
            }
        }

        entries.sort();
        let tree = gix_object::Tree { entries };
        let tree_oid = importer.put(&tree, node.base_oid)?;
        cb(tree_oid, &tree, node.base_oid, importer)?;
        Ok(tree_oid)
    }
}

enum TreeBuilderRoot {
    Loaded(TreeBuilderNode),
    Stored(ObjectId),
}

enum TreeBuilderEntry {
    Loaded(TreeBuilderNode),
    Stored(EntryKind, ObjectId),
}

struct TreeBuilderNode {
    modified: bool,
    base_oid: Option<ObjectId>,
    metadata: ObjectId,
    entries: FHashMap<Vec<u8>, TreeBuilderEntry>,
}

impl TreeBuilderNode {
    fn empty(metadata: ObjectId) -> Self {
        Self {
            modified: false,
            base_oid: None,
            metadata,
            entries: FHashMap::default(),
        }
    }
}
