use gix_hash::ObjectId;
use gix_object::tree::{EntryKind, EntryMode};

use super::{ImportError, Importer};
use crate::FHashMap;

pub(crate) struct TreeBuilder {
    root: TreeBuilderRoot,
}

impl TreeBuilder {
    pub(crate) fn new() -> Self {
        Self {
            root: TreeBuilderRoot::Tree(TreeBuilderNode::empty()),
        }
    }

    pub(crate) fn with_base(base: ObjectId) -> Self {
        Self {
            root: TreeBuilderRoot::Ext(base),
        }
    }

    pub(crate) fn mod_oid(
        &mut self,
        path: &[u8],
        mode: EntryMode,
        oid: ObjectId,
        importer: &mut Importer,
    ) -> Result<(), ImportError> {
        let (node, entry_name) = self.find_entry(path, true, true, importer)?.unwrap();

        node.entries
            .insert(entry_name.to_vec(), TreeBuilderEntry::Entry(mode, oid));
        Ok(())
    }

    pub(crate) fn mod_inline(
        &mut self,
        path: &[u8],
        mode: EntryMode,
        blob: Vec<u8>,
        delta_base: Option<ObjectId>,
        importer: &mut Importer,
    ) -> Result<ObjectId, ImportError> {
        let (node, entry_name) = self.find_entry(path, true, true, importer)?.unwrap();

        let blob_oid = importer.put_blob(blob, delta_base)?;

        node.entries
            .insert(entry_name.to_vec(), TreeBuilderEntry::Entry(mode, blob_oid));
        Ok(blob_oid)
    }

    pub(crate) fn rm(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<Option<(EntryMode, ObjectId)>, ImportError> {
        if let Some((node, entry_name)) = self.find_entry(path, true, false, importer)? {
            Ok(node
                .entries
                .remove(entry_name)
                .and_then(|entry| match entry {
                    TreeBuilderEntry::Entry(mode, oid) => Some((mode, oid)),
                    TreeBuilderEntry::Tree(sub_node) => {
                        sub_node.base_oid.map(|oid| (EntryKind::Tree.into(), oid))
                    }
                }))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn ls_file(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<Option<(EntryMode, ObjectId)>, ImportError> {
        if let Some((node, entry_name)) = self.find_entry(path, false, false, importer)? {
            if let Some(entry) = node.entries.get_mut(entry_name) {
                match *entry {
                    TreeBuilderEntry::Entry(mode, _) if mode.is_tree() => Ok(None),
                    TreeBuilderEntry::Entry(mode, oid) => Ok(Some((mode, oid))),
                    TreeBuilderEntry::Tree(_) => Ok(None),
                }
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    fn find_entry<'a, 'b>(
        &'a mut self,
        path: &'b [u8],
        modify: bool,
        create: bool,
        importer: &mut Importer,
    ) -> Result<Option<(&'a mut TreeBuilderNode, &'b [u8])>, ImportError> {
        assert!(modify || !create);

        let mut comps = path.split(|&c| c == b'/');
        let last_comp = comps.next_back().unwrap();

        if let TreeBuilderRoot::Ext(tree_oid) = self.root {
            self.root = TreeBuilderRoot::Tree(Self::read_tree(tree_oid, importer)?);
        }

        let mut cur_node = match self.root {
            TreeBuilderRoot::Tree(ref mut tree) => tree,
            TreeBuilderRoot::Ext(_) => unreachable!(),
        };

        for dir_comp in comps {
            cur_node.modified |= modify;
            if cur_node.entries.contains_key(dir_comp) {
                let entry = cur_node.entries.get_mut(dir_comp).unwrap();
                match *entry {
                    TreeBuilderEntry::Tree(ref mut sub_node) => {
                        cur_node = sub_node;
                    }
                    TreeBuilderEntry::Entry(mode, oid) if mode.is_tree() => {
                        *entry = TreeBuilderEntry::Tree(Self::read_tree(oid, importer)?);
                        cur_node = match *entry {
                            TreeBuilderEntry::Tree(ref mut sub_node) => sub_node,
                            TreeBuilderEntry::Entry(..) => unreachable!(),
                        };
                    }
                    TreeBuilderEntry::Entry(..) => {
                        if create {
                            return Err(ImportError::ParentPathIsNotDir {
                                path: path.to_vec(),
                            });
                        } else {
                            return Ok(None);
                        }
                    }
                }
            } else if create {
                let entry = cur_node
                    .entries
                    .entry(dir_comp.to_vec())
                    .or_insert_with(|| TreeBuilderEntry::Tree(TreeBuilderNode::empty()));
                cur_node = match *entry {
                    TreeBuilderEntry::Tree(ref mut sub_node) => sub_node,
                    TreeBuilderEntry::Entry(..) => unreachable!(),
                };
            } else {
                return Ok(None);
            }
        }

        cur_node.modified |= modify;
        Ok(Some((cur_node, last_comp)))
    }

    fn read_tree(
        tree_oid: ObjectId,
        importer: &mut Importer,
    ) -> Result<TreeBuilderNode, ImportError> {
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
        let entries = tree
            .entries
            .into_iter()
            .map(|entry| {
                (
                    entry.filename.to_vec(),
                    TreeBuilderEntry::Entry(entry.mode, entry.oid.into()),
                )
            })
            .collect();

        Ok(TreeBuilderNode {
            modified: false,
            base_oid,
            entries,
        })
    }

    pub(crate) fn build(mut self, importer: &mut Importer) -> Result<ObjectId, ImportError> {
        match self.root {
            TreeBuilderRoot::Tree(ref node) => {
                if let Some(tree_oid) = Self::build_node(node, importer)? {
                    self.root = TreeBuilderRoot::Ext(tree_oid);
                    Ok(tree_oid)
                } else {
                    let tree_oid = importer.empty_tree_oid();
                    self.root = TreeBuilderRoot::Tree(TreeBuilderNode::empty());
                    Ok(tree_oid)
                }
            }
            TreeBuilderRoot::Ext(tree_oid) => Ok(tree_oid),
        }
    }

    fn build_node(
        node: &TreeBuilderNode,
        importer: &mut Importer,
    ) -> Result<Option<ObjectId>, ImportError> {
        if !node.modified {
            assert_eq!(node.base_oid.is_none(), node.entries.is_empty());
            return Ok(node.base_oid);
        }

        let mut entries = Vec::new();
        for (k, v) in node.entries.iter() {
            match *v {
                TreeBuilderEntry::Tree(ref sub_node) => {
                    if let Some(sub_tree_oid) = Self::build_node(sub_node, importer)? {
                        entries.push(gix_object::tree::Entry {
                            mode: EntryKind::Tree.into(),
                            filename: k.as_slice().into(),
                            oid: sub_tree_oid,
                        });
                    }
                }
                TreeBuilderEntry::Entry(mode, oid) => {
                    if !mode.is_tree() || oid != importer.empty_tree_oid() {
                        entries.push(gix_object::tree::Entry {
                            mode,
                            filename: k.as_slice().into(),
                            oid,
                        });
                    }
                }
            }
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            entries.sort();
            importer
                .put(gix_object::Tree { entries }, node.base_oid)
                .map(Some)
        }
    }
}

enum TreeBuilderRoot {
    Tree(TreeBuilderNode),
    Ext(ObjectId),
}

enum TreeBuilderEntry {
    Tree(TreeBuilderNode),
    Entry(EntryMode, ObjectId),
}

struct TreeBuilderNode {
    modified: bool,
    base_oid: Option<ObjectId>,
    entries: FHashMap<Vec<u8>, TreeBuilderEntry>,
}

impl TreeBuilderNode {
    fn empty() -> Self {
        Self {
            modified: false,
            base_oid: None,
            entries: FHashMap::default(),
        }
    }
}
