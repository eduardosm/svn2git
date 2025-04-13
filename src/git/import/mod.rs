use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;

use gix_hash::ObjectId;
use gix_object::tree::{EntryKind, EntryMode};
use gix_object::{Object, ObjectRef};

mod obj_map;
mod temp_storage;
mod temp_storage_thread;
mod tree_builder;

pub(crate) use tree_builder::TreeBuilder;

#[derive(Debug)]
pub(crate) enum ImportError {
    CreateFileError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    ReadFileError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    WriteFileError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    SeekFileError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    RemoveFileError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    CreateDirError {
        path: std::path::PathBuf,
        error: std::io::Error,
    },
    RenameError {
        source_path: std::path::PathBuf,
        dest_path: std::path::PathBuf,
        error: std::io::Error,
    },
    ObjectNotFound {
        id: ObjectId,
    },
    ParseObjectError {
        oid: ObjectId,
    },
    UnexpectedObjectKind {
        id: ObjectId,
        kind: gix_object::Kind,
    },
    DeltaPatchError {
        error: super::delta::PatchError,
    },
    ParentPathIsNotDir {
        path: Vec<u8>,
    },
}

impl std::error::Error for ImportError {}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::CreateFileError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to create file {path:?}: {error}")
            }
            Self::ReadFileError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to read file {path:?}: {error}")
            }
            Self::WriteFileError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to write file {path:?}: {error}")
            }
            Self::SeekFileError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to seek file {path:?}: {error}")
            }
            Self::RemoveFileError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to remove file {path:?}: {error}")
            }
            Self::CreateDirError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to create directory {path:?}: {error}")
            }
            Self::RenameError {
                ref source_path,
                ref dest_path,
                ref error,
            } => {
                write!(
                    f,
                    "failed to rename {source_path:?} to {dest_path:?}: {error}"
                )
            }
            Self::ObjectNotFound { id } => {
                write!(f, "object {id} not found")
            }
            Self::ParseObjectError { oid: id } => {
                write!(f, "failed to parse object {id}")
            }
            Self::UnexpectedObjectKind { id, kind } => {
                write!(f, "object {id} is a {kind}")
            }
            Self::DeltaPatchError { ref error } => {
                write!(f, "failed to apply delta: {error}")
            }
            Self::ParentPathIsNotDir { ref path } => {
                write!(
                    f,
                    "parent path of \"{}\" is not a directory",
                    path.escape_ascii(),
                )
            }
        }
    }
}

pub(crate) struct Importer {
    path: std::path::PathBuf,
    hash_kind: gix_hash::Kind,
    temp_storage: temp_storage_thread::TempStorageThread,
    empty_tree_oid: ObjectId,
    head_ref: String,
    refs: BTreeMap<String, ObjectId>,
}

impl Importer {
    pub(crate) fn init(path: &std::path::Path, obj_cache_size: usize) -> Result<Self, ImportError> {
        init_repo(path)?;

        let hash_kind = gix_hash::Kind::Sha1;

        let temp_storage = temp_storage::TempStorage::create(path, obj_cache_size)?;
        let temp_storage = temp_storage_thread::TempStorageThread::new(temp_storage);

        let empty_tree_oid = temp_storage.insert(gix_object::Tree::empty(), hash_kind, None)?;

        Ok(Self {
            path: path.to_path_buf(),
            hash_kind,
            temp_storage,
            empty_tree_oid,
            head_ref: "refs/heads/master".into(),
            refs: BTreeMap::new(),
        })
    }

    pub(crate) fn abort(self) {
        let _ = self.temp_storage.finish();
    }

    pub(crate) fn finish(
        self,
        mut progress_cb: impl FnMut(ImportFinishProgress),
    ) -> Result<(), ImportError> {
        let tmp_storage = self.temp_storage.finish()?;

        let seen_objects =
            gather_objects(self.refs.values().copied(), &tmp_storage, &mut progress_cb)?;

        let mut packs_dir = self.path.clone();
        packs_dir.push("objects");
        packs_dir.push("pack");

        let (pack_hash, pack_index_entires) = write_pack_data(
            &packs_dir,
            self.hash_kind,
            seen_objects.iter().copied(),
            &tmp_storage,
            &mut progress_cb,
        )?;

        tmp_storage.remove()?;

        progress_cb(ImportFinishProgress::MakeIndex);

        write_pack_index(&packs_dir, pack_hash, pack_index_entires)?;

        let head_path = self.path.join("HEAD");
        create_file_fmt(head_path, format_args!("ref: {}\n", self.head_ref))?;

        let mut packed_refs_data = Vec::<u8>::new();
        for (ref_name, ref_oid) in self.refs {
            packed_refs_data.extend(format!("{ref_oid} {ref_name}\n").as_bytes());
        }

        let packed_refs_path = self.path.join("packed-refs");
        create_file(packed_refs_path, &packed_refs_data)?;

        Ok(())
    }

    #[inline]
    pub(crate) fn empty_tree_oid(&self) -> ObjectId {
        self.empty_tree_oid
    }

    pub(crate) fn put(
        &mut self,
        object: impl gix_object::WriteTo,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        self.temp_storage.insert(object, self.hash_kind, delta_base)
    }

    pub(crate) fn put_blob(
        &mut self,
        data: Vec<u8>,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        self.temp_storage
            .insert_raw(gix_object::Kind::Blob, data, self.hash_kind, delta_base)
    }

    pub(crate) fn get_raw(&self, id: ObjectId) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        self.temp_storage.get_raw(id)
    }

    pub(crate) fn get<T: TryFrom<Object, Error = Object>>(
        &self,
        id: ObjectId,
    ) -> Result<T, ImportError> {
        let obj = self.temp_storage.get(id)?;

        T::try_from(obj).map_err(|obj| ImportError::UnexpectedObjectKind {
            id,
            kind: obj.kind(),
        })
    }

    pub(crate) fn get_blob(&self, id: ObjectId) -> Result<Vec<u8>, ImportError> {
        let (obj_kind, raw_obj) = self.temp_storage.get_raw(id)?;

        if obj_kind != gix_object::Kind::Blob {
            return Err(ImportError::UnexpectedObjectKind { id, kind: obj_kind });
        }

        Ok(raw_obj)
    }

    pub(crate) fn ls(
        &self,
        root_oid: ObjectId,
        path: &[u8],
    ) -> Result<Option<(EntryMode, ObjectId)>, ImportError> {
        if path.is_empty() {
            return Ok(Some((EntryKind::Tree.into(), root_oid)));
        }

        let mut cur_mode = EntryMode::from(EntryKind::Tree);
        let mut cur_oid = root_oid;

        for entry_name in path.split(|&c| c == b'/') {
            if !cur_mode.is_tree() {
                return Ok(None);
            }

            let (obj_kind, raw_obj) = self.get_raw(cur_oid)?;
            if obj_kind != gix_object::Kind::Tree {
                return Err(ImportError::UnexpectedObjectKind {
                    id: cur_oid,
                    kind: obj_kind,
                });
            }

            let cur_tree = gix_object::TreeRef::from_bytes(&raw_obj)
                .map_err(|_| ImportError::ParseObjectError { oid: cur_oid })?;

            if let Some(entry) = cur_tree
                .entries
                .iter()
                .find(|entry| entry.filename == entry_name)
            {
                cur_mode = entry.mode;
                cur_oid = entry.oid.into();
            } else {
                return Ok(None);
            }
        }

        Ok(Some((cur_mode, cur_oid)))
    }

    pub(crate) fn set_head(&mut self, head_ref: &str) {
        self.head_ref = head_ref.into();
    }

    pub(crate) fn set_ref(&mut self, ref_name: &str, commit_oid: ObjectId) {
        self.refs.insert(ref_name.into(), commit_oid);
    }
}

pub(crate) enum ImportFinishProgress {
    Gather(usize, usize),
    Sort(usize),
    Write(usize, usize),
    MakeIndex,
}

fn init_repo(path: &std::path::Path) -> Result<(), ImportError> {
    std::fs::create_dir(path).map_err(|e| ImportError::CreateDirError {
        path: path.to_path_buf(),
        error: e,
    })?;

    let objects_path = path.join("objects");
    create_dir(&objects_path)?;

    let objects_info_path = objects_path.join("info");
    create_dir(objects_info_path)?;

    let objects_pack_path = objects_path.join("pack");
    create_dir(objects_pack_path)?;

    let refs_path = path.join("refs");
    create_dir(&refs_path)?;

    let refs_heads_path = refs_path.join("heads");
    create_dir(refs_heads_path)?;

    let refs_tags_path = refs_path.join("tags");
    create_dir(refs_tags_path)?;

    let branches_path = path.join("branches");
    create_dir(branches_path)?;

    let hooks_path = path.join("hooks");
    create_dir(hooks_path)?;

    let info_path = path.join("info");
    create_dir(&info_path)?;

    let info_exclude_path = info_path.join("exclude");
    create_file(info_exclude_path, b"")?;

    let config_path = path.join("config");
    let config = b"[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n";
    create_file(config_path, config)?;

    Ok(())
}

fn gather_objects(
    initial_set: impl IntoIterator<Item = ObjectId>,
    tmp_storage: &temp_storage::TempStorage,
    mut cb: impl FnMut(ImportFinishProgress),
) -> Result<Vec<ObjectId>, ImportError> {
    let mut seen_objects = obj_map::ObjMap::new();
    let mut obj_queue = VecDeque::new();

    fn see(
        obj_id: ObjectId,
        seen_objects: &mut obj_map::ObjMap<()>,
        obj_queue: &mut VecDeque<ObjectId>,
    ) {
        if seen_objects.insert(obj_id, ()).is_none() {
            obj_queue.push_back(obj_id);
        }
    }

    for init_oid in initial_set {
        see(init_oid, &mut seen_objects, &mut obj_queue);
    }

    cb(ImportFinishProgress::Gather(
        seen_objects.len(),
        tmp_storage.num_objects(),
    ));

    while let Some(obj_id) = obj_queue.pop_front() {
        let (obj_kind, raw_obj) = tmp_storage.get_raw(obj_id)?;

        let obj = ObjectRef::from_bytes(obj_kind, &raw_obj)
            .map_err(|_| ImportError::ParseObjectError { oid: obj_id })?;

        let parse_hex_oid = |hex| ObjectId::from_hex(hex).unwrap();
        match obj {
            ObjectRef::Tree(tree) => {
                for entry in tree.entries.iter() {
                    match entry.mode.kind() {
                        EntryKind::Tree => {
                            see(entry.oid.to_owned(), &mut seen_objects, &mut obj_queue);
                        }
                        EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                            seen_objects.insert(entry.oid.to_owned(), ());
                        }
                        EntryKind::Commit => {}
                    }
                }
            }
            ObjectRef::Blob(_) => {}
            ObjectRef::Commit(commit) => {
                see(
                    parse_hex_oid(commit.tree),
                    &mut seen_objects,
                    &mut obj_queue,
                );

                for &parent in commit.parents.iter() {
                    see(parse_hex_oid(parent), &mut seen_objects, &mut obj_queue);
                }
            }
            ObjectRef::Tag(tag) => {
                see(parse_hex_oid(tag.target), &mut seen_objects, &mut obj_queue);
            }
        }

        cb(ImportFinishProgress::Gather(
            seen_objects.len(),
            tmp_storage.num_objects(),
        ));
    }

    cb(ImportFinishProgress::Sort(seen_objects.len()));

    let mut seen_objects = seen_objects.keys().collect::<Vec<_>>();
    seen_objects.sort_by_key(|&oid| tmp_storage.get_offset(oid).unwrap());

    Ok(seen_objects)
}

struct PackIndexEntry {
    oid: ObjectId,
    offset: u64,
    crc32: u32,
}

fn write_pack_data(
    packs_dir: &std::path::Path,
    hash_kind: gix_hash::Kind,
    seen_objects: impl ExactSizeIterator<Item = ObjectId>,
    tmp_storage: &temp_storage::TempStorage,
    mut cb: impl FnMut(ImportFinishProgress),
) -> Result<(ObjectId, Vec<PackIndexEntry>), ImportError> {
    let pack_data_version = gix_pack::data::Version::V2;

    let pack_data_tmp_path = packs_dir.join("temp_pack");
    let pack_data_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pack_data_tmp_path)
        .map_err(|e| ImportError::CreateFileError {
            path: pack_data_tmp_path.clone(),
            error: e,
        })?;
    let mut pack_data_file = gix_hash::io::Write::new(pack_data_file, hash_kind);

    let mut pack_data_offset = 0;

    let num_objects = seen_objects.len();

    let pack_data_header =
        gix_pack::data::header::encode(pack_data_version, num_objects.try_into().unwrap());

    file_write_all(&mut pack_data_file, &pack_data_tmp_path, &pack_data_header)?;
    pack_data_offset += u64::try_from(pack_data_header.len()).unwrap();

    let mut index_entries = Vec::new();
    let mut offset_map = obj_map::ObjMap::new();

    for (i, oid) in seen_objects.enumerate() {
        let entry_offset = pack_data_offset;

        let (obj_kind, delta_base_oid, mut raw_obj) = tmp_storage.get_raw_maybe_delta(oid)?;
        let header;
        if let Some(base_offset) = delta_base_oid.and_then(|base_oid| offset_map.get(base_oid)) {
            header = gix_pack::data::entry::Header::OfsDelta {
                base_distance: entry_offset - base_offset,
            };
        } else {
            if delta_base_oid.is_some() {
                (_, raw_obj) = tmp_storage.get_raw(oid)?;
            }
            header = match obj_kind {
                gix_object::Kind::Tree => gix_pack::data::entry::Header::Tree,
                gix_object::Kind::Blob => gix_pack::data::entry::Header::Blob,
                gix_object::Kind::Commit => gix_pack::data::entry::Header::Commit,
                gix_object::Kind::Tag => gix_pack::data::entry::Header::Tag,
            };
        }

        let decompressed_size = u64::try_from(raw_obj.len()).unwrap();
        let mut raw_header = Vec::new();
        header.write_to(decompressed_size, &mut raw_header).unwrap();

        let mut compressor = gix_features::zlib::stream::deflate::Write::new(Vec::new());
        compressor.write_all(&raw_obj).unwrap();
        compressor.flush().unwrap();

        let compressed = compressor.into_inner();

        file_write_all(&mut pack_data_file, &pack_data_tmp_path, &raw_header)?;
        pack_data_offset += u64::try_from(raw_header.len()).unwrap();

        file_write_all(&mut pack_data_file, &pack_data_tmp_path, &compressed)?;
        pack_data_offset += u64::try_from(compressed.len()).unwrap();

        let crc32 = 0;
        let crc32 = gix_features::hash::crc32_update(crc32, &raw_header);
        let crc32 = gix_features::hash::crc32_update(crc32, &compressed);
        index_entries.push(PackIndexEntry {
            oid,
            offset: entry_offset,
            crc32,
        });
        offset_map.insert(oid, entry_offset);

        cb(ImportFinishProgress::Write(i + 1, num_objects));
    }

    let pack_hash = pack_data_file
        .hash
        .try_finalize()
        .expect("SHA-1 collision attack detected");

    let pack_data_file = pack_data_file.inner;
    file_write_all(&pack_data_file, &pack_data_tmp_path, pack_hash.as_bytes())?;

    file_flush(pack_data_file, &pack_data_tmp_path)?;

    let pack_data_final_path = packs_dir.join(format!("pack-{pack_hash}.pack"));
    rename(pack_data_tmp_path, pack_data_final_path)?;

    Ok((pack_hash, index_entries))
}

fn write_pack_index(
    packs_dir: &std::path::Path,
    pack_hash: ObjectId,
    mut entries: Vec<PackIndexEntry>,
) -> Result<(), ImportError> {
    // V2 pack index format described in
    // https://git-scm.com/docs/pack-format#_version_2_pack_idx_files_support_packs_larger_than_4_gib_and

    entries.sort_by_key(|entry| entry.oid);

    let mut fan_out = [0u32; 256];
    for entry in entries.iter() {
        let fan_out_i = &mut fan_out[usize::from(entry.oid.as_bytes()[0])];
        *fan_out_i = fan_out_i.checked_add(1).unwrap();
    }

    let pack_index_path = packs_dir.join(format!("pack-{pack_hash}.idx"));
    let pack_index_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pack_index_path)
        .map_err(|e| ImportError::CreateFileError {
            path: pack_index_path.clone(),
            error: e,
        })?;
    let pack_index_file = std::io::BufWriter::new(pack_index_file);
    let mut pack_index_file = gix_hash::io::Write::new(pack_index_file, pack_hash.kind());

    // Pack header
    file_write_all(&mut pack_index_file, &pack_index_path, b"\xFFtOc")?;

    let index_version = 2u32;
    file_write_all(
        &mut pack_index_file,
        &pack_index_path,
        &index_version.to_be_bytes(),
    )?;

    // Fan-out table
    let mut fan_out_acc = 0u32;
    for &fan_out_i in fan_out.iter() {
        fan_out_acc = fan_out_acc.checked_add(fan_out_i).unwrap();
        file_write_all(
            &mut pack_index_file,
            &pack_index_path,
            &fan_out_acc.to_be_bytes(),
        )?;
    }

    // Object hash table
    for entry in entries.iter() {
        file_write_all(&mut pack_index_file, &pack_index_path, entry.oid.as_bytes())?;
    }

    // CRC32 table
    for entry in entries.iter() {
        file_write_all(
            &mut pack_index_file,
            &pack_index_path,
            &entry.crc32.to_be_bytes(),
        )?;
    }

    // 4-byte offsets
    let mut num_8byte_offsets = 0i32;
    for entry in entries.iter() {
        let value = if let Ok(offset) = i32::try_from(entry.offset) {
            offset as u32
        } else {
            let value = num_8byte_offsets as u32 | 0x8000_0000;
            num_8byte_offsets = num_8byte_offsets.checked_add(1).unwrap();
            value
        };
        file_write_all(&mut pack_index_file, &pack_index_path, &value.to_be_bytes())?;
    }

    // 8-byte offsets
    for entry in entries.iter() {
        if i32::try_from(entry.offset).is_err() {
            file_write_all(
                &mut pack_index_file,
                &pack_index_path,
                &entry.offset.to_be_bytes(),
            )?;
        }
    }

    // Pack checksum
    file_write_all(&mut pack_index_file, &pack_index_path, pack_hash.as_bytes())?;

    // Index checksum
    let index_hash = pack_index_file
        .hash
        .try_finalize()
        .expect("SHA-1 collision attack detected");
    let mut pack_index_file = pack_index_file.inner;

    file_write_all(
        &mut pack_index_file,
        &pack_index_path,
        index_hash.as_bytes(),
    )?;

    file_flush(pack_index_file, &pack_index_path)?;

    Ok(())
}

fn create_dir<P>(path: P) -> Result<(), ImportError>
where
    P: AsRef<std::path::Path> + Into<std::path::PathBuf>,
{
    std::fs::create_dir(path.as_ref()).map_err(|e| ImportError::CreateDirError {
        path: path.into(),
        error: e,
    })
}

fn create_file<P>(path: P, data: &[u8]) -> Result<(), ImportError>
where
    P: AsRef<std::path::Path> + Into<std::path::PathBuf>,
{
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path.as_ref())
        .and_then(|mut file| {
            file.write_all(data)?;
            file.flush()?;
            Ok(())
        })
        .map_err(|e| ImportError::CreateFileError {
            path: path.into(),
            error: e,
        })
}

fn create_file_fmt<P>(path: P, data: impl std::fmt::Display) -> Result<(), ImportError>
where
    P: AsRef<std::path::Path> + Into<std::path::PathBuf>,
{
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path.as_ref())
        .and_then(|mut file| {
            write!(file, "{data}")?;
            file.flush()?;
            Ok(())
        })
        .map_err(|e| ImportError::CreateFileError {
            path: path.into(),
            error: e,
        })
}

fn rename<P, Q>(from: P, to: Q) -> Result<(), ImportError>
where
    P: AsRef<std::path::Path> + Into<std::path::PathBuf>,
    Q: AsRef<std::path::Path> + Into<std::path::PathBuf>,
{
    std::fs::rename(from.as_ref(), to.as_ref()).map_err(|e| ImportError::RenameError {
        source_path: from.into(),
        dest_path: to.into(),
        error: e,
    })
}

#[inline]
fn file_write_all(
    mut w: impl std::io::Write,
    path: &std::path::Path,
    data: &[u8],
) -> Result<(), ImportError> {
    w.write_all(data).map_err(|e| ImportError::WriteFileError {
        path: path.to_path_buf(),
        error: e,
    })
}

#[inline]
fn file_flush(mut w: impl std::io::Write, path: &std::path::Path) -> Result<(), ImportError> {
    w.flush().map_err(|e| ImportError::WriteFileError {
        path: path.to_path_buf(),
        error: e,
    })
}
