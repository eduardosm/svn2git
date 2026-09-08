use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;

use gix_hash::ObjectId;
use gix_object::ObjectRef;
use gix_object::tree::EntryKind;

use crate::{FHashMap, FHashSet};

mod change_set;
mod temp_storage;

pub(crate) use change_set::ChangeSet;

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
    RemoveDirError {
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
    OtherIoError {
        error: std::io::Error,
    },
    Sha1Collision {
        hash: ObjectId,
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
            Self::RemoveDirError {
                ref path,
                ref error,
            } => {
                write!(f, "failed to remove directory {path:?}: {error}")
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
            Self::OtherIoError { ref error } => {
                write!(f, "{error}")
            }
            Self::Sha1Collision { hash } => {
                write!(f, "SHA-1 collision attack with hash {hash}")
            }
        }
    }
}

fn convert_hash_error(error: gix_hash::hasher::Error) -> ImportError {
    match error {
        gix_hash::hasher::Error::CollisionAttack { digest } => {
            ImportError::Sha1Collision { hash: digest }
        }
    }
}

pub(crate) struct Importer {
    path: std::path::PathBuf,
    hash_kind: gix_hash::Kind,
    temp_storage: temp_storage::TempStorage,
    empty_tree_oid: ObjectId,
    head_ref: String,
    refs: BTreeMap<String, ObjectId>,
}

impl Importer {
    pub(crate) fn init(
        path: &std::path::Path,
        hash_kind: gix_hash::Kind,
        large_obj_threshold: usize,
        obj_cache_size: usize,
    ) -> Result<Self, ImportError> {
        init_repo(path, hash_kind)?;

        let temp_storage = temp_storage::TempStorage::create(
            &path.join("temp_storage"),
            hash_kind,
            large_obj_threshold,
            obj_cache_size,
        )?;

        let empty_tree_oid = Self::put_inner(gix_object::Tree::empty(), None, &temp_storage)?;

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
        self.temp_storage.abort();
    }

    pub(crate) fn finish(
        self,
        mut progress_cb: impl FnMut(ImportFinishProgress),
    ) -> Result<(), ImportError> {
        let seen_objects = match gather_objects(
            self.refs.values().copied(),
            &self.temp_storage,
            self.hash_kind,
            &mut progress_cb,
        ) {
            Ok(seen_objects) => seen_objects,
            Err(e) => {
                self.temp_storage.abort();
                return Err(e);
            }
        };

        let mut packs_dir = self.path.clone();
        packs_dir.push("objects");
        packs_dir.push("pack");

        let (pack_hash, pack_index_entries) = match write_pack_data(
            &packs_dir,
            self.hash_kind,
            seen_objects.iter().copied(),
            &self.temp_storage,
            &mut progress_cb,
        ) {
            Ok((pack_hash, pack_index_entries)) => (pack_hash, pack_index_entries),
            Err(e) => {
                self.temp_storage.abort();
                return Err(e);
            }
        };

        progress_cb(ImportFinishProgress::MakeIndex);

        self.temp_storage.finish()?;

        write_pack_index(&packs_dir, pack_hash, pack_index_entries)?;

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
        &self,
        object: impl gix_object::WriteTo,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        Self::put_inner(object, delta_base, &self.temp_storage)
    }

    fn put_inner(
        object: impl gix_object::WriteTo,
        delta_base: Option<ObjectId>,
        temp_storage: &temp_storage::TempStorage,
    ) -> Result<ObjectId, ImportError> {
        let obj_kind = object.kind();

        let mut obj_writer = temp_storage.insert_raw_stream(obj_kind, delta_base);
        gix_object::WriteTo::write_to(&object, &mut obj_writer)
            .map_err(|e| ImportError::OtherIoError { error: e })?;

        let obj_id = obj_writer.finish()?;

        Ok(obj_id)
    }

    pub(crate) fn put_blob_stream(&self, delta_base: Option<ObjectId>) -> ObjectWriter<'_> {
        let obj_writer = self
            .temp_storage
            .insert_raw_stream(gix_object::Kind::Blob, delta_base);
        ObjectWriter { writer: obj_writer }
    }

    pub(crate) fn put_blob(
        &mut self,
        data: Vec<u8>,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        self.temp_storage
            .insert_raw(gix_object::Kind::Blob, data, delta_base)
    }

    pub(crate) fn get_raw(&self, id: ObjectId) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        self.temp_storage.get_raw(id)
    }

    pub(crate) fn get_blob_stream(&self, id: ObjectId) -> Result<ObjectReader, ImportError> {
        let (obj_kind, _, reader) = self.temp_storage.get_raw_stream(id)?;
        assert_eq!(
            obj_kind,
            gix_object::Kind::Blob,
            "unexpected object kind for {id}",
        );
        Ok(ObjectReader { reader })
    }

    pub(crate) fn get_blob(&self, id: ObjectId) -> Result<Vec<u8>, ImportError> {
        let (obj_kind, raw_obj) = self.temp_storage.get_raw(id)?;
        assert_eq!(
            obj_kind,
            gix_object::Kind::Blob,
            "unexpected object kind for {id}",
        );

        Ok(raw_obj)
    }

    pub(crate) fn ls(
        &self,
        root_oid: ObjectId,
        path: &[u8],
    ) -> Result<Option<(EntryKind, ObjectId)>, ImportError> {
        if path.is_empty() {
            return Ok(Some((EntryKind::Tree, root_oid)));
        }

        let mut cur_kind = EntryKind::Tree;
        let mut cur_oid = root_oid;

        for entry_name in path.split(|&c| c == b'/') {
            if cur_kind != EntryKind::Tree {
                return Ok(None);
            }

            let (obj_kind, raw_obj) = self.get_raw(cur_oid)?;
            assert_eq!(
                obj_kind,
                gix_object::Kind::Tree,
                "unexpected object kind for {cur_oid}",
            );

            let cur_tree = gix_object::TreeRef::from_bytes(&raw_obj, self.hash_kind)
                .unwrap_or_else(|_| {
                    panic!("failed to parse object {cur_oid}");
                });

            if let Some(entry) = cur_tree
                .entries
                .iter()
                .find(|entry| entry.filename == entry_name)
            {
                cur_kind = entry.mode.kind();
                cur_oid = entry.oid.into();
            } else {
                return Ok(None);
            }
        }

        Ok(Some((cur_kind, cur_oid)))
    }

    pub(crate) fn set_head(&mut self, head_ref: &str) {
        self.head_ref = head_ref.into();
    }

    pub(crate) fn set_ref(&mut self, ref_name: &str, commit_oid: ObjectId) {
        self.refs.insert(ref_name.into(), commit_oid);
    }
}

pub(crate) struct ObjectWriter<'a> {
    writer: temp_storage::TempStorageWriter<'a>,
}

impl ObjectWriter<'_> {
    pub(crate) fn finish(self) -> Result<ObjectId, ImportError> {
        self.writer.finish()
    }
}

impl std::io::Write for ObjectWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

pub(crate) struct ObjectReader {
    reader: temp_storage::TempStorageReader,
}

impl std::io::Read for ObjectReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        self.reader.read_to_end(buf)
    }
}

pub(crate) enum ImportFinishProgress {
    Gather(usize, usize),
    Sort(usize),
    Write(usize, usize),
    MakeIndex,
}

fn init_repo(path: &std::path::Path, hash_kind: gix_hash::Kind) -> Result<(), ImportError> {
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
    let mut config = Vec::new();
    config.extend(b"[core]\n");
    match hash_kind {
        gix_hash::Kind::Sha1 => {
            config.extend(b"\trepositoryformatversion = 0\n");
        }
        gix_hash::Kind::Sha256 => {
            config.extend(b"\trepositoryformatversion = 1\n");
        }
        _ => unreachable!(),
    }
    config.extend(b"\tfilemode = true\n\tbare = true\n");
    match hash_kind {
        gix_hash::Kind::Sha1 => {}
        gix_hash::Kind::Sha256 => {
            config.extend(b"[extensions]\n\tobjectformat = sha256\n");
        }
        _ => unreachable!(),
    }
    create_file(config_path, &config)?;

    Ok(())
}

fn gather_objects(
    initial_set: impl IntoIterator<Item = ObjectId>,
    temp_storage: &temp_storage::TempStorage,
    hash_kind: gix_hash::Kind,
    mut cb: impl FnMut(ImportFinishProgress),
) -> Result<Vec<ObjectId>, ImportError> {
    let total_num_objects = temp_storage.num_objects();

    struct Gatherer {
        seen_objects_set: FHashSet<ObjectId>,
        seen_objects_vec: Vec<(usize, ObjectId)>,
        obj_queue: VecDeque<ObjectId>,
    }

    let mut gatherer = Gatherer {
        seen_objects_set: FHashSet::default(),
        seen_objects_vec: Vec::new(),
        obj_queue: VecDeque::new(),
    };

    impl Gatherer {
        fn see(
            &mut self,
            obj_id: ObjectId,
            enqueue: bool,
            temp_storage: &temp_storage::TempStorage,
        ) {
            if self.seen_objects_set.insert(obj_id) {
                self.seen_objects_vec
                    .push((temp_storage.get_order(obj_id).unwrap(), obj_id));
                if enqueue {
                    self.obj_queue.push_back(obj_id);
                }
            }
        }
    }

    for init_oid in initial_set {
        gatherer.see(init_oid, true, temp_storage);
    }

    cb(ImportFinishProgress::Gather(
        gatherer.seen_objects_set.len(),
        total_num_objects,
    ));

    while let Some(obj_id) = gatherer.obj_queue.pop_front() {
        let (obj_kind, raw_obj) = temp_storage.get_raw(obj_id)?;

        let obj = ObjectRef::from_bytes(&raw_obj, obj_kind, hash_kind).unwrap_or_else(|_| {
            panic!("failed to parse object {obj_id}");
        });

        let parse_hex_oid = |hex| ObjectId::from_hex(hex).unwrap();
        match obj {
            ObjectRef::Tree(tree) => {
                for entry in tree.entries.iter() {
                    match entry.mode.kind() {
                        EntryKind::Tree => {
                            gatherer.see(entry.oid.to_owned(), true, temp_storage);
                        }
                        EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                            gatherer.see(entry.oid.to_owned(), false, temp_storage);
                        }
                        EntryKind::Commit => {}
                    }
                }
            }
            ObjectRef::Blob(_) => unreachable!(), // blobs are never added to the queue
            ObjectRef::Commit(commit) => {
                gatherer.see(parse_hex_oid(commit.tree), true, temp_storage);

                for &parent in commit.parents.iter() {
                    gatherer.see(parse_hex_oid(parent), true, temp_storage);
                }
            }
            ObjectRef::Tag(tag) => {
                gatherer.see(parse_hex_oid(tag.target), true, temp_storage);
            }
        }

        cb(ImportFinishProgress::Gather(
            gatherer.seen_objects_set.len(),
            total_num_objects,
        ));
    }

    cb(ImportFinishProgress::Sort(gatherer.seen_objects_set.len()));

    gatherer
        .seen_objects_vec
        .sort_unstable_by_key(|&(order, _)| order);

    Ok(gatherer
        .seen_objects_vec
        .iter()
        .map(|&(_, oid)| oid)
        .collect())
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
    temp_storage: &temp_storage::TempStorage,
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
    let mut offset_map = FHashMap::default();

    for (i, oid) in seen_objects.enumerate() {
        let entry_offset = pack_data_offset;

        let (obj_kind, mut obj_md_size, delta_base_oid, mut raw_stream) =
            temp_storage.get_raw_stream_maybe_delta(oid)?;
        let header;
        if let Some(base_offset) = delta_base_oid.and_then(|base_oid| offset_map.get(&base_oid)) {
            header = gix_pack::data::entry::Header::OfsDelta {
                base_distance: entry_offset - base_offset,
            };
        } else {
            if delta_base_oid.is_some() {
                (_, obj_md_size, raw_stream) = temp_storage.get_raw_stream(oid)?;
            }
            header = match obj_kind {
                gix_object::Kind::Tree => gix_pack::data::entry::Header::Tree,
                gix_object::Kind::Blob => gix_pack::data::entry::Header::Blob,
                gix_object::Kind::Commit => gix_pack::data::entry::Header::Commit,
                gix_object::Kind::Tag => gix_pack::data::entry::Header::Tag,
            };
        }

        let mut crc32_stream = Crc32Stream::new(&mut pack_data_file);

        header
            .write_to(obj_md_size, &mut crc32_stream)
            .map_err(|e| ImportError::WriteFileError {
                path: pack_data_tmp_path.clone(),
                error: e,
            })?;

        let mut compressor = gix_zlib::stream::deflate::Write::new(
            &mut crc32_stream,
            gix_zlib::Compression::DEFAULT,
        );
        std::io::copy(&mut raw_stream, &mut compressor)
            .map_err(|e| ImportError::OtherIoError { error: e })?;
        compressor
            .flush()
            .map_err(|e| ImportError::WriteFileError {
                path: pack_data_tmp_path.clone(),
                error: e,
            })?;

        let (crc32, entry_len) = crc32_stream.finish();
        pack_data_offset += entry_len;

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
        .map_err(convert_hash_error)?;

    let pack_data_file = pack_data_file.inner;
    file_write_all(&pack_data_file, &pack_data_tmp_path, pack_hash.as_bytes())?;

    file_flush(pack_data_file, &pack_data_tmp_path)?;

    let pack_data_final_path = packs_dir.join(format!("pack-{pack_hash}.pack"));
    rename(pack_data_tmp_path, pack_data_final_path)?;

    Ok((pack_hash, index_entries))
}

struct Crc32Stream<T: std::io::Write> {
    crc32: u32,
    len: u64,
    inner: T,
}

impl<T: std::io::Write> Crc32Stream<T> {
    #[inline]
    fn new(inner: T) -> Self {
        Self {
            crc32: 0,
            len: 0,
            inner,
        }
    }

    #[inline]
    fn finish(self) -> (u32, u64) {
        (self.crc32, self.len)
    }
}

impl<T: std::io::Write> std::io::Write for Crc32Stream<T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc32 = gix_features::hash::crc32_update(self.crc32, &buf[..n]);
        self.len += u64::try_from(n).unwrap();
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_pack_index(
    packs_dir: &std::path::Path,
    pack_hash: ObjectId,
    mut entries: Vec<PackIndexEntry>,
) -> Result<(), ImportError> {
    // V2 pack index format described in
    // https://git-scm.com/docs/pack-format#_version_2_pack_idx_files_support_packs_larger_than_4_gib_and

    entries.sort_unstable_by_key(|entry| entry.oid);

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
        .map_err(convert_hash_error)?;
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
