use gix_hash::ObjectId;
use gix_object::tree::{EntryKind, EntryMode};

use super::{ImportError, Importer};
use crate::FHashMap;

pub(crate) struct ChangeSet {
    orig: Option<ObjectId>,
    root: FHashMap<Vec<u8>, EntryChange>,
}

enum EntryChange {
    Remove,
    Change(EntryMode, ObjectId),
    ChangeTree(FHashMap<Vec<u8>, EntryChange>),
    NewTree(Option<ObjectId>, FHashMap<Vec<u8>, EntryChange>),
}

impl ChangeSet {
    pub(crate) fn new(orig: Option<ObjectId>) -> Self {
        Self {
            orig,
            root: FHashMap::default(),
        }
    }

    pub(crate) fn remove(&mut self, path: &[u8]) {
        self.set_entry(path, EntryChange::Remove);
    }

    pub(crate) fn change(&mut self, path: &[u8], mode: EntryMode, oid: ObjectId) {
        self.set_entry(path, EntryChange::Change(mode, oid));
    }

    fn set_entry(&mut self, path: &[u8], value: EntryChange) {
        let mut components = path.split(|&c| c == b'/');
        let last_component = components.next_back().unwrap();

        let mut cur_tree = &mut self.root;
        for component in components {
            cur_tree = match cur_tree.entry(component.to_vec()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let entry = entry.insert(EntryChange::ChangeTree(FHashMap::default()));
                    match entry {
                        EntryChange::ChangeTree(sub_tree) => sub_tree,
                        _ => unreachable!(),
                    }
                }
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let entry = entry.into_mut();
                    match entry {
                        EntryChange::ChangeTree(sub_tree) | EntryChange::NewTree(_, sub_tree) => {
                            sub_tree
                        }
                        EntryChange::Change(mode, oid) if mode.is_tree() => {
                            *entry = EntryChange::NewTree(Some(*oid), FHashMap::default());
                            match entry {
                                EntryChange::NewTree(_, sub_tree) => sub_tree,
                                _ => unreachable!(),
                            }
                        }
                        EntryChange::Remove | EntryChange::Change(_, _) => {
                            *entry = EntryChange::NewTree(None, FHashMap::default());
                            match entry {
                                EntryChange::NewTree(_, sub_tree) => sub_tree,
                                _ => unreachable!(),
                            }
                        }
                    }
                }
            };
        }

        cur_tree.insert(last_component.to_vec(), value);
    }

    pub(crate) fn apply(&self, importer: &mut Importer) -> Result<Option<ObjectId>, ImportError> {
        Self::apply_tree(&self.root, self.orig, importer)
    }

    fn apply_tree(
        change_set: &FHashMap<Vec<u8>, EntryChange>,
        orig_tree_oid: Option<ObjectId>,
        importer: &mut Importer,
    ) -> Result<Option<ObjectId>, ImportError> {
        let raw_orig_tree = if let Some(orig_tree_oid) = orig_tree_oid {
            let (kind, raw_orig_tree) = importer.get_raw(orig_tree_oid)?;
            assert_eq!(
                kind,
                gix_object::Kind::Tree,
                "unexpected object kind for {orig_tree_oid}",
            );
            Some(raw_orig_tree)
        } else {
            None
        };

        let mut entries = if let Some(ref raw_orig_tree) = raw_orig_tree {
            let orig_tree = gix_object::TreeRef::from_bytes(raw_orig_tree).unwrap_or_else(|_| {
                panic!("failed to parse object {}", orig_tree_oid.unwrap());
            });

            orig_tree
                .entries
                .iter()
                .map(|entry| (entry.filename, (entry.mode, ObjectId::from(entry.oid))))
                .collect()
        } else {
            FHashMap::default()
        };

        use gix_object::bstr::BStr;
        for (entry_name, entry_change) in change_set.iter() {
            match *entry_change {
                EntryChange::Remove => {
                    entries.remove(BStr::new(entry_name));
                }
                EntryChange::Change(mode, oid) => {
                    entries.insert(BStr::new(entry_name), (mode, oid));
                }
                EntryChange::ChangeTree(ref sub_tree) => {
                    let sub_tree_orig_oid = entries
                        .get(BStr::new(entry_name))
                        .and_then(|&(mode, oid)| if mode.is_tree() { Some(oid) } else { None });
                    if let Some(sub_tree_oid) =
                        Self::apply_tree(sub_tree, sub_tree_orig_oid, importer)?
                    {
                        entries.insert(
                            BStr::new(entry_name),
                            (EntryKind::Tree.into(), sub_tree_oid),
                        );
                    } else {
                        entries.remove(BStr::new(entry_name));
                    }
                }
                EntryChange::NewTree(sub_tree_orig_oid, ref sub_tree) => {
                    if let Some(sub_tree_oid) =
                        Self::apply_tree(sub_tree, sub_tree_orig_oid, importer)?
                    {
                        entries.insert(
                            BStr::new(entry_name),
                            (EntryKind::Tree.into(), sub_tree_oid),
                        );
                    } else {
                        entries.remove(BStr::new(entry_name));
                    }
                }
            }
        }

        if entries.is_empty() {
            Ok(None)
        } else {
            let mut entries: Vec<_> = entries
                .iter()
                .map(|(name, &(mode, ref oid))| gix_object::tree::EntryRef {
                    filename: name,
                    mode,
                    oid: oid.as_ref(),
                })
                .collect();

            entries.sort();

            importer
                .put(gix_object::TreeRef { entries }, orig_tree_oid)
                .map(Some)
        }
    }
}
