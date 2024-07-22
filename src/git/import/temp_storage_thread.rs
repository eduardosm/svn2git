use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use gix_hash::ObjectId;
use gix_object::{Object, ObjectRef};

use super::obj_map::ObjMap;
use super::temp_storage::TempStorage;
use super::ImportError;

pub(super) struct TempStorageThread {
    data: Arc<Data>,
    sender: ChannelSender,
    join: std::thread::JoinHandle<()>,
}

struct Data {
    storage: TempStorage,
    inner: Mutex<Inner>,
}

struct Inner {
    pending: ObjMap<(gix_object::Kind, Vec<u8>, Option<ObjectId>)>,
    error: Option<ImportError>,
}

impl TempStorageThread {
    pub(super) fn new(storage: TempStorage) -> Self {
        let (sender, receiver) = create_channel(16 * 1024 * 1024);

        let data = Arc::new(Data {
            storage,
            inner: Mutex::new(Inner {
                pending: ObjMap::new(),
                error: None,
            }),
        });

        let data_clone = data.clone();
        let join = std::thread::Builder::new()
            .name("temp writer".into())
            .spawn(|| {
                Self::thread_main(data_clone, receiver);
            })
            .expect("failed to spawn thread");

        Self { data, sender, join }
    }

    pub(crate) fn finish(self) -> Result<TempStorage, ImportError> {
        drop(self.sender);
        self.join.join().unwrap();

        // The thread has joined, so there should not more references to data.
        let data = Arc::into_inner(self.data).unwrap();
        let inner = data.inner.into_inner().unwrap();

        if let Some(e) = inner.error {
            return Err(e);
        }

        Ok(data.storage)
    }

    pub(crate) fn insert(
        &self,
        object: impl gix_object::WriteTo,
        hash_kind: gix_hash::Kind,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        let obj_kind = object.kind();

        let mut raw_obj = Vec::new();
        gix_object::WriteTo::write_to(&object, &mut raw_obj).unwrap();

        self.insert_raw(obj_kind, raw_obj, hash_kind, delta_base)
    }

    pub(crate) fn insert_raw(
        &self,
        obj_kind: gix_object::Kind,
        raw_obj: Vec<u8>,
        hash_kind: gix_hash::Kind,
        delta_base: Option<ObjectId>,
    ) -> Result<ObjectId, ImportError> {
        let obj_id = gix_object::compute_hash(hash_kind, obj_kind, &raw_obj);

        let mut inner = self.data.inner.lock().unwrap();

        match inner.pending.entry(obj_id) {
            super::obj_map::Entry::Occupied(_) => {}
            super::obj_map::Entry::Vacant(entry) => {
                let obj_size = raw_obj.len();

                entry.insert((obj_kind, raw_obj, delta_base));
                drop(inner);

                if !self.sender.send(obj_id, obj_size) {
                    return Err(self.data.inner.lock().unwrap().error.take().unwrap());
                }
            }
        }

        Ok(obj_id)
    }

    pub(crate) fn get(&self, obj_id: ObjectId) -> Result<Object, ImportError> {
        let (obj_kind, raw_obj) = self.get_raw(obj_id)?;

        let obj = ObjectRef::from_bytes(obj_kind, &raw_obj)
            .map_err(|_| ImportError::ParseObjectError { oid: obj_id })?;

        Ok(obj.into_owned())
    }

    pub(crate) fn get_raw(
        &self,
        obj_id: ObjectId,
    ) -> Result<(gix_object::Kind, Vec<u8>), ImportError> {
        let mut inner = self.data.inner.lock().unwrap();
        if let Some((obj_kind, raw_obj, _)) = inner.pending.get(obj_id) {
            Ok((*obj_kind, raw_obj.clone()))
        } else {
            if let Some(e) = inner.error.take() {
                return Err(e);
            }
            drop(inner);
            self.data.storage.get_raw(obj_id)
        }
    }

    fn thread_main(data: Arc<Data>, receiver: ChannelReceiver) {
        while let Some(obj_id) = receiver.recv() {
            let mut inner = data.inner.lock().unwrap();

            let (obj_kind, raw_obj, delta_base) = inner.pending.remove(obj_id).unwrap();
            if let Err(e) = data
                .storage
                .insert_raw_with_oid(obj_id, obj_kind, raw_obj, delta_base, inner)
            {
                data.inner.lock().unwrap().error = Some(e);
                break;
            }
        }
    }
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
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close(false);
        self.inner.condvar.notify_all();
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
                    .map(|sum| sum > self.inner.max_size)
                    .unwrap_or(true)
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
}

impl Drop for ChannelReceiver {
    fn drop(&mut self) {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close(true);
        self.inner.condvar.notify_all();
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
