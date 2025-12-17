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
            root: TreeBuilderRoot::Tree(TreeBuilderTree::empty()),
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
        let (entry_tree, entry_name) = self.find_entry(path, true, true, importer)?.unwrap();

        entry_tree
            .entries
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
        let (entry_tree, entry_name) = self.find_entry(path, true, true, importer)?.unwrap();

        let blob_oid = importer.put_blob(blob, delta_base)?;

        entry_tree
            .entries
            .insert(entry_name.to_vec(), TreeBuilderEntry::Entry(mode, blob_oid));
        Ok(blob_oid)
    }

    pub(crate) fn rm(&mut self, path: &[u8], importer: &mut Importer) -> Result<bool, ImportError> {
        if let Some((entry_tree, entry_name)) = self.find_entry(path, true, false, importer)? {
            Ok(entry_tree.entries.remove(entry_name).is_some())
        } else {
            Ok(false)
        }
    }

    pub(crate) fn ls(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<Option<(EntryMode, ObjectId)>, ImportError> {
        if let Some((entry_tree, entry_name)) = self.find_entry(path, false, false, importer)? {
            if let Some(entry) = entry_tree.entries.get_mut(entry_name) {
                match *entry {
                    TreeBuilderEntry::Entry(mode, oid) => Ok(Some((mode, oid))),
                    TreeBuilderEntry::SubTree(ref sub_tree) => {
                        if let Some(oid) = Self::materialize_sub_tree(sub_tree, importer)? {
                            let mode = EntryKind::Tree.into();
                            *entry = TreeBuilderEntry::Entry(mode, oid);
                            Ok(Some((mode, oid)))
                        } else {
                            Ok(None)
                        }
                    }
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
    ) -> Result<Option<(&'a mut TreeBuilderTree, &'b [u8])>, ImportError> {
        assert!(modify || !create);

        let mut comps = path.split(|&c| c == b'/');
        let last_comp = comps.next_back().unwrap();

        if let TreeBuilderRoot::Ext(tree_oid) = self.root {
            self.root = TreeBuilderRoot::Tree(Self::read_tree(tree_oid, importer)?);
        }

        let mut cur_tree = match self.root {
            TreeBuilderRoot::Tree(ref mut tree) => tree,
            TreeBuilderRoot::Ext(_) => unreachable!(),
        };

        for dir_comp in comps {
            cur_tree.modified |= modify;
            if cur_tree.entries.contains_key(dir_comp) {
                let entry = cur_tree.entries.get_mut(dir_comp).unwrap();
                match *entry {
                    TreeBuilderEntry::SubTree(ref mut tree) => {
                        cur_tree = tree;
                    }
                    TreeBuilderEntry::Entry(mode, oid) if mode.is_tree() => {
                        *entry = TreeBuilderEntry::SubTree(Self::read_tree(oid, importer)?);
                        cur_tree = match *entry {
                            TreeBuilderEntry::SubTree(ref mut tree) => tree,
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
                let entry = cur_tree
                    .entries
                    .entry(dir_comp.to_vec())
                    .or_insert_with(|| TreeBuilderEntry::SubTree(TreeBuilderTree::empty()));
                cur_tree = match *entry {
                    TreeBuilderEntry::SubTree(ref mut tree) => tree,
                    TreeBuilderEntry::Entry(..) => unreachable!(),
                };
            } else {
                return Ok(None);
            }
        }

        cur_tree.modified |= modify;
        Ok(Some((cur_tree, last_comp)))
    }

    fn read_tree(
        tree_oid: ObjectId,
        importer: &mut Importer,
    ) -> Result<TreeBuilderTree, ImportError> {
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

        Ok(TreeBuilderTree {
            modified: false,
            base_oid,
            entries,
        })
    }

    pub(crate) fn materialize(mut self, importer: &mut Importer) -> Result<ObjectId, ImportError> {
        match self.root {
            TreeBuilderRoot::Tree(ref tree) => {
                if let Some(tree_oid) = Self::materialize_sub_tree(tree, importer)? {
                    self.root = TreeBuilderRoot::Ext(tree_oid);
                    Ok(tree_oid)
                } else {
                    let tree_oid = importer.empty_tree_oid();
                    self.root = TreeBuilderRoot::Tree(TreeBuilderTree::empty());
                    Ok(tree_oid)
                }
            }
            TreeBuilderRoot::Ext(tree_oid) => Ok(tree_oid),
        }
    }

    fn materialize_sub_tree(
        sub_tree: &TreeBuilderTree,
        importer: &mut Importer,
    ) -> Result<Option<ObjectId>, ImportError> {
        if !sub_tree.modified {
            assert_eq!(sub_tree.base_oid.is_none(), sub_tree.entries.is_empty());
            return Ok(sub_tree.base_oid);
        }

        let mut entries = Vec::new();
        for (k, v) in sub_tree.entries.iter() {
            match *v {
                TreeBuilderEntry::SubTree(ref sub_tree) => {
                    if let Some(sub_tree_oid) = Self::materialize_sub_tree(sub_tree, importer)? {
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
                .put(gix_object::Tree { entries }, sub_tree.base_oid)
                .map(Some)
        }
    }
}

enum TreeBuilderRoot {
    Tree(TreeBuilderTree),
    Ext(ObjectId),
}

enum TreeBuilderEntry {
    SubTree(TreeBuilderTree),
    Entry(EntryMode, ObjectId),
}

struct TreeBuilderTree {
    modified: bool,
    base_oid: Option<ObjectId>,
    entries: FHashMap<Vec<u8>, TreeBuilderEntry>,
}

impl TreeBuilderTree {
    fn empty() -> Self {
        Self {
            modified: false,
            base_oid: None,
            entries: FHashMap::default(),
        }
    }
}
