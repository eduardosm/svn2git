// svndiff format described in
// https://svn.apache.org/repos/asf/subversion/trunk/notes/svndiff

#[derive(Debug)]
pub(crate) enum ApplyError {
    InvalidDeltaHeader,
    DestIo(std::io::Error),
    InvalidVarLenInt,
    OffsetTooLarge,
    LenTooLarge,
    SourceViewOutOfBounds {
        source_len: usize,
        view_offset: usize,
        view_len: usize,
    },
    TruncatedInstrs,
    TruncatedNewData,
    NotEnoughNewData,
    InvalidInstr,
    MismatchedTargetLen,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::InvalidDeltaHeader => write!(f, "invalid delta header"),
            Self::DestIo(ref e) => write!(f, "destination I/O error: {e}"),
            Self::InvalidVarLenInt => write!(f, "invalid variable-length integer"),
            Self::OffsetTooLarge => write!(f, "offset too large"),
            Self::LenTooLarge => write!(f, "length too large"),
            Self::SourceViewOutOfBounds {
                source_len,
                view_offset,
                view_len,
            } => write!(
                f,
                "source view with offset {view_offset} and length {view_len} out of bounds, source length is {source_len}",
            ),
            Self::TruncatedInstrs => write!(f, "truncated instructions"),
            Self::TruncatedNewData => write!(f, "truncated new data"),
            Self::NotEnoughNewData => write!(f, "not enough new data"),
            Self::InvalidInstr => write!(f, "invalid instruction"),
            Self::MismatchedTargetLen => write!(f, "mismatched target length"),
        }
    }
}

pub(crate) fn apply(
    delta: &[u8],
    source: &[u8],
    dest: &mut dyn std::io::Write,
) -> Result<(), ApplyError> {
    let mut rem_delta = delta;
    // only support version 0
    rem_delta = rem_delta
        .strip_prefix(b"SVN\0")
        .ok_or(ApplyError::InvalidDeltaHeader)?;

    while !rem_delta.is_empty() {
        let source_view_off = read_var_len_int(&mut rem_delta)?;
        let source_view_len = read_var_len_int(&mut rem_delta)?;
        let target_view_len = read_var_len_int(&mut rem_delta)?;
        let instrs_len = read_var_len_int(&mut rem_delta)?;
        let new_data_len = read_var_len_int(&mut rem_delta)?;

        let source_view_off =
            usize::try_from(source_view_off).map_err(|_| ApplyError::OffsetTooLarge)?;
        let source_view_len =
            usize::try_from(source_view_len).map_err(|_| ApplyError::LenTooLarge)?;
        let source_view = source
            .get(source_view_off..(source_view_off + source_view_len))
            .ok_or(ApplyError::SourceViewOutOfBounds {
                source_len: source.len(),
                view_offset: source_view_off,
                view_len: source_view_len,
            })?;

        let instrs_len = usize::try_from(instrs_len).map_err(|_| ApplyError::LenTooLarge)?;
        if rem_delta.len() < instrs_len {
            return Err(ApplyError::TruncatedInstrs);
        }
        let mut instrs;
        (instrs, rem_delta) = rem_delta.split_at(instrs_len);

        let new_data_len = usize::try_from(new_data_len).map_err(|_| ApplyError::LenTooLarge)?;
        if rem_delta.len() < new_data_len {
            return Err(ApplyError::TruncatedNewData);
        }
        let mut new_data;
        (new_data, rem_delta) = rem_delta.split_at(new_data_len);

        let target_view_len =
            usize::try_from(target_view_len).map_err(|_| ApplyError::LenTooLarge)?;
        let mut target_buf = Vec::with_capacity(target_view_len);

        while !instrs.is_empty() {
            let (instr, copy_len) = read_instruction(&mut instrs)?;
            let copy_len = usize::try_from(copy_len).map_err(|_| ApplyError::LenTooLarge)?;

            match instr {
                0b00 => {
                    // copy from source view
                    let copy_offset = read_var_len_int(&mut instrs)?;
                    let copy_offset =
                        usize::try_from(copy_offset).map_err(|_| ApplyError::OffsetTooLarge)?;

                    target_buf.extend(&source_view[copy_offset..(copy_offset + copy_len)]);
                }
                0b01 => {
                    // copy from target view
                    let copy_offset = read_var_len_int(&mut instrs)?;
                    let copy_offset =
                        usize::try_from(copy_offset).map_err(|_| ApplyError::LenTooLarge)?;

                    for i in 0..copy_len {
                        target_buf.push(target_buf[copy_offset + i]);
                    }
                }
                0b10 => {
                    // copy from new data
                    if copy_len > new_data.len() {
                        return Err(ApplyError::NotEnoughNewData);
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

        if target_buf.len() != target_view_len {
            return Err(ApplyError::MismatchedTargetLen);
        }

        dest.write_all(&target_buf).map_err(ApplyError::DestIo)?;
    }

    Ok(())
}

fn read_var_len_int(src: &mut &[u8]) -> Result<u64, ApplyError> {
    let mut value = 0;
    loop {
        let byte;
        (byte, *src) = src.split_first().ok_or(ApplyError::InvalidVarLenInt)?;

        if value > (u64::MAX >> 7) {
            return Err(ApplyError::InvalidVarLenInt);
        }

        value = (value << 7) | u64::from(byte & 0x7F);
        if (byte & 0x80) == 0 {
            return Ok(value);
        }
    }
}

fn read_instruction(src: &mut &[u8]) -> Result<(u8, u64), ApplyError> {
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

#[cfg(test)]
mod tests {
    use super::apply;

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
        apply(delta, source, &mut target).unwrap();

        assert_eq!(target, expected_target);
    }
}
