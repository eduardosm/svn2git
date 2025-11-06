use gix_hash::ObjectId;

#[inline]
fn get_u64_hash(id: &ObjectId) -> u64 {
    match id {
        ObjectId::Sha1(hash) => {
            let (hash_chunk, _) = hash.split_first_chunk().unwrap();
            u64::from_ne_bytes(*hash_chunk)
        }
        _ => unreachable!(),
    }
}

pub(super) struct ObjMap<T> {
    table: hashbrown::HashTable<(ObjectId, T)>,
}

impl<T> ObjMap<T> {
    pub(super) fn new() -> Self {
        Self {
            table: hashbrown::HashTable::new(),
        }
    }

    #[inline]
    pub(super) fn len(&self) -> usize {
        self.table.len()
    }

    pub(super) fn insert(&mut self, key: ObjectId, value: T) -> Option<T> {
        match self.table.entry(
            get_u64_hash(&key),
            |(k, _)| *k == key,
            |(k, _)| get_u64_hash(k),
        ) {
            hashbrown::hash_table::Entry::Occupied(entry) => {
                Some(std::mem::replace(&mut entry.into_mut().1, value))
            }
            hashbrown::hash_table::Entry::Vacant(entry) => {
                entry.insert((key, value));
                None
            }
        }
    }

    pub(super) fn remove(&mut self, key: ObjectId) -> Option<T> {
        match self
            .table
            .find_entry(get_u64_hash(&key), |(k, _)| *k == key)
        {
            Ok(entry) => Some(entry.remove().0.1),
            Err(_) => None,
        }
    }

    pub(super) fn get(&self, key: ObjectId) -> Option<&T> {
        self.table
            .find(get_u64_hash(&key), |(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    pub(super) fn get_mut(&mut self, key: ObjectId) -> Option<&mut T> {
        self.table
            .find_mut(get_u64_hash(&key), |(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    pub(super) fn entry(&mut self, key: ObjectId) -> Entry<'_, T> {
        match self.table.entry(
            get_u64_hash(&key),
            |(k, _)| *k == key,
            |(k, _)| get_u64_hash(k),
        ) {
            hashbrown::hash_table::Entry::Occupied(entry) => {
                Entry::Occupied(OccupiedEntry { entry })
            }
            hashbrown::hash_table::Entry::Vacant(entry) => {
                Entry::Vacant(VacantEntry { key, entry })
            }
        }
    }

    #[inline]
    pub(super) fn keys(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.table.iter().map(|&(k, _)| k)
    }
}

pub(super) enum Entry<'a, T> {
    Occupied(OccupiedEntry<'a, T>),
    Vacant(VacantEntry<'a, T>),
}

pub(super) struct OccupiedEntry<'a, T> {
    entry: hashbrown::hash_table::OccupiedEntry<'a, (ObjectId, T)>,
}

impl<T> OccupiedEntry<'_, T> {
    pub(super) fn get(&self) -> &T {
        &self.entry.get().1
    }
}

pub(super) struct VacantEntry<'a, T> {
    key: ObjectId,
    entry: hashbrown::hash_table::VacantEntry<'a, (ObjectId, T)>,
}

impl<T> VacantEntry<'_, T> {
    pub(super) fn insert(self, value: T) {
        self.entry.insert((self.key, value));
    }
}
