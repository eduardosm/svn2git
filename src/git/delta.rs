#[derive(Debug)]
pub(crate) enum PatchError {
    BadHeaderSize,
    UnexpectedBaseSize { expected: usize, actual: usize },
    TruncatedDelta,
    InvalidOpcode,
    SrcOutOfBounds,
    UnexpectedOutputSize { expected: usize, actual: usize },
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::BadHeaderSize => write!(f, "bad header size"),
            Self::UnexpectedBaseSize { expected, actual } => {
                write!(
                    f,
                    "unexpected delta size: expected {expected}, got {actual}"
                )
            }
            Self::TruncatedDelta => {
                write!(f, "truncated delta")
            }
            Self::InvalidOpcode => {
                write!(f, "invalid opcode")
            }
            Self::SrcOutOfBounds => {
                write!(f, "source out of founds")
            }
            Self::UnexpectedOutputSize { expected, actual } => {
                write!(
                    f,
                    "unexpected output size: expected {expected}, got {actual}"
                )
            }
        }
    }
}

pub(super) fn patch(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, PatchError> {
    let mut rem_delta = delta;

    let base_len = read_header_size(&mut rem_delta).ok_or(PatchError::BadHeaderSize)?;
    if base.len() != base_len {
        return Err(PatchError::UnexpectedBaseSize {
            expected: base_len,
            actual: base.len(),
        });
    }

    let out_len = read_header_size(&mut rem_delta).ok_or(PatchError::BadHeaderSize)?;
    let mut out = Vec::with_capacity(out_len);

    while !rem_delta.is_empty() {
        let op = rem_delta[0];
        rem_delta = &rem_delta[1..];

        if op == 0 {
            return Err(PatchError::InvalidOpcode);
        } else if op & 0x80 != 0 {
            let mut get_byte = || -> Result<u8, PatchError> {
                let byte;
                (byte, rem_delta) = rem_delta.split_first().ok_or(PatchError::TruncatedDelta)?;
                Ok(*byte)
            };

            let mut offset = 0;
            if op & 0x01 != 0 {
                offset |= u32::from(get_byte()?);
            }
            if op & 0x02 != 0 {
                offset |= u32::from(get_byte()?) << 8;
            }
            if op & 0x04 != 0 {
                offset |= u32::from(get_byte()?) << 16;
            }
            if op & 0x08 != 0 {
                offset |= u32::from(get_byte()?) << 24;
            }

            let mut len = 0;
            if op & 0x10 != 0 {
                len |= u32::from(get_byte()?);
            }
            if op & 0x20 != 0 {
                len |= u32::from(get_byte()?) << 8;
            }
            if op & 0x40 != 0 {
                len |= u32::from(get_byte()?) << 16;
            }
            if len == 0 {
                len = 0x10000;
            }

            let offset = usize::try_from(offset).map_err(|_| PatchError::SrcOutOfBounds)?;
            let len = usize::try_from(len).map_err(|_| PatchError::SrcOutOfBounds)?;

            let src_start = offset;
            let src_end = offset.checked_add(len).ok_or(PatchError::SrcOutOfBounds)?;
            let src_chunk = base
                .get(src_start..src_end)
                .ok_or(PatchError::SrcOutOfBounds)?;

            out.extend(src_chunk);
        } else {
            let len = usize::from(op);
            out.extend(&rem_delta[..len]);
            rem_delta = &rem_delta[len..];
        }
    }

    if out.len() == out_len {
        Ok(out)
    } else {
        Err(PatchError::UnexpectedOutputSize {
            expected: out_len,
            actual: out.len(),
        })
    }
}

fn read_header_size(delta: &mut &[u8]) -> Option<usize> {
    let mut iter = delta.iter();
    let mut shift = 0;
    let mut size = 0usize;
    for &byte in iter.by_ref() {
        if shift >= usize::BITS {
            return None;
        }

        let chunk = usize::from(byte & 0x7F);
        let shifted = chunk << shift;
        if (shifted >> shift) != chunk {
            return None;
        }

        size |= usize::from(byte & 0x7F) << shift;

        if byte & 0x80 == 0 {
            *delta = iter.as_slice();
            return Some(size);
        }
        shift += 7;
    }

    None
}

struct DeltaTable {
    table: Vec<u32>,
    mask: u32,
}

impl DeltaTable {
    fn create(src: &[u8], window_shift: u32) -> Self {
        // Delta format cannot encode offsets larger than 32 bits
        let max_offset = u32::try_from(src.len()).unwrap_or(u32::MAX) as usize;

        let window_len = 1usize.checked_shl(window_shift).unwrap();

        let num_entries = (max_offset >> window_shift).next_power_of_two();
        let mut table = Self {
            table: vec![u32::MAX; num_entries],
            mask: u32::try_from(num_entries - 1).unwrap(),
        };

        for (i, src_chunk) in src[..max_offset].chunks_exact(window_len).enumerate() {
            let hash = cyclic_poly_23::CyclicPoly32::from_block(src_chunk).value();

            let offset = i * window_len;
            table.insert(hash, offset);
        }

        table
    }

    fn insert(&mut self, hash: u32, offset: usize) {
        let entry = &mut self.table[(hash & self.mask) as usize];
        if *entry == u32::MAX {
            *entry = offset.try_into().unwrap();
        }
    }

    fn get(&self, hash: u32) -> Option<usize> {
        let entry = self.table[(hash & self.mask) as usize];
        if entry != u32::MAX {
            Some(entry as usize)
        } else {
            None
        }
    }
}

pub(super) fn diff(base: &[u8], target: &[u8], window_shift: u32) -> Option<Vec<u8>> {
    let window_len = 1usize.checked_shl(window_shift).unwrap();
    if target.len() <= window_len.max(16) {
        return None;
    }

    let table = DeltaTable::create(base, window_shift);

    let mut out = Vec::new();
    encode_header_size(base.len(), &mut out);
    encode_header_size(target.len(), &mut out);

    let mut chunks = Vec::new();

    let mut target_i = 0;
    let mut target_handled = 0;
    let mut hasher = cyclic_poly_23::CyclicPoly32::from_block(&target[..window_len]);
    loop {
        let hash = hasher.value();

        if let Some(src_i) = table.get(hash).filter(|&src_i| {
            base[src_i..(src_i + window_len)] == target[target_i..(target_i + window_len)]
        }) {
            let mut src_range = src_i..(src_i + window_len);
            let mut target_range = target_i..(target_i + window_len);

            while src_range.start != 0
                && target_range.start != 0
                && base[src_range.start - 1] == target[target_range.start - 1]
            {
                src_range.start -= 1;
                target_range.start -= 1;
            }

            while src_range.end != base.len()
                && target_range.end != target.len()
                && base[src_range.end] == target[target_range.end]
            {
                src_range.end += 1;
                target_range.end += 1;
            }

            let mut target_eff_start = target_handled.max(target_range.start);
            let mut eff_out_len = out.len();
            while let Some(&(prev_target_i, prev_out_i)) = chunks.last() {
                if prev_target_i < target_range.start {
                    break;
                }

                target_eff_start = prev_target_i;
                eff_out_len = prev_out_i;
                chunks.pop();
            }

            out.truncate(eff_out_len);

            if target_eff_start > target_handled {
                let inline_data = &target[target_handled..target_eff_start];
                for inline_chunk in inline_data.chunks(0x7F) {
                    chunks.push((target_handled, out.len()));
                    out.push(inline_chunk.len() as u8);
                    out.extend(inline_chunk);
                    target_handled += inline_chunk.len();
                }
            }

            let src_offset = src_range.start + (target_eff_start - target_range.start);

            let mut src_len = src_range.end - src_offset;
            let mut src_offset = u32::try_from(src_offset).unwrap();

            // Maximum source chunk length is 16777215 bytes (2^24 - 1),
            // split into multiple if larger
            let max_src_chunk_len = 0xFFFFFF;
            while src_len != 0 {
                let src_chunk_len = u32::try_from(src_len)
                    .unwrap_or(u32::MAX)
                    .min(max_src_chunk_len);

                chunks.push((target_eff_start, out.len()));

                let mut op = 0x80;
                if src_offset & 0xFF != 0 {
                    op |= 0x01;
                }
                if src_offset & 0xFF00 != 0 {
                    op |= 0x02;
                }
                if src_offset & 0xFF0000 != 0 {
                    op |= 0x04;
                }
                if src_offset & 0xFF000000 != 0 {
                    op |= 0x08;
                }
                if src_chunk_len != 0x10000 {
                    if src_chunk_len & 0xFF != 0 {
                        op |= 0x10;
                    }
                    if src_chunk_len & 0xFF00 != 0 {
                        op |= 0x20;
                    }
                    if src_chunk_len & 0xFF0000 != 0 {
                        op |= 0x40;
                    }
                }

                out.push(op);
                if src_offset & 0xFF != 0 {
                    out.push(src_offset as u8);
                }
                if src_offset & 0xFF00 != 0 {
                    out.push((src_offset >> 8) as u8);
                }
                if src_offset & 0xFF0000 != 0 {
                    out.push((src_offset >> 16) as u8);
                }
                if src_offset & 0xFF000000 != 0 {
                    out.push((src_offset >> 24) as u8);
                }
                if src_chunk_len != 0x10000 {
                    if src_chunk_len & 0xFF != 0 {
                        out.push(src_chunk_len as u8);
                    }
                    if src_chunk_len & 0xFF00 != 0 {
                        out.push((src_chunk_len >> 8) as u8);
                    }
                    if src_chunk_len & 0xFF0000 != 0 {
                        out.push((src_chunk_len >> 16) as u8);
                    }
                }

                target_eff_start += src_chunk_len as usize;
                src_offset += src_chunk_len;
                src_len -= src_chunk_len as usize;
            }

            target_handled = target_range.end;
            assert_eq!(target_eff_start, target_handled);

            if target.len() - target_range.end < window_len {
                break;
            }
            target_i = target_range.end;
            hasher.reset_hash();
            hasher.update(&target[target_i..(target_i + window_len)]);
        } else {
            let Some(&rot_new_byte) = target.get(target_i + window_len) else {
                break;
            };
            let rot_old_byte = target[target_i];
            target_i += 1;

            hasher.rotate(rot_old_byte, rot_new_byte);
        }
    }

    if target_handled < target.len() {
        let inline_data = &target[target_handled..];
        for inline_chunk in inline_data.chunks(0x7F) {
            out.push(inline_chunk.len() as u8);
            out.extend(inline_chunk);
        }
    }

    if out.len() >= target.len() {
        None
    } else {
        Some(out)
    }
}

fn encode_header_size(size: usize, out: &mut Vec<u8>) {
    let mut rem = size;
    loop {
        let mut byte = (rem & 0x7F) as u8;
        rem >>= 7;
        let last = rem == 0;
        if !last {
            byte |= 0x80;
        }

        out.push(byte);

        if last {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{diff, encode_header_size, patch, read_header_size};

    #[test]
    fn tests_header_size() {
        fn test(src: &[u8], expected: Option<(usize, usize)>) {
            let mut cursor = src;

            let r = read_header_size(&mut cursor);
            assert_eq!(r, expected.map(|(s, _)| s));

            if let Some((size, size_len)) = expected {
                assert_eq!(cursor, &src[size_len..]);

                let mut reencoded = Vec::new();
                encode_header_size(size, &mut reencoded);
                assert_eq!(reencoded, src[..size_len]);
            }
        }

        test(&[], None);
        test(&[0x81], None);
        test(&[0x81, 0x82], None);
        test(&[0x81; 100], None);

        test(&[0x01], Some((1, 1)));
        test(&[0x81, 0x02], Some((257, 2)));
        test(&[0x81, 0x82, 0x03], Some((49409, 3)));

        test(&[0x01, 0xAA], Some((1, 1)));
        test(&[0x81, 0x02, 0xAA], Some((257, 2)));
        test(&[0x81, 0x82, 0x03, 0xAA], Some((49409, 3)));
    }

    #[test]
    fn test_diff_and_patch() {
        fn test(base: &[u8], target: &[u8], window_shift: u32, expected_diff: &[u8]) {
            let diff = diff(base, target, window_shift).unwrap();
            if diff != expected_diff {
                panic!(
                    "\"{}\" != \"{}\"",
                    diff.escape_ascii(),
                    expected_diff.escape_ascii(),
                );
            }

            let patched = patch(base, &diff).unwrap();
            if patched != target {
                panic!(
                    "\"{}\" != \"{}\"",
                    patched.escape_ascii(),
                    target.escape_ascii(),
                );
            }
        }

        test(
            b"This is a test for delta compression",
            b"There is a test for Delta compressioN",
            2,
            &[
                0x24, // Base length
                0x25, // Target length
                0x05, b'T', b'h', b'e', b'r', b'e', // Inline "There"
                0x91, 0x04, 0x0F, // Offset 4, length 15
                0x01, b'D', // Inline "D"
                0x91, 0x14, 0x0F, // Offset 20, length 15
                0x01, b'N', // Inline "N"
            ],
        );

        test(
            b"_this is a test this is a test",
            b"this is a test this is a test",
            3,
            &[
                0x1E, // Base length
                0x1D, // Target length
                0x91, 0x01, 0x1D, // Offset 1, length 29
            ],
        );

        test(
            b"_this is a test this is a test",
            b"this is a test:this is a test",
            3,
            &[
                0x1E, // Base length
                0x1D, // Target length
                0x91, 0x10, 0x0E, // Offset 16, length 14
                0x01, b':', // Inline ":"
                0x91, 0x10, 0x0E, // Offset 16, length 14
            ],
        );

        test(
            b" is a test this is a test |This is a tesT This is a test",
            b"This is a tesT This is a test",
            3,
            &[
                0x38, // Base length
                0x1D, // Target length
                0x91, 0x1B, 0x1D, // Offset 27, length 29
            ],
        );

        // Test case larger than 2^24 bytes

        let mut base = Vec::new();
        base.push(b'0');
        base.extend(std::iter::repeat_n(b'A', 5_000_000));
        base.extend(std::iter::repeat_n(b'B', 5_000_000));
        base.extend(std::iter::repeat_n(b'C', 5_000_000));
        base.extend(std::iter::repeat_n(b'D', 5_000_000));

        let mut target = Vec::new();
        target.push(b'1');
        target.extend(&base[1..]);
        target.push(b'E');

        test(
            &base,
            &target,
            3,
            &[
                0x81, 0xDA, 0xC4, 0x09, // Base length
                0x82, 0xDA, 0xC4, 0x09, // Target length
                0x01, b'1', // Inline "1"
                0xF1, 0x01, 0xFF, 0xFF, 0xFF, // Offset 1, length 16777215
                0xF8, 0x01, 0x01, 0x2D, 0x31, // Offset 16777216, length 3222785
                0x01, b'E', // Inline "E"
            ],
        );
    }
}
