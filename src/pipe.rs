use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

pub(crate) struct PipeReader {
    inner: Arc<Inner>,
}

pub(crate) struct PipeWriter {
    inner: Arc<Inner>,
}

struct Inner {
    buffer: Mutex<Buffer>,
    condvar: Condvar,
}

struct Buffer {
    closed: bool,
    buf: VecDeque<u8>,
}

pub(crate) fn create() -> (PipeReader, PipeWriter) {
    let inner = Arc::new(Inner {
        buffer: Mutex::new(Buffer {
            closed: false,
            buf: VecDeque::with_capacity(1024 * 1024),
        }),
        condvar: Condvar::new(),
    });

    (
        PipeReader {
            inner: inner.clone(),
        },
        PipeWriter { inner },
    )
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        let mut buffer = self.inner.buffer.lock().unwrap();
        if !buffer.closed {
            buffer.closed = true;
            // Notify that it cannot be written to anymore
            self.inner.condvar.notify_all();
        }
    }
}

impl std::io::Read for PipeReader {
    fn read(&mut self, dest_buf: &mut [u8]) -> std::io::Result<usize> {
        let mut buffer = self.inner.buffer.lock().unwrap();
        loop {
            if buffer.buf.is_empty() {
                if buffer.closed {
                    return Ok(0);
                }
                buffer = self.inner.condvar.wait(buffer).unwrap();
            } else {
                let was_full = buffer.buf.len() == buffer.buf.capacity();
                let to_read = dest_buf.len().min(buffer.buf.len());
                let (buf_1, buf_2) = buffer.buf.as_slices();

                if buf_1.len() < to_read {
                    dest_buf[..buf_1.len()].copy_from_slice(buf_1);
                    dest_buf[buf_1.len()..to_read]
                        .copy_from_slice(&buf_2[..(to_read - buf_1.len())]);
                } else {
                    dest_buf[..to_read].copy_from_slice(&buf_1[..to_read]);
                }

                buffer.buf.drain(..to_read);

                if was_full {
                    // Buffer was full, notify writer thread that there is
                    // space again
                    self.inner.condvar.notify_all();
                }
                drop(buffer);

                return Ok(to_read);
            }
        }
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let mut buffer = self.inner.buffer.lock().unwrap();
        if !buffer.closed {
            buffer.closed = true;
            // Notify that, once the buffer is empty,
            // it cannot be read from
            self.inner.condvar.notify_all();
        }
    }
}

impl std::io::Write for PipeWriter {
    fn write(&mut self, src_buf: &[u8]) -> std::io::Result<usize> {
        let mut buffer = self.inner.buffer.lock().unwrap();
        loop {
            if buffer.closed {
                return Ok(0);
            } else if buffer.buf.len() == buffer.buf.capacity() {
                buffer = self.inner.condvar.wait(buffer).unwrap();
            } else {
                let was_empty = buffer.buf.is_empty();
                let to_write = src_buf.len().min(buffer.buf.capacity() - buffer.buf.len());

                buffer.buf.extend(&src_buf[..to_write]);

                if was_empty {
                    // Buffer was empty, notify reader thread that there is
                    // data again
                    self.inner.condvar.notify_all();
                }
                drop(buffer);

                return Ok(to_write);
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
