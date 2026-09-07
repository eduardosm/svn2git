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
    offset: u64,
    kind: gix_object::Kind,
    delta_depth: u8,
    delta_base: Option<ObjectId>,
}

impl TempStorage {
    pub(super) fn create(path: &std::path::Path, cache_size: usize) -> Result<Self, ImportError> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| ImportError::CreateFileError {
                path: path.into(),
                error: e,
            })?;

        file.write_all(b"\0temp storage\0")
            .map_err(|e| ImportError::WriteFileError {
                path: path.into(),
                error: e,
            })?;

        let data = Arc::new(Data {
            path: path.into(),
            file: Mutex::new(file),
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
        let _ = std::fs::remove_file(&data.path);
    }

    pub(super) fn finish(self) -> Result<(), ImportError> {
        drop(self.sender);
        self.join.join().unwrap();

        // The thread has joined, so there should not more references to data.
        let mut data = Arc::into_inner(self.data).unwrap();
        drop(data.file);

        if let Some(e) = data.error.get_mut().unwrap().take() {
            let _ = std::fs::remove_file(&data.path);
            return Err(e);
        }

        std::fs::remove_file(&data.path).map_err(|e| ImportError::RemoveFileError {
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

    pub(super) fn insert_raw(
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
                let offset = entry.get().offset;
                drop(objs_info);
                if offset != u64::MAX {
                    self.data.cache.insert(offset, raw_obj);
                }
                return Ok(());
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ObjInfo {
                    order,
                    offset: u64::MAX,
                    kind: obj_kind,
                    delta_depth: u8::MAX,
                    delta_base: None,
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

    pub(super) fn get_raw(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        self.data.get_raw(obj_id)
    }

    pub(super) fn get_raw_maybe_delta(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Option<ObjectId>, Vec<u8>), ImportError> {
        self.data.get_raw_maybe_delta(obj_id)
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
            if let Err(e) = Self::insert_raw_with_oid(&data, obj_id, obj_kind, raw_obj, delta_base)
            {
                data.error.lock().unwrap().set(e);
                break;
            }
        }
    }

    fn insert_raw_with_oid(
        data: &Data,
        obj_id: ObjectId,
        obj_kind: gix_object::Kind,
        obj_data: Vec<u8>,
        delta_base_oid: Option<ObjectId>,
    ) -> Result<(), ImportError> {
        let mut delta_data = None;
        if let Some(delta_base_oid) = delta_base_oid {
            let delta_base_info = data
                .get_finished_obj_info(delta_base_oid)
                .unwrap_or_else(|| {
                    panic!("delta base object {delta_base_oid} not found");
                });

            assert_eq!(delta_base_info.kind, obj_kind, "invalid delta base kind");

            if delta_base_info.delta_depth < 50 {
                let delta_base = data.get_raw_from_info(&delta_base_info)?;

                let delta_window_shift = 4;
                if let Some(delta) = delta::diff(&delta_base, &obj_data, delta_window_shift) {
                    debug_assert_eq!(delta::patch(&delta_base, &delta).unwrap(), obj_data);
                    delta_data = Some((delta, delta_base_oid, delta_base_info.delta_depth + 1));
                }
            }
        }

        let (raw_data, delta_base, delta_depth) =
            if let Some((ref delta, delta_base, delta_depth)) = delta_data {
                (delta.as_slice(), Some(delta_base), delta_depth)
            } else {
                (obj_data.as_slice(), None, 0)
            };

        let offset = write_compress(&data.file.lock().unwrap(), &data.path, raw_data)?;

        let mut objs_info = data.objs_info.lock().unwrap();
        let info = objs_info.map.get_mut(&obj_id).unwrap();
        assert_eq!(info.offset, u64::MAX);
        info.offset = offset;
        info.delta_depth = delta_depth;
        info.delta_base = delta_base;
        drop(objs_info);

        data.condvar.notify_all();

        data.cache.insert(offset, obj_data);

        Ok(())
    }
}

impl Data {
    fn with_obj_info<R>(
        &self,
        obj_id: ObjectId,
        f: impl FnOnce(&ObjInfo) -> R,
    ) -> Result<Option<R>, ImportError> {
        let mut objs_info = self.objs_info.lock().unwrap();
        loop {
            let Some(info) = objs_info.map.get(&obj_id) else {
                return Ok(None);
            };
            if info.offset != u64::MAX {
                return Ok(Some(f(info)));
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

    fn get_obj_info(&self, obj_id: ObjectId) -> Result<Option<ObjInfo>, ImportError> {
        self.with_obj_info(obj_id, |&info| info)
    }

    fn get_finished_obj_info(&self, obj_id: ObjectId) -> Option<ObjInfo> {
        let objs_info = self.objs_info.lock().unwrap();
        let info = objs_info.map.get(&obj_id)?;
        if info.offset == u64::MAX {
            panic!("object {obj_id} is not finished yet");
        }

        Some(*info)
    }

    fn get_raw(&self, obj_id: ObjectId) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        let objs_info = self.objs_info.lock().unwrap();
        if let Some((obj_kind, raw_obj, _)) = objs_info.pending.get(&obj_id) {
            Ok((*obj_kind, raw_obj.clone()))
        } else {
            drop(objs_info);
            let obj_info = self.get_obj_info(obj_id)?.unwrap_or_else(|| {
                panic!("object {obj_id} not found");
            });

            let obj_data = self.get_raw_from_info(&obj_info)?;

            Ok((obj_info.kind, obj_data))
        }
    }

    fn get_raw_maybe_delta(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Option<ObjectId>, Vec<u8>), ImportError> {
        let info = self.get_obj_info(obj_id)?.unwrap_or_else(|| {
            panic!("object {obj_id} not found");
        });

        let data = read_decompress(&self.file.lock().unwrap(), &self.path, info.offset)?;

        Ok((info.kind, info.delta_base, data))
    }

    fn get_raw_from_info(&self, obj_info: &ObjInfo) -> Result<Vec<u8>, ImportError> {
        let obj_data = if let Some(obj_data) = self.cache.get(obj_info.offset) {
            obj_data
        } else {
            let maybe_delta_data =
                read_decompress(&self.file.lock().unwrap(), &self.path, obj_info.offset)?;

            let obj_data = if let Some(delta_base_oid) = obj_info.delta_base {
                self.resolve_delta(&maybe_delta_data, delta_base_oid)?
            } else {
                maybe_delta_data
            };

            self.cache.insert(obj_info.offset, obj_data.clone());

            obj_data
        };

        Ok(obj_data)
    }

    fn resolve_delta(&self, delta: &[u8], imm_base_oid: ObjectId) -> Result<Vec<u8>, ImportError> {
        let mut chain = Vec::new();

        let mut cur_base_oid = imm_base_oid;
        let mut cur_data;
        loop {
            let cur_base_info = self.get_obj_info(cur_base_oid)?.unwrap();
            if let Some(cur_base_data) = self.cache.get(cur_base_info.offset) {
                cur_data = cur_base_data;
                break;
            }

            if let Some(delta_base_oid) = cur_base_info.delta_base {
                chain.push(cur_base_info.offset);
                cur_base_oid = delta_base_oid;
            } else {
                cur_data =
                    read_decompress(&self.file.lock().unwrap(), &self.path, cur_base_info.offset)?;
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
