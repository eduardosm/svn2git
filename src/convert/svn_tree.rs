use super::bin_ser_de::{self, DeserializeError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Node {
    pub(super) metadata: gix_hash::ObjectId,
    // entries are sorted by name, but a `Vec` is faster than a `BTreeMap`
    pub(super) entries: Vec<(Vec<u8>, NodeEntry)>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum NodeEntry {
    Dir(gix_hash::ObjectId),
    File {
        special: FileSpecial,
        executable: bool,
        oid: gix_hash::ObjectId,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum FileSpecial {
    None,
    Link,
}

impl Node {
    pub(super) fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.serialize_into(&mut out);
        out
    }

    fn serialize_into(&self, out: &mut Vec<u8>) {
        if cfg!(debug_assertions) {
            for entries in self.entries.windows(2) {
                debug_assert!(
                    entries[0].0 < entries[1].0,
                    "entries must be sorted by name",
                );
            }
        }

        bin_ser_de::serialize_oid_into(&self.metadata, out);
        out.extend(self.entries.len().to_ne_bytes());
        for (name, entry) in &self.entries {
            bin_ser_de::serialize_byte_slice_into(name, out);
            entry.serialize_into(out);
        }
    }

    pub(super) fn deserialize(mut src: &[u8]) -> Result<Self, DeserializeError> {
        let r = Self::deserialize_from(&mut src)?;
        if !src.is_empty() {
            return Err(DeserializeError);
        }
        Ok(r)
    }

    fn deserialize_from(src: &mut &[u8]) -> Result<Self, DeserializeError> {
        let metadata = bin_ser_de::deserialize_oid_from(src)?;
        let entries_len = usize::from_ne_bytes(bin_ser_de::deserialize_byte_array_from(src)?);
        let mut entries = Vec::with_capacity(entries_len);
        for _ in 0..entries_len {
            let name = bin_ser_de::deserialize_byte_slice_from(src)?;
            let entry = NodeEntry::deserialize_from(src)?;
            entries.push((name, entry));
        }

        Ok(Self { metadata, entries })
    }

    pub(super) fn deserialize_only_metadata(
        mut src: &[u8],
    ) -> Result<gix_hash::ObjectId, DeserializeError> {
        bin_ser_de::deserialize_oid_from(&mut src)
    }

    pub(super) fn deserialize_find_entry(
        src: &[u8],
        entry_name: &[u8],
    ) -> Result<Option<NodeEntry>, DeserializeError> {
        let mut src = src;
        let _ = bin_ser_de::deserialize_oid_from(&mut src)?;
        let entries_len = usize::from_ne_bytes(bin_ser_de::deserialize_byte_array_from(&mut src)?);
        for _ in 0..entries_len {
            let current_name = bin_ser_de::deserialize_byte_slice_from(&mut src)?;
            let current_entry = NodeEntry::deserialize_from(&mut src)?;
            if current_name == entry_name {
                return Ok(Some(current_entry));
            }
        }
        Ok(None)
    }
}

impl NodeEntry {
    fn serialize_into(&self, out: &mut Vec<u8>) {
        match self {
            NodeEntry::Dir(oid) => {
                out.push(0);
                bin_ser_de::serialize_oid_into(oid, out);
            }
            NodeEntry::File {
                special,
                executable,
                oid,
            } => {
                out.push(1);
                out.push(match special {
                    FileSpecial::None => 0,
                    FileSpecial::Link => 1,
                });
                out.push((*executable).into());
                bin_ser_de::serialize_oid_into(oid, out);
            }
        }
    }

    fn deserialize_from(src: &mut &[u8]) -> Result<Self, DeserializeError> {
        let entry_type = bin_ser_de::deserialize_byte_from(src)?;
        match entry_type {
            0 => {
                let oid = bin_ser_de::deserialize_oid_from(src)?;
                Ok(NodeEntry::Dir(oid))
            }
            1 => {
                let special = match bin_ser_de::deserialize_byte_from(src)? {
                    0 => FileSpecial::None,
                    1 => FileSpecial::Link,
                    _ => return Err(DeserializeError),
                };
                let executable = bin_ser_de::deserialize_bool_from(src)?;
                let oid = bin_ser_de::deserialize_oid_from(src)?;
                Ok(NodeEntry::File {
                    special,
                    executable,
                    oid,
                })
            }
            _ => Err(DeserializeError),
        }
    }

    #[inline]
    pub(super) fn is_dir(&self) -> bool {
        matches!(self, NodeEntry::Dir(_))
    }
}

impl FileSpecial {
    #[inline]
    pub(super) fn is_special(&self) -> bool {
        !matches!(self, FileSpecial::None)
    }
}

#[cfg(test)]
mod tests {
    use super::{FileSpecial, Node, NodeEntry};

    #[test]
    fn serialize_and_deserialize() {
        let hash_kind = gix_hash::Kind::Sha1;
        let oid1 = gix_object::compute_hash(hash_kind, gix_object::Kind::Blob, b"obj 1").unwrap();
        let oid2 = gix_object::compute_hash(hash_kind, gix_object::Kind::Blob, b"obj 2").unwrap();
        let oid3 = gix_object::compute_hash(hash_kind, gix_object::Kind::Tree, b"obj 3").unwrap();
        let tree = Node {
            metadata: oid1,
            entries: [
                (b"dir".to_vec(), NodeEntry::Dir(oid2)),
                (
                    b"file".to_vec(),
                    NodeEntry::File {
                        special: FileSpecial::None,
                        executable: true,
                        oid: oid1,
                    },
                ),
                (
                    b"symlink".to_vec(),
                    NodeEntry::File {
                        special: FileSpecial::Link,
                        executable: false,
                        oid: oid3,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let serialized = tree.serialize();
        let deserialized = Node::deserialize(&serialized).unwrap();
        assert_eq!(deserialized, tree);
    }
}
