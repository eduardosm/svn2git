use std::collections::VecDeque;
use std::io::{Read as _, Seek as _, Write as _};
use std::sync::{Arc, Condvar, Mutex};

use gix_hash::ObjectId;

use super::super::delta;
use super::ImportError;
use crate::FHashMap;

pub(super) struct TempStorage {
    data: Arc<Data>,
    sender: ChannelSender,
    join: std::thread::JoinHandle<()>,
}

struct Data {
    path: std::path::PathBuf,
    file: Mutex<std::fs::File>,
    hash_kind: gix_hash::Kind,
    large_threshold: usize,
    objs_info: Mutex<ObjsInfo>,
    condvar: Condvar,
    cache: Cache,
    error: Mutex<ErrorStatus>,
}

enum ErrorStatus {
    NoError,
    HasError(ImportError),
    ErrorTaken,
}

impl ErrorStatus {
    fn set(&mut self, error: ImportError) {
        *self = ErrorStatus::HasError(error);
    }

    fn take(&mut self) -> Option<ImportError> {
        match std::mem::replace(self, ErrorStatus::ErrorTaken) {
            ErrorStatus::NoError => {
                *self = ErrorStatus::NoError;
                None
            }
            ErrorStatus::HasError(e) => Some(e),
            ErrorStatus::ErrorTaken => {
                panic!("`TempStorage` reused after error");
            }
        }
    }
}

struct ObjsInfo {
    map: FHashMap<ObjectId, ObjInfo>,
    pending: FHashMap<ObjectId, (gix_object::Kind, Vec<u8>, Option<ObjectId>)>,
    stopped: bool,
}

#[derive(Copy, Clone)]
struct ObjInfo {
    order: usize,
    kind: gix_object::Kind,
    state: ObjState,
}

#[derive(Copy, Clone)]
enum ObjState {
    Pending,
    // The `u64` is the uncompressed size of the object data
    Stored(u64, ObjStorage),
}

#[derive(Copy, Clone)]
enum ObjStorage {
    Normal {
        offset: u64,
        delta_depth: u8,
        delta_base: Option<ObjectId>,
    },
    Large,
}

const MAIN_FILE_NAME: &str = "main";
const LARGE_DIR_NAME: &str = "large";

impl TempStorage {
    pub(super) fn create(
        path: &std::path::Path,
        hash_kind: gix_hash::Kind,
        large_threshold: usize,
        cache_size: usize,
    ) -> Result<Self, ImportError> {
        std::fs::create_dir(path).map_err(|e| ImportError::CreateDirError {
            path: path.into(),
            error: e,
        })?;

        let main_path = path.join(MAIN_FILE_NAME);
        let large_path = path.join(LARGE_DIR_NAME);

        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&main_path)
            .map_err(|e| ImportError::CreateFileError {
                path: main_path.clone(),
                error: e,
            })?;

        file.write_all(b"\0temp storage\0")
            .map_err(|e| ImportError::WriteFileError {
                path: main_path,
                error: e,
            })?;

        std::fs::create_dir(&large_path).map_err(|e| ImportError::CreateDirError {
            path: large_path.clone(),
            error: e,
        })?;

        let data = Arc::new(Data {
            path: path.into(),
            file: Mutex::new(file),
            hash_kind,
            large_threshold,
            objs_info: Mutex::new(ObjsInfo {
                map: FHashMap::default(),
                pending: FHashMap::default(),
                stopped: false,
            }),
            condvar: Condvar::new(),
            cache: Cache::new(cache_size),
            error: Mutex::new(ErrorStatus::NoError),
        });

        let (sender, receiver) = create_channel(16 * 1024 * 1024);
        let data_clone = data.clone();

        let join = std::thread::Builder::new()
            .name("temp writer".into())
            .spawn(|| {
                Self::thread_main(data_clone, receiver);
            })
            .expect("failed to spawn thread");

        Ok(Self { data, sender, join })
    }

    pub(super) fn abort(self) {
        self.sender.close(true);
        drop(self.sender);
        self.join.join().unwrap();

        // The thread has joined, so there should not more references to data.
        let data = Arc::into_inner(self.data).unwrap();

        drop(data.file);
        let _ = std::fs::remove_dir_all(&data.path);
    }

    pub(super) fn finish(self) -> Result<(), ImportError> {
        drop(self.sender);
        self.join.join().unwrap();

        // The thread has joined, so there should not more references to data.
        let mut data = Arc::into_inner(self.data).unwrap();
        drop(data.file);

        if let Some(e) = data.error.get_mut().unwrap().take() {
            let _ = std::fs::remove_dir_all(&data.path);
            return Err(e);
        }

        std::fs::remove_dir_all(&data.path).map_err(|e| ImportError::RemoveDirError {
            path: data.path,
            error: e,
        })?;

        Ok(())
    }

    pub(super) fn num_objects(&self) -> usize {
        self.data.objs_info.lock().unwrap().map.len()
    }

    pub(super) fn get_order(&self, obj_id: ObjectId) -> Option<usize> {
        let objs_info = self.data.objs_info.lock().unwrap();
        objs_info.map.get(&obj_id).map(|info| info.order)
    }

    pub(super) fn insert_raw_stream(
        &self,
        obj_kind: gix_object::Kind,
        delta_base: Option<ObjectId>,
    ) -> TempStorageWriter<'_> {
        TempStorageWriter::new(self, obj_kind, delta_base)
    }

    pub(super) fn insert_raw(
        &self,
        obj_kind: gix_object::Kind,
        raw_obj: Vec<u8>,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        let obj_id = gix_object::compute_hash(self.data.hash_kind, obj_kind, &raw_obj)
            .map_err(super::convert_hash_error)?;

        if raw_obj.len() <= self.data.large_threshold {
            self.insert_raw_normal(obj_id, obj_kind, raw_obj, delta_base)?;
            Ok(obj_id)
        } else {
            let (tmp_path, tmp_file) = self
                .data
                .get_large_tmp_file()
                .map_err(|e| ImportError::OtherIoError { error: e })?;
            let mut tmp_file = Box::new(lz4_flex::frame::FrameEncoder::new(tmp_file));

            tmp_file
                .write_all(&raw_obj)
                .map_err(|e| ImportError::WriteFileError {
                    path: tmp_path.clone(),
                    error: e,
                })?;
            tmp_file.finish().map_err(|e| ImportError::WriteFileError {
                path: tmp_path.clone(),
                error: e.into(),
            })?;

            // Large objects are excluded from delta compression
            self.data.insert_large(
                tmp_path,
                Some(obj_id),
                raw_obj.len().try_into().unwrap(),
                obj_kind,
            )
        }
    }

    fn insert_raw_normal(
        &self,
        obj_id: ObjectId,
        obj_kind: gix_object::Kind,
        raw_obj: Vec<u8>,
        delta_base: Option<ObjectId>,
    ) -> Result<(), ImportError> {
        let mut objs_info = self.data.objs_info.lock().unwrap();
        let order = objs_info.map.len();
        match objs_info.map.entry(obj_id) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let offset = match entry.get().state {
                    ObjState::Pending => None,
                    ObjState::Stored(_, ObjStorage::Normal { offset, .. }) => Some(offset),
                    ObjState::Stored(_, ObjStorage::Large) => None,
                };
                drop(objs_info);
                if let Some(offset) = offset {
                    self.data.cache.insert(offset, raw_obj);
                }
                return Ok(());
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ObjInfo {
                    order,
                    kind: obj_kind,
                    state: ObjState::Pending,
                });
            }
        }

        match objs_info.pending.entry(obj_id) {
            std::collections::hash_map::Entry::Occupied(_) => {
                // The previous insertion should have already caught this
                unreachable!();
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let obj_size = raw_obj.len();

                entry.insert((obj_kind, raw_obj, delta_base));
                drop(objs_info);

                if !self.sender.send(obj_id, obj_size) {
                    return Err(self.data.error.lock().unwrap().take().unwrap());
                }
            }
        }

        Ok(())
    }

    pub(super) fn get_raw_stream(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, u64, TempStorageReader), ImportError> {
        let objs_info = self.data.objs_info.lock().unwrap();
        if let Some((obj_kind, raw_obj, _)) = objs_info.pending.get(&obj_id) {
            let obj_size = u64::try_from(raw_obj.len()).unwrap();
            let stream = TempStorageReader::with_buf(raw_obj.clone());
            Ok((*obj_kind, obj_size, stream))
        } else {
            drop(objs_info);
            let (obj_kind, obj_size, obj_state) = self
                .data
                .get_obj_info_wait_stored(obj_id)?
                .unwrap_or_else(|| {
                    panic!("object {obj_id} not found");
                });

            let stream = self.data.get_raw_stream_from_info(obj_id, &obj_state)?;

            Ok((obj_kind, obj_size, stream))
        }
    }

    pub(super) fn get_raw(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        let objs_info = self.data.objs_info.lock().unwrap();
        if let Some((obj_kind, raw_obj, _)) = objs_info.pending.get(&obj_id) {
            Ok((*obj_kind, raw_obj.clone()))
        } else {
            drop(objs_info);
            let (obj_kind, _, obj_state) = self
                .data
                .get_obj_info_wait_stored(obj_id)?
                .unwrap_or_else(|| {
                    panic!("object {obj_id} not found");
                });

            let obj_data = self.data.get_raw_from_info(obj_id, &obj_state)?;

            Ok((obj_kind, obj_data))
        }
    }

    pub(super) fn get_raw_stream_maybe_delta(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, u64, Option<ObjectId>, TempStorageReader), ImportError> {
        let (obj_kind, obj_size, state) = self
            .data
            .get_obj_info_wait_stored(obj_id)?
            .unwrap_or_else(|| {
                panic!("object {obj_id} not found");
            });

        match state {
            ObjStorage::Normal {
                offset, delta_base, ..
            } => {
                let data =
                    read_decompress(&self.data.file.lock().unwrap(), &self.data.path, offset)?;
                let obj_md_size = u64::try_from(data.len()).unwrap();
                Ok((
                    obj_kind,
                    obj_md_size,
                    delta_base,
                    TempStorageReader::with_buf(data),
                ))
            }
            ObjStorage::Large => {
                let large_path = self.data.get_large_path(obj_id);
                let file =
                    std::fs::File::open(&large_path).map_err(|e| ImportError::ReadFileError {
                        path: large_path.clone(),
                        error: e,
                    })?;
                Ok((obj_kind, obj_size, None, TempStorageReader::with_file(file)))
            }
        }
    }

    fn thread_main(data: Arc<Data>, receiver: ChannelReceiver) {
        struct Guard<'a> {
            data: &'a Data,
        }

        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                let mut objs_info = self
                    .data
                    .objs_info
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                objs_info.stopped = true;
                self.data.condvar.notify_all();
            }
        }

        let _guard = Guard { data: &data };

        while let Some(obj_id) = receiver.recv() {
            let (obj_kind, raw_obj, delta_base) = data
                .objs_info
                .lock()
                .unwrap()
                .pending
                .remove(&obj_id)
                .unwrap();
            if let Err(e) = data.insert_raw_with_oid(obj_id, obj_kind, raw_obj, delta_base) {
                data.error.lock().unwrap().set(e);
                break;
            }
        }
    }
}

pub(super) struct TempStorageWriter<'a> {
    temp_storage: &'a TempStorage,
    obj_kind: gix_object::Kind,
    delta_base: Option<ObjectId>,
    state: TempStorageWriterState,
}

enum TempStorageWriterState {
    Buf(Vec<u8>),
    Large {
        tmp_path: std::path::PathBuf,
        tmp_file: Box<lz4_flex::frame::FrameEncoder<std::fs::File>>,
        size: u64,
    },
}

impl<'a> TempStorageWriter<'a> {
    fn new(
        temp_storage: &'a TempStorage,
        obj_kind: gix_object::Kind,
        delta_base: Option<ObjectId>,
    ) -> Self {
        Self {
            temp_storage,
            obj_kind,
            delta_base,
            state: TempStorageWriterState::Buf(Vec::new()),
        }
    }

    pub(crate) fn finish(self) -> Result<ObjectId, ImportError> {
        match self.state {
            TempStorageWriterState::Buf(buf) => {
                let obj_id =
                    gix_object::compute_hash(self.temp_storage.data.hash_kind, self.obj_kind, &buf)
                        .map_err(super::convert_hash_error)?;
                self.temp_storage
                    .insert_raw_normal(obj_id, self.obj_kind, buf, self.delta_base)?;
                Ok(obj_id)
            }
            TempStorageWriterState::Large {
                tmp_path,
                tmp_file,
                size,
            } => {
                tmp_file.finish().map_err(|e| ImportError::WriteFileError {
                    path: tmp_path.clone(),
                    error: e.into(),
                })?;

                // Large objects are excluded from delta compression
                self.temp_storage
                    .data
                    .insert_large(tmp_path, None, size, self.obj_kind)
            }
        }
    }
}

impl std::io::Write for TempStorageWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.state {
            TempStorageWriterState::Buf(ref mut write_buf) => {
                if write_buf
                    .len()
                    .checked_add(buf.len())
                    .is_some_and(|total_len| total_len <= self.temp_storage.data.large_threshold)
                {
                    write_buf.extend(buf);
                    Ok(buf.len())
                } else {
                    let (tmp_path, tmp_file) = self.temp_storage.data.get_large_tmp_file()?;
                    let mut tmp_file = Box::new(lz4_flex::frame::FrameEncoder::new(tmp_file));

                    tmp_file.write_all(write_buf)?;
                    let mut size = u64::try_from(write_buf.len()).unwrap();

                    let r = tmp_file.write(buf);
                    if let Ok(n) = r {
                        size = size.checked_add(u64::try_from(n).unwrap()).unwrap();
                    }

                    self.state = TempStorageWriterState::Large {
                        tmp_path,
                        tmp_file,
                        size,
                    };
                    r
                }
            }
            TempStorageWriterState::Large {
                ref mut tmp_file,
                ref mut size,
                ..
            } => {
                let r = tmp_file.write(buf);
                if let Ok(n) = r {
                    *size = size.checked_add(u64::try_from(n).unwrap()).unwrap();
                }
                r
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.state {
            TempStorageWriterState::Buf(_) => Ok(()),
            TempStorageWriterState::Large {
                ref mut tmp_file, ..
            } => tmp_file.flush(),
        }
    }
}

pub(super) struct TempStorageReader {
    state: TempStorageReaderMode,
}

enum TempStorageReaderMode {
    Buf {
        data: Vec<u8>,
        pos: usize,
    },
    Large {
        file: lz4_flex::frame::FrameDecoder<std::fs::File>,
    },
}

impl TempStorageReader {
    fn with_buf(buf: Vec<u8>) -> Self {
        Self {
            state: TempStorageReaderMode::Buf { data: buf, pos: 0 },
        }
    }

    fn with_file(file: std::fs::File) -> Self {
        Self {
            state: TempStorageReaderMode::Large {
                file: lz4_flex::frame::FrameDecoder::new(file),
            },
        }
    }
}

impl std::io::Read for TempStorageReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.state {
            TempStorageReaderMode::Buf {
                ref data,
                ref mut pos,
            } => {
                let rem_data = &data[*pos..];
                let n = std::cmp::min(buf.len(), rem_data.len());
                buf[..n].copy_from_slice(&rem_data[..n]);
                *pos += n;
                Ok(n)
            }
            TempStorageReaderMode::Large { ref mut file } => file.read(buf),
        }
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        match self.state {
            TempStorageReaderMode::Buf {
                ref data,
                ref mut pos,
            } => {
                let rem_data = &data[*pos..];
                buf.extend_from_slice(rem_data);
                *pos = data.len();
                Ok(rem_data.len())
            }
            TempStorageReaderMode::Large { ref mut file } => file.read_to_end(buf),
        }
    }
}

impl Data {
    fn get_large_path(&self, obj_id: ObjectId) -> std::path::PathBuf {
        let oid_str = obj_id.to_hex().to_string();
        let mut path = self.path.join(LARGE_DIR_NAME);
        path.push(&oid_str[0..2]);
        path.push(&oid_str[2..]);
        path
    }

    fn get_large_tmp_file(&self) -> Result<(std::path::PathBuf, std::fs::File), std::io::Error> {
        let base_path = self.path.join(LARGE_DIR_NAME);
        loop {
            let tmp_path = base_path.join(format!("tmp-{:08x}", rand::random::<u32>()));
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp_path)
            {
                Ok(file) => {
                    return Ok((tmp_path, file));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    continue;
                }
                Err(e) => {
                    return Err(e);
                }
            };
        }
    }

    fn get_obj_info_wait_stored(
        &self,
        obj_id: ObjectId,
    ) -> Result<Option<(gix_object::Kind, u64, ObjStorage)>, ImportError> {
        let mut objs_info = self.objs_info.lock().unwrap();
        loop {
            let Some(info) = objs_info.map.get(&obj_id) else {
                return Ok(None);
            };
            if let ObjState::Stored(obj_size, obj_storage) = info.state {
                return Ok(Some((info.kind, obj_size, obj_storage)));
            }
            if objs_info.stopped {
                // The thread has stopped, so waiting on the condvar would deadlock.
                // `error` will be `None` if the thread panicked, but in that case
                // the program is in "bug state" so panicking again is acceptable.
                return Err(self.error.lock().unwrap().take().unwrap());
            }

            objs_info = self.condvar.wait(objs_info).unwrap();
        }
    }

    fn get_obj_info_expect_stored(
        &self,
        obj_id: ObjectId,
    ) -> Option<(gix_object::Kind, ObjStorage)> {
        let objs_info = self.objs_info.lock().unwrap();
        let info = objs_info.map.get(&obj_id)?;
        let ObjState::Stored(_, obj_storage) = info.state else {
            panic!("object {obj_id} is not stored yet");
        };
        Some((info.kind, obj_storage))
    }

    fn insert_raw_with_oid(
        &self,
        obj_id: ObjectId,
        obj_kind: gix_object::Kind,
        obj_data: Vec<u8>,
        delta_base_oid: Option<ObjectId>,
    ) -> Result<(), ImportError> {
        let mut delta_data = None;
        if let Some(delta_base_oid) = delta_base_oid {
            let (delta_base_kind, delta_base_state) = self
                .get_obj_info_expect_stored(delta_base_oid)
                .unwrap_or_else(|| {
                    panic!("delta base object {delta_base_oid} not found");
                });

            assert_eq!(delta_base_kind, obj_kind, "invalid delta base kind");

            match delta_base_state {
                ObjStorage::Normal {
                    delta_depth: delta_base_delta_depth,
                    ..
                } => {
                    if delta_base_delta_depth < 50 {
                        let delta_base =
                            self.get_raw_from_info(delta_base_oid, &delta_base_state)?;

                        let delta_window_shift = 4;
                        if let Some(delta) = delta::diff(&delta_base, &obj_data, delta_window_shift)
                        {
                            debug_assert_eq!(delta::patch(&delta_base, &delta).unwrap(), obj_data);
                            delta_data = Some((delta, delta_base_oid, delta_base_delta_depth + 1));
                        }
                    }
                }
                ObjStorage::Large => {
                    // Large objects are excluded from delta compression
                }
            }
        }

        let (raw_data, delta_base, delta_depth) =
            if let Some((ref delta, delta_base, delta_depth)) = delta_data {
                (delta.as_slice(), Some(delta_base), delta_depth)
            } else {
                (obj_data.as_slice(), None, 0)
            };

        let offset = write_compress(&self.file.lock().unwrap(), &self.path, raw_data)?;

        let mut objs_info = self.objs_info.lock().unwrap();
        let info = objs_info.map.get_mut(&obj_id).unwrap();
        assert!(matches!(info.state, ObjState::Pending));
        let obj_size = u64::try_from(obj_data.len()).unwrap();
        info.state = ObjState::Stored(
            obj_size,
            ObjStorage::Normal {
                offset,
                delta_depth,
                delta_base,
            },
        );
        drop(objs_info);

        self.condvar.notify_all();

        self.cache.insert(offset, obj_data);

        Ok(())
    }

    fn insert_large(
        &self,
        tmp_path: std::path::PathBuf,
        obj_id: Option<ObjectId>,
        obj_size: u64,
        obj_kind: gix_object::Kind,
    ) -> Result<ObjectId, ImportError> {
        let obj_id = if let Some(obj_id) = obj_id {
            obj_id
        } else {
            // Re-read the whole object to compute the hash
            let tmp_file =
                std::fs::File::open(&tmp_path).map_err(|e| ImportError::ReadFileError {
                    path: tmp_path.clone(),
                    error: e,
                })?;
            let mut tmp_file = lz4_flex::frame::FrameDecoder::new(tmp_file);
            gix_object::compute_stream_hash(
                self.hash_kind,
                obj_kind,
                &mut tmp_file,
                obj_size,
                &mut gix_features::progress::Discard,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .map_err(|e| match e {
                gix_hash::io::Error::Io(e) => ImportError::ReadFileError {
                    path: tmp_path.clone(),
                    error: e,
                },
                gix_hash::io::Error::Hasher(e) => super::convert_hash_error(e),
            })?
        };

        let mut objs_info = self.objs_info.lock().unwrap();
        let order = objs_info.map.len();
        match objs_info.map.entry(obj_id) {
            std::collections::hash_map::Entry::Occupied(_) => {
                // Duplicated object, discard the temporary file
                std::fs::remove_file(&tmp_path).map_err(|e| ImportError::RemoveFileError {
                    path: tmp_path,
                    error: e,
                })?;
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                // It is important that the mutex stays locked until the file is renamed
                let final_path = self.get_large_path(obj_id);
                let parent_path = final_path.parent().unwrap();
                std::fs::create_dir_all(parent_path).map_err(|e| ImportError::CreateDirError {
                    path: parent_path.into(),
                    error: e,
                })?;
                std::fs::rename(&tmp_path, &final_path).map_err(|e| ImportError::RenameError {
                    source_path: tmp_path,
                    dest_path: final_path,
                    error: e,
                })?;
                entry.insert(ObjInfo {
                    order,
                    kind: obj_kind,
                    state: ObjState::Stored(obj_size, ObjStorage::Large),
                });
            }
        }

        Ok(obj_id)
    }

    fn get_raw_from_info(
        &self,
        obj_id: ObjectId,
        obj_state: &ObjStorage,
    ) -> Result<Vec<u8>, ImportError> {
        match *obj_state {
            ObjStorage::Normal {
                offset, delta_base, ..
            } => {
                if let Some(obj_data) = self.cache.get(offset) {
                    Ok(obj_data)
                } else {
                    let maybe_delta_data =
                        read_decompress(&self.file.lock().unwrap(), &self.path, offset)?;

                    let obj_data = if let Some(delta_base_oid) = delta_base {
                        self.resolve_delta(&maybe_delta_data, delta_base_oid)?
                    } else {
                        maybe_delta_data
                    };

                    self.cache.insert(offset, obj_data.clone());

                    Ok(obj_data)
                }
            }
            ObjStorage::Large => {
                let large_path = self.get_large_path(obj_id);
                let file =
                    std::fs::File::open(&large_path).map_err(|e| ImportError::ReadFileError {
                        path: large_path.clone(),
                        error: e,
                    })?;
                let mut decompressor = lz4_flex::frame::FrameDecoder::new(file);
                let mut data = Vec::new();
                decompressor
                    .read_to_end(&mut data)
                    .map_err(|e| ImportError::ReadFileError {
                        path: large_path.clone(),
                        error: e,
                    })?;
                Ok(data)
            }
        }
    }

    fn get_raw_stream_from_info(
        &self,
        obj_id: ObjectId,
        obj_storage: &ObjStorage,
    ) -> Result<TempStorageReader, ImportError> {
        match *obj_storage {
            ObjStorage::Normal {
                offset, delta_base, ..
            } => {
                if let Some(obj_data) = self.cache.get(offset) {
                    Ok(TempStorageReader::with_buf(obj_data))
                } else {
                    let maybe_delta_data =
                        read_decompress(&self.file.lock().unwrap(), &self.path, offset)?;

                    let obj_data = if let Some(delta_base_oid) = delta_base {
                        self.resolve_delta(&maybe_delta_data, delta_base_oid)?
                    } else {
                        maybe_delta_data
                    };

                    self.cache.insert(offset, obj_data.clone());

                    Ok(TempStorageReader::with_buf(obj_data))
                }
            }
            ObjStorage::Large => {
                let large_path = self.get_large_path(obj_id);
                let file =
                    std::fs::File::open(&large_path).map_err(|e| ImportError::ReadFileError {
                        path: large_path.clone(),
                        error: e,
                    })?;
                Ok(TempStorageReader::with_file(file))
            }
        }
    }

    fn resolve_delta(&self, delta: &[u8], imm_base_oid: ObjectId) -> Result<Vec<u8>, ImportError> {
        let mut chain = Vec::new();

        let mut cur_base_oid = imm_base_oid;
        let mut cur_data;
        loop {
            let (_, _, cur_base_state) = self.get_obj_info_wait_stored(cur_base_oid)?.unwrap();
            let (cur_base_offset, cur_base_delta_base) = match cur_base_state {
                ObjStorage::Normal {
                    offset, delta_base, ..
                } => (offset, delta_base),
                ObjStorage::Large => {
                    // Large objects are excluded from delta compression
                    unreachable!();
                }
            };
            if let Some(cur_base_data) = self.cache.get(cur_base_offset) {
                cur_data = cur_base_data;
                break;
            }

            if let Some(delta_base_oid) = cur_base_delta_base {
                chain.push(cur_base_offset);
                cur_base_oid = delta_base_oid;
            } else {
                cur_data =
                    read_decompress(&self.file.lock().unwrap(), &self.path, cur_base_offset)?;
                break;
            }
        }

        for &delta_offset in chain.iter().rev() {
            let delta_data = read_decompress(&self.file.lock().unwrap(), &self.path, delta_offset)?;

            let target_data = delta::patch(&cur_data, &delta_data).unwrap_or_else(|e| {
                panic!("failed to apply delta: {e}");
            });
            cur_data = target_data;
        }

        let final_data = delta::patch(&cur_data, delta).unwrap_or_else(|e| {
            panic!("failed to apply delta: {e}");
        });

        Ok(final_data)
    }
}

struct Cache {
    cache: Mutex<lru_mem::LruCache<u64, Vec<u8>>>,
}

impl Cache {
    fn new(size: usize) -> Self {
        Self {
            cache: Mutex::new(lru_mem::LruCache::new(size)),
        }
    }

    fn insert(&self, key: u64, value: Vec<u8>) {
        let _ = self.cache.lock().unwrap().insert(key, value);
    }

    fn get(&self, key: u64) -> Option<Vec<u8>> {
        self.cache.lock().unwrap().get(&key).cloned()
    }
}

fn write_compress(
    mut file: &std::fs::File,
    path: &std::path::Path,
    src: &[u8],
) -> Result<u64, ImportError> {
    let offset = file
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| ImportError::SeekFileError {
            path: path.to_path_buf(),
            error: e,
        })?;

    let mut compressor = lz4_flex::frame::FrameEncoder::new(file);
    compressor
        .write_all(src)
        .map_err(|e| ImportError::WriteFileError {
            path: path.to_path_buf(),
            error: e,
        })?;
    compressor
        .finish()
        .map_err(|e| ImportError::WriteFileError {
            path: path.to_path_buf(),
            error: e.into(),
        })?;

    Ok(offset)
}

fn read_decompress(
    mut file: &std::fs::File,
    path: &std::path::Path,
    offset: u64,
) -> Result<Vec<u8>, ImportError> {
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| ImportError::SeekFileError {
            path: path.to_path_buf(),
            error: e,
        })?;

    let mut data = Vec::new();

    let mut decompressor = lz4_flex::frame::FrameDecoder::new(file);
    decompressor
        .read_to_end(&mut data)
        .map_err(|e| ImportError::ReadFileError {
            path: path.to_path_buf(),
            error: e,
        })?;

    Ok(data)
}

struct ChannelSender {
    inner: Arc<ChannelInner>,
}

struct ChannelReceiver {
    inner: Arc<ChannelInner>,
}

struct ChannelInner {
    max_size: usize,
    queue: Mutex<ChannelQueue>,
    condvar: Condvar,
}

struct ChannelQueue {
    closed: bool,
    size_sum: usize,
    queue: VecDeque<(ObjectId, usize)>,
}

fn create_channel(max_size: usize) -> (ChannelSender, ChannelReceiver) {
    let inner = Arc::new(ChannelInner {
        max_size,
        queue: Mutex::new(ChannelQueue {
            closed: false,
            size_sum: 0,
            queue: VecDeque::new(),
        }),
        condvar: Condvar::new(),
    });

    let sender = ChannelSender {
        inner: inner.clone(),
    };
    let receiver = ChannelReceiver { inner };

    (sender, receiver)
}

impl ChannelInner {
    fn close(&self, clear: bool) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close(clear);
        self.condvar.notify_all();
    }
}

impl ChannelQueue {
    fn close(&mut self, clear: bool) {
        self.closed = true;
        if clear {
            self.size_sum = 0;
            self.queue.clear();
        }
    }
}

impl Drop for ChannelSender {
    fn drop(&mut self) {
        self.inner.close(false);
    }
}

impl ChannelSender {
    #[must_use]
    fn send(&self, obj_id: ObjectId, size: usize) -> bool {
        let mut queue = self.inner.queue.lock().unwrap();
        loop {
            if queue.closed {
                return false;
            }

            if !queue.queue.is_empty()
                && queue
                    .size_sum
                    .checked_add(size)
                    .is_none_or(|sum| sum > self.inner.max_size)
            {
                queue = self.inner.condvar.wait(queue).unwrap();
            } else {
                queue.queue.push_back((obj_id, size));
                queue.size_sum += size;

                if queue.queue.len() == 1 {
                    // Was empty, notify
                    self.inner.condvar.notify_all();
                }

                return true;
            }
        }
    }

    fn close(&self, clear: bool) {
        self.inner.close(clear);
    }
}

impl Drop for ChannelReceiver {
    fn drop(&mut self) {
        self.inner.close(true);
    }
}

impl ChannelReceiver {
    #[must_use]
    fn recv(&self) -> Option<ObjectId> {
        let mut queue = self.inner.queue.lock().unwrap();
        loop {
            if let Some((obj_id, size)) = queue.queue.pop_front() {
                queue.size_sum -= size;
                self.inner.condvar.notify_all();
                return Some(obj_id);
            } else if queue.closed {
                return None;
            } else {
                queue = self.inner.condvar.wait(queue).unwrap();
            }
        }
    }
}
