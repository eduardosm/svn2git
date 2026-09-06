// svndiff format described in
// https://svn.apache.org/repos/asf/subversion/trunk/notes/svndiff

#[derive(Debug)]
pub(crate) enum ApplyError {
    InvalidDeltaHeader,
    DeltaRead(std::io::Error),
    SourceRead(std::io::Error),
    DestWrite(std::io::Error),
    InvalidVarLenInt,
    WindowTooLarge,
    SourceViewSlidesBackwards,
    CopyLenIsZero,
    SourceCopyOutOfBounds {
        source_view_len: usize,
        copy_offset: u64,
        copy_len: usize,
    },
    TargetCopyOutOfBounds {
        target_len: usize,
        copy_offset: u64,
    },
    NewDataCopyOutOfBounds {
        new_data_len: usize,
        copy_len: usize,
    },
    NewDataNotConsumed,
    InvalidInstr,
    MismatchedTargetLen,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::InvalidDeltaHeader => write!(f, "invalid delta header"),
            Self::DeltaRead(ref e) => write!(f, "failed to read delta: {e}"),
            Self::SourceRead(ref e) => write!(f, "failed to read source: {e}"),
            Self::DestWrite(ref e) => write!(f, "failed to write destination: {e}"),
            Self::InvalidVarLenInt => write!(f, "invalid variable-length integer"),
            Self::WindowTooLarge => write!(f, "window too large"),
            Self::SourceViewSlidesBackwards => write!(f, "source view slides backwards"),
            Self::CopyLenIsZero => write!(f, "copy length is zero"),
            Self::SourceCopyOutOfBounds {
                source_view_len,
                copy_offset,
                copy_len,
            } => write!(
                f,
                "copy of {copy_len} byte(s) from the source view at offset {copy_offset} is out of bounds, the source view is {source_view_len} byte(s) long",
            ),
            Self::TargetCopyOutOfBounds {
                target_len,
                copy_offset,
            } => write!(
                f,
                "copy from the target view at offset {copy_offset} is out of bounds, only {target_len} byte(s) have been produced so far",
            ),
            Self::NewDataCopyOutOfBounds {
                new_data_len,
                copy_len,
            } => write!(
                f,
                "copy of {copy_len} byte(s) from the new data is out of bounds, only {new_data_len} byte(s) are left",
            ),
            Self::NewDataNotConsumed => write!(f, "new data not fully consumed"),
            Self::InvalidInstr => write!(f, "invalid instruction"),
            Self::MismatchedTargetLen => write!(f, "mismatched target length"),
        }
    }
}

// From Subversion source code
const SVN_DELTA_WINDOW_SIZE: usize = 102400;
const SVN_MAX_ENCODED_UINT_LEN: usize = 10;
const SVN_MAX_INSTRUCTION_LEN: usize = 2 * SVN_MAX_ENCODED_UINT_LEN + 1;
const SVN_MAX_INSTRUCTION_SECTION_LEN: usize = SVN_DELTA_WINDOW_SIZE * SVN_MAX_INSTRUCTION_LEN;

pub(crate) fn apply(
    delta: &mut dyn std::io::BufRead,
    source: &mut dyn std::io::Read,
    dest: &mut dyn std::io::Write,
) -> Result<(), ApplyError> {
    let mut header = [0; 4];
    delta
        .read_exact(&mut header)
        .map_err(ApplyError::DeltaRead)?;
    // only support version 0
    if header != *b"SVN\0" {
        return Err(ApplyError::InvalidDeltaHeader);
    }

    let mut source_view = WindowBuf::new(source);

    while buf_read_has_data_left(delta).map_err(ApplyError::DeltaRead)? {
        let source_view_off = read_var_len_int(delta)?;
        let source_view_len = read_var_len_int(delta)?;
        let target_view_len = read_var_len_int(delta)?;
        let instrs_len = read_var_len_int(delta)?;
        let new_data_len = read_var_len_int(delta)?;

        let source_view_len = usize::try_from(source_view_len)
            .ok()
            .filter(|&l| l <= SVN_DELTA_WINDOW_SIZE)
            .ok_or(ApplyError::WindowTooLarge)?;

        if !source_view
            .slide_forward(source_view_off, source_view_len)
            .map_err(ApplyError::SourceRead)?
        {
            return Err(ApplyError::SourceViewSlidesBackwards);
        }
        let source_view = source_view.buf();

        let instrs_len = usize::try_from(instrs_len)
            .ok()
            .filter(|&l| l <= SVN_MAX_INSTRUCTION_SECTION_LEN)
            .ok_or(ApplyError::WindowTooLarge)?;
        let mut instrs = vec![0; instrs_len];
        delta
            .read_exact(&mut instrs)
            .map_err(ApplyError::DeltaRead)?;
        let mut instrs = instrs.as_slice();

        let new_data_len = usize::try_from(new_data_len)
            .ok()
            .filter(|&l| l <= SVN_DELTA_WINDOW_SIZE + SVN_MAX_ENCODED_UINT_LEN)
            .ok_or(ApplyError::WindowTooLarge)?;
        let mut new_data = vec![0; new_data_len];
        delta
            .read_exact(&mut new_data)
            .map_err(ApplyError::DeltaRead)?;
        let mut new_data = new_data.as_slice();

        let target_view_len = usize::try_from(target_view_len)
            .ok()
            .filter(|&l| l <= SVN_DELTA_WINDOW_SIZE)
            .ok_or(ApplyError::WindowTooLarge)?;
        let mut target_buf = Vec::with_capacity(target_view_len);

        while !instrs.is_empty() {
            let (instr, copy_len) = read_instruction(&mut instrs)?;
            if copy_len == 0 {
                return Err(ApplyError::CopyLenIsZero);
            }

            // No instruction may produce more data than the target view holds,
            // which also keeps `target_buf` bounded by `target_view_len`.
            let copy_len = usize::try_from(copy_len)
                .ok()
                .filter(|&l| l <= target_view_len - target_buf.len())
                .ok_or(ApplyError::MismatchedTargetLen)?;

            match instr {
                0b00 => {
                    // copy from source view
                    let copy_offset = read_var_len_int(&mut instrs)?;

                    let copy_src = usize::try_from(copy_offset)
                        .ok()
                        .and_then(|off| source_view.get(off..)?.get(..copy_len))
                        .ok_or(ApplyError::SourceCopyOutOfBounds {
                            source_view_len: source_view.len(),
                            copy_offset,
                            copy_len,
                        })?;
                    target_buf.extend(copy_src);
                }
                0b01 => {
                    // copy from target view
                    let copy_offset = read_var_len_int(&mut instrs)?;

                    // The copied range is allowed to overlap the data being
                    // produced, so each byte is read back as it is written.
                    // Only the first read can be out of bounds: every later one
                    // trails the write by a constant amount.
                    let copy_offset = usize::try_from(copy_offset)
                        .ok()
                        .filter(|&off| off < target_buf.len())
                        .ok_or(ApplyError::TargetCopyOutOfBounds {
                            target_len: target_buf.len(),
                            copy_offset,
                        })?;

                    for i in 0..copy_len {
                        target_buf.push(target_buf[copy_offset + i]);
                    }
                }
                0b10 => {
                    // copy from new data
                    if copy_len > new_data.len() {
                        return Err(ApplyError::NewDataCopyOutOfBounds {
                            new_data_len: new_data.len(),
                            copy_len,
                        });
                    }
                    let copy_data;
                    (copy_data, new_data) = new_data.split_at(copy_len);
                    target_buf.extend(copy_data);
                }
                0b11 => {
                    // invalid
                    return Err(ApplyError::InvalidInstr);
                }
                _ => unreachable!(),
            }
        }

        if !new_data.is_empty() {
            return Err(ApplyError::NewDataNotConsumed);
        }

        if target_buf.len() != target_view_len {
            return Err(ApplyError::MismatchedTargetLen);
        }

        dest.write_all(&target_buf).map_err(ApplyError::DestWrite)?;
    }

    Ok(())
}

fn read_var_len_int(src: &mut (impl std::io::BufRead + ?Sized)) -> Result<u64, ApplyError> {
    let mut buf: &[u8] = &[];
    let mut consumed = 0;
    let mut value = 0;
    loop {
        if buf.is_empty() {
            src.consume(consumed);
            consumed = 0;
            buf = src.fill_buf().map_err(ApplyError::DeltaRead)?;
        }

        let byte;
        (byte, buf) = buf.split_first().ok_or(ApplyError::InvalidVarLenInt)?;
        consumed += 1;

        if value > (u64::MAX >> 7) {
            return Err(ApplyError::InvalidVarLenInt);
        }

        value = (value << 7) | u64::from(byte & 0x7F);
        if (byte & 0x80) == 0 {
            break;
        }
    }

    src.consume(consumed);
    Ok(value)
}

fn read_instruction(src: &mut &[u8]) -> Result<(u8, u64), ApplyError> {
    // Caller must ensure that `src` is not empty.
    let first_byte = src[0];
    *src = &src[1..];

    let instr = first_byte >> 6;

    if (first_byte & 0x3F) != 0 {
        let len = u64::from(first_byte & 0x3F);
        Ok((instr, len))
    } else {
        let len = read_var_len_int(src)?;
        Ok((instr, len))
    }
}

fn buf_read_has_data_left(src: &mut (impl std::io::BufRead + ?Sized)) -> std::io::Result<bool> {
    // TODO: use `BufRead::has_data_left` when stable
    src.fill_buf().map(|buf| !buf.is_empty())
}

struct WindowBuf<R: std::io::Read> {
    source: R,
    current_offset: u64,
    buf: Vec<u8>,
}

impl<R: std::io::Read> WindowBuf<R> {
    fn new(source: R) -> Self {
        Self {
            source,
            current_offset: 0,
            buf: Vec::new(),
        }
    }

    fn buf(&self) -> &[u8] {
        &self.buf
    }

    fn slide_forward(&mut self, new_offset: u64, new_len: usize) -> Result<bool, std::io::Error> {
        if new_offset < self.current_offset {
            return Ok(false);
        }
        let current_len_u64 = u64::try_from(self.buf.len()).unwrap();
        let current_end_offset = self.current_offset + current_len_u64;
        let new_len_u64 = u64::try_from(new_len).unwrap();
        if new_offset
            .checked_add(new_len_u64)
            .is_some_and(|new_end| new_end < current_end_offset)
        {
            return Ok(false);
        }

        let offset_diff = new_offset - self.current_offset;
        if let Some(offset_diff) = usize::try_from(offset_diff)
            .ok()
            .filter(|&d| d <= self.buf.len())
        {
            self.buf.drain(..offset_diff);
        } else {
            let mut rem_skip = offset_diff - current_len_u64;
            self.buf.clear();
            let mut buf = [0; 1024];
            while rem_skip != 0 {
                let to_read = usize::try_from(rem_skip)
                    .ok()
                    .filter(|&r| r <= buf.len())
                    .unwrap_or(buf.len());
                self.source.read_exact(&mut buf[..to_read])?;
                rem_skip -= u64::try_from(to_read).unwrap();
            }
        }

        // The backwards-sliding check above guarantees that the new window
        // ends at or after the current one, so `new_len` is never smaller than
        // the number of bytes kept, and the slice below is always in range.
        let old_len = self.buf.len();
        self.buf.resize(new_len, 0);
        self.source.read_exact(&mut self.buf[old_len..])?;
        self.current_offset = new_offset;

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{ApplyError, WindowBuf, apply};

    #[test]
    fn test_apply() {
        // Example from specification document
        let source = b"aaaabbbbcccc";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x0C, // source view length 12
            0x10, // target view length 16
            0x07, // instructions length 7
            0x01, // new data length 1
            0x04, 0x00, // source, length 4, offset 0
            0x04, 0x08, // source, length 4, offset 8
            0x81, // new, length 1
            0x47, 0x08, // target, length 7, offset 8
            b'd', // new data 'd'
        ];
        let expected_target = b"aaaaccccdddddddd";

        let mut target = Vec::new();
        apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap();

        assert_eq!(target, expected_target);
    }

    #[test]
    fn test_apply_source_copy_out_of_bounds() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x04, // target view length 4
            0x03, // instructions length 3
            0x00, // new data length 0
            0x04, 0x81, 0x48, // source, length 4, offset 200
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::SourceCopyOutOfBounds {
                source_view_len: 4,
                copy_offset: 200,
                copy_len: 4,
            },
        ));
    }

    #[test]
    fn test_apply_target_copy_out_of_bounds() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x04, // target view length 4
            0x02, // instructions length 2
            0x00, // new data length 0
            0x44, 0x10, // target, length 4, offset 16
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::TargetCopyOutOfBounds {
                target_len: 0,
                copy_offset: 16,
            },
        ));
    }

    #[test]
    fn test_apply_target_copy_from_unwritten_byte() {
        let source = b"aaaa";
        // Copying from the target view may overlap the data being produced,
        // but it cannot start at a byte that has not been produced yet.
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x04, // target view length 4
            0x04, // instructions length 4
            0x00, // new data length 0
            0x02, 0x00, // source, length 2, offset 0
            0x42, 0x02, // target, length 2, offset 2
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::TargetCopyOutOfBounds {
                target_len: 2,
                copy_offset: 2,
            },
        ));
    }

    #[test]
    fn test_apply_too_much_target_data() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x02, // target view length 2
            0x02, // instructions length 2
            0x00, // new data length 0
            0x04, 0x00, // source, length 4, offset 0
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::MismatchedTargetLen,
        ));
    }

    #[test]
    fn test_apply_window_too_large() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            // source view length `u64::MAX`
            0x81, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F, //
            0x01, // target view length 1
            0x00, // instructions length 0
            0x00, // new data length 0
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::WindowTooLarge,
        ));
    }

    #[test]
    fn test_apply_new_data_copy_out_of_bounds() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x00, // source view length 0
            0x04, // target view length 4
            0x01, // instructions length 1
            0x01, // new data length 1
            0x84, // new, length 4
            b'x', // new data
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::NewDataCopyOutOfBounds {
                new_data_len: 1,
                copy_len: 4,
            },
        ));
    }

    #[test]
    fn test_apply_copy_len_is_zero() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x04, // target view length 4
            0x03, // instructions length 3
            0x00, // new data length 0
            0x40, 0x00, 0x00, // source, length 0, offset 0
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::CopyLenIsZero,
        ));
    }

    #[test]
    fn test_apply_invalid_instr() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x04, // source view length 4
            0x04, // target view length 4
            0x01, // instructions length 1
            0x00, // new data length 0
            0xC4, // invalid instruction, length 4
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::InvalidInstr,
        ));
    }

    #[test]
    fn test_apply_new_data_not_consumed() {
        let source = b"aaaa";
        let delta = &[
            b'S', b'V', b'N', 0,    // header
            0x00, // source view offset 0
            0x00, // source view length 0
            0x01, // target view length 1
            0x01, // instructions length 1
            0x02, // new data length 2
            0x81, // new, length 1
            b'x', b'y', // new data
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::NewDataNotConsumed,
        ));
    }

    #[test]
    fn test_apply_source_view_slides_backwards() {
        let source = b"aaaabbbb";
        let delta = &[
            b'S', b'V', b'N', 0, // header
            // first window, source view [2, 4)
            0x02, // source view offset 2
            0x02, // source view length 2
            0x02, // target view length 2
            0x02, // instructions length 2
            0x00, // new data length 0
            0x02, 0x00, // source, length 2, offset 0
            // second window, source view [0, 2)
            0x00, // source view offset 0
            0x02, // source view length 2
            0x02, // target view length 2
            0x02, // instructions length 2
            0x00, // new data length 0
            0x02, 0x00, // source, length 2, offset 0
        ];

        let mut target = Vec::new();
        assert!(matches!(
            apply(&mut delta.as_slice(), &mut source.as_slice(), &mut target).unwrap_err(),
            ApplyError::SourceViewSlidesBackwards,
        ));
    }

    #[test]
    fn test_window_buf() {
        let source = b"abcdefghijklmnopqrstuvwxyz";
        let mut window_buf = WindowBuf::new(source.as_slice());

        assert_eq!(window_buf.buf(), b"");
        assert!(window_buf.slide_forward(0, 6).unwrap());
        assert_eq!(window_buf.buf(), b"abcdef");
        assert!(window_buf.slide_forward(0, 6).unwrap());
        assert_eq!(window_buf.buf(), b"abcdef");
        assert!(window_buf.slide_forward(2, 4).unwrap());
        assert_eq!(window_buf.buf(), b"cdef");
        assert!(window_buf.slide_forward(3, 6).unwrap());
        assert_eq!(window_buf.buf(), b"defghi");
        assert!(window_buf.slide_forward(20, 5).unwrap());
        assert_eq!(window_buf.buf(), b"uvwxy");
    }
}
