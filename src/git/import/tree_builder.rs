use std::collections::HashMap;

use gix_hash::ObjectId;
use gix_object::tree::{EntryKind, EntryMode};

use super::{ImportError, Importer};

pub(crate) struct TreeBuilder {
    root: TreeBuilderRoot,
}

impl TreeBuilder {
    pub(crate) fn new() -> Self {
        Self {
            root: TreeBuilderRoot::Tree {
                tree: HashMap::new(),
                base_oid: None,
            },
        }
    }

    pub(crate) fn reset(&mut self, base: ObjectId) {
        self.root = TreeBuilderRoot::Ext(base);
    }

    pub(crate) fn clear(&mut self) {
        self.root = TreeBuilderRoot::Tree {
            tree: HashMap::new(),
            base_oid: None,
        };
    }

    pub(crate) fn mod_oid(
        &mut self,
        path: &[u8],
        mode: EntryMode,
        oid: ObjectId,
        importer: &mut Importer,
    ) -> Result<(), ImportError> {
        let (entry_tree, entry_name) = self.find_entry(path, true, importer)?.unwrap();

        entry_tree.insert(entry_name.to_vec(), TreeBuilderEntry::Entry(mode, oid));
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
        let (entry_tree, entry_name) = self.find_entry(path, true, importer)?.unwrap();

        let blob_oid = importer.put_blob(blob, delta_base)?;

        entry_tree.insert(entry_name.to_vec(), TreeBuilderEntry::Entry(mode, blob_oid));
        Ok(blob_oid)
    }

    pub(crate) fn rm(&mut self, path: &[u8], importer: &mut Importer) -> Result<bool, ImportError> {
        if let Some((entry_tree, entry_name)) = self.find_entry(path, false, importer)? {
            Ok(entry_tree.remove(entry_name).is_some())
        } else {
            Ok(false)
        }
    }

    pub(crate) fn ls(
        &mut self,
        path: &[u8],
        importer: &mut Importer,
    ) -> Result<Option<(EntryMode, ObjectId)>, ImportError> {
        if let Some((entry_tree, entry_name)) = self.find_entry(path, false, importer)? {
            if let Some(entry) = entry_tree.get_mut(entry_name) {
                match *entry {
                    TreeBuilderEntry::Entry(mode, oid) => Ok(Some((mode, oid))),
                    TreeBuilderEntry::SubTree {
                        tree: ref sub_tree,
                        base_oid,
                    } => {
                        if let Some(oid) = Self::materialize_subtree(sub_tree, base_oid, importer)?
                        {
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
        create: bool,
        importer: &mut Importer,
    ) -> Result<Option<(&'a mut HashMap<Vec<u8>, TreeBuilderEntry>, &'b [u8])>, ImportError> {
        let mut comps = path.split(|&c| c == b'/');
        let last_comp = comps.next_back().unwrap();

        if let TreeBuilderRoot::Ext(tree_oid) = self.root {
            self.root = TreeBuilderRoot::Tree {
                tree: Self::read_tree(tree_oid, importer)?,
                base_oid: Some(tree_oid),
            };
        }

        let mut cur_tree = match self.root {
            TreeBuilderRoot::Tree { ref mut tree, .. } => tree,
            TreeBuilderRoot::Ext(_) => unreachable!(),
        };

        for dir_comp in comps {
            if cur_tree.contains_key(dir_comp) {
                let entry = cur_tree.get_mut(dir_comp).unwrap();
                match *entry {
                    TreeBuilderEntry::SubTree { ref mut tree, .. } => {
                        cur_tree = tree;
                    }
                    TreeBuilderEntry::Entry(mode, oid) if mode.is_tree() => {
                        *entry = TreeBuilderEntry::SubTree {
                            tree: Self::read_tree(oid, importer)?,
                            base_oid: Some(oid),
                        };
                        cur_tree = match *entry {
                            TreeBuilderEntry::SubTree { ref mut tree, .. } => tree,
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
                let entry = cur_tree.entry(dir_comp.to_vec()).or_insert_with(|| {
                    TreeBuilderEntry::SubTree {
                        tree: HashMap::new(),
                        base_oid: None,
                    }
                });
                cur_tree = match *entry {
                    TreeBuilderEntry::SubTree { ref mut tree, .. } => tree,
                    TreeBuilderEntry::Entry(..) => unreachable!(),
                };
            } else {
                return Ok(None);
            }
        }

        Ok(Some((cur_tree, last_comp)))
    }

    fn read_tree(
        tree_oid: ObjectId,
        importer: &mut Importer,
    ) -> Result<HashMap<Vec<u8>, TreeBuilderEntry>, ImportError> {
        let (obj_kind, raw_obj) = importer.get_raw(tree_oid)?;
        if obj_kind != gix_object::Kind::Tree {
            return Err(ImportError::UnexpectedObjectKind {
                id: tree_oid,
                kind: obj_kind,
            });
        }
        let tree = gix_object::TreeRef::from_bytes(&raw_obj)
            .map_err(|_| ImportError::ParseObjectError { oid: tree_oid })?;

        Ok(tree
            .entries
            .into_iter()
            .map(|entry| {
                (
                    entry.filename.to_vec(),
                    TreeBuilderEntry::Entry(entry.mode, entry.oid.into()),
                )
            })
            .collect())
    }

    pub(crate) fn materialize(&mut self, importer: &mut Importer) -> Result<ObjectId, ImportError> {
        match self.root {
            TreeBuilderRoot::Tree { ref tree, base_oid } => {
                if let Some(tree_oid) = Self::materialize_subtree(tree, base_oid, importer)? {
                    self.root = TreeBuilderRoot::Ext(tree_oid);
                    Ok(tree_oid)
                } else {
                    let tree_oid = importer.put(gix_object::Tree { entries: vec![] }, None)?;
                    self.root = TreeBuilderRoot::Tree {
                        tree: HashMap::new(),
                        base_oid: None,
                    };
                    Ok(tree_oid)
                }
            }
            TreeBuilderRoot::Ext(tree_oid) => Ok(tree_oid),
        }
    }

    fn materialize_subtree(
        sub_tree: &HashMap<Vec<u8>, TreeBuilderEntry>,
        base_oid: Option<ObjectId>,
        importer: &mut Importer,
    ) -> Result<Option<ObjectId>, ImportError> {
        let mut entries = Vec::new();
        for (k, v) in sub_tree.iter() {
            match *v {
                TreeBuilderEntry::SubTree {
                    tree: ref sub_tree,
                    base_oid,
                } => {
                    if let Some(sub_tree_oid) =
                        Self::materialize_subtree(sub_tree, base_oid, importer)?
                    {
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
                .put(gix_object::Tree { entries }, base_oid)
                .map(Some)
        }
    }
}

enum TreeBuilderRoot {
    Tree {
        tree: HashMap<Vec<u8>, TreeBuilderEntry>,
        base_oid: Option<ObjectId>,
    },
    Ext(ObjectId),
}

enum TreeBuilderEntry {
    SubTree {
        tree: HashMap<Vec<u8>, TreeBuilderEntry>,
        base_oid: Option<ObjectId>,
    },
    Entry(EntryMode, ObjectId),
}
