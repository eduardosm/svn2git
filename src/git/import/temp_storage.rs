use std::io::{Read as _, Seek as _, Write as _};
use std::sync::{Condvar, Mutex};

use gix_hash::ObjectId;

use super::super::delta;
use super::ImportError;
use crate::FHashMap;

pub(super) struct TempStorage {
    path: std::path::PathBuf,
    file: Mutex<std::fs::File>,
    info: ObjsInfo,
    cache: Cache,
}

impl TempStorage {
    pub(super) fn create(
        dir_path: &std::path::Path,
        cache_size: usize,
    ) -> Result<Self, ImportError> {
        let path = dir_path.join("temp_storage");

        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| ImportError::CreateFileError {
                path: path.clone(),
                error: e,
            })?;

        file.write_all(b"\0temp storage\0")
            .map_err(|e| ImportError::WriteFileError {
                path: path.clone(),
                error: e,
            })?;

        Ok(Self {
            path,
            file: Mutex::new(file),
            info: ObjsInfo::new(),
            cache: Cache::new(cache_size),
        })
    }

    pub(super) fn remove(self) -> Result<(), ImportError> {
        drop(self.file);
        std::fs::remove_file(&self.path).map_err(|e| ImportError::RemoveFileError {
            path: self.path,
            error: e,
        })
    }

    pub(super) fn num_objects(&self) -> usize {
        self.info.num_objects()
    }

    pub(super) fn get_offset(&self, obj_id: ObjectId) -> Option<u64> {
        self.info.with_info(obj_id, |info| info.offset)
    }

    pub(super) fn insert_raw_with_oid<T>(
        &self,
        obj_id: ObjectId,
        obj_kind: gix_object::Kind,
        obj_data: Vec<u8>,
        delta_base_oid: Option<ObjectId>,
        drop_after_pre_insert: T,
    ) -> Result<(), ImportError> {
        if let Some(offset) = self.info.pre_insert(obj_id, obj_kind) {
            self.cache.insert(offset, obj_data);
            return Ok(());
        }

        drop(drop_after_pre_insert);

        let mut delta_data = None;
        if let Some(delta_base_oid) = delta_base_oid {
            let (delta_base_info, delta_base) =
                self.get_raw_with_info(delta_base_oid, |&info| info)?;

            if delta_base_info.kind != obj_kind {
                return Err(ImportError::UnexpectedObjectKind {
                    id: delta_base_oid,
                    kind: delta_base_info.kind,
                });
            }

            if delta_base_info.delta_depth < 50 {
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

        let offset = write_compress(&self.file.lock().unwrap(), &self.path, raw_data)?;

        self.info
            .finish_insert(obj_id, offset, delta_depth, delta_base);
        self.cache.insert(offset, obj_data);

        Ok(())
    }

    pub(super) fn get_raw(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        self.get_raw_with_info(obj_id, |info| info.kind)
    }

    fn get_raw_with_info<T>(
        &self,
        obj_id: ObjectId,
        f: impl FnOnce(&ObjInfo) -> T,
    ) -> Result<(T, Vec<u8>), ImportError> {
        let info = self
            .info
            .get(obj_id)
            .ok_or(ImportError::ObjectNotFound { id: obj_id })?;

        let obj_data = if let Some(obj_data) = self.cache.get(info.offset) {
            obj_data
        } else {
            let maybe_delta_data =
                read_decompress(&self.file.lock().unwrap(), &self.path, info.offset)?;

            let obj_data = if let Some(delta_base_oid) = info.delta_base {
                self.resolve_delta(&maybe_delta_data, delta_base_oid)?
            } else {
                maybe_delta_data
            };

            self.cache.insert(info.offset, obj_data.clone());

            obj_data
        };

        Ok((f(&info), obj_data))
    }

    pub(super) fn get_raw_maybe_delta(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Option<ObjectId>, Vec<u8>), ImportError> {
        let info = self
            .info
            .get(obj_id)
            .ok_or(ImportError::ObjectNotFound { id: obj_id })?;

        let data = read_decompress(&self.file.lock().unwrap(), &self.path, info.offset)?;

        Ok((info.kind, info.delta_base, data))
    }

    fn resolve_delta(&self, delta: &[u8], imm_base_oid: ObjectId) -> Result<Vec<u8>, ImportError> {
        let mut chain = Vec::new();

        let mut cur_base_oid = imm_base_oid;
        let mut cur_data;
        loop {
            let cur_base_info = self.info.get(cur_base_oid).unwrap();
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

            let target_data = delta::patch(&cur_data, &delta_data)
                .map_err(|e| ImportError::DeltaPatchError { error: e })?;
            cur_data = target_data;
        }

        let final_data = delta::patch(&cur_data, delta)
            .map_err(|e| ImportError::DeltaPatchError { error: e })?;

        Ok(final_data)
    }
}

struct ObjsInfo {
    map: Mutex<FHashMap<ObjectId, ObjInfo>>,
    condvar: Condvar,
}

#[derive(Copy, Clone)]
struct ObjInfo {
    offset: u64,
    kind: gix_object::Kind,
    delta_depth: u8,
    delta_base: Option<ObjectId>,
}

impl ObjsInfo {
    fn new() -> Self {
        Self {
            map: Mutex::new(FHashMap::default()),
            condvar: Condvar::new(),
        }
    }

    fn num_objects(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    fn pre_insert(&self, obj_id: ObjectId, kind: gix_object::Kind) -> Option<u64> {
        match self.map.lock().unwrap().entry(obj_id) {
            std::collections::hash_map::Entry::Occupied(entry) => Some(entry.get().offset),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(ObjInfo {
                    offset: u64::MAX,
                    kind,
                    delta_depth: u8::MAX,
                    delta_base: None,
                });
                None
            }
        }
    }

    fn finish_insert(
        &self,
        obj_id: ObjectId,
        offset: u64,
        delta_depth: u8,
        delta_base: Option<ObjectId>,
    ) {
        let mut map = self.map.lock().unwrap();
        let info = map.get_mut(&obj_id).unwrap();
        assert_eq!(info.offset, u64::MAX);
        info.offset = offset;
        info.delta_depth = delta_depth;
        info.delta_base = delta_base;

        self.condvar.notify_all();
    }

    fn with_info<R>(&self, obj_id: ObjectId, f: impl FnOnce(&ObjInfo) -> R) -> Option<R> {
        let mut map = self.map.lock().unwrap();
        loop {
            let info = map.get(&obj_id)?;
            if info.offset != u64::MAX {
                return Some(f(info));
            }

            map = self.condvar.wait(map).unwrap();
        }
    }

    fn get(&self, obj_id: ObjectId) -> Option<ObjInfo> {
        self.with_info(obj_id, |&info| info)
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
