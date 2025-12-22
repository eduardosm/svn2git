#[derive(Debug)]
pub(super) struct DeserializeError;

#[inline]
pub(super) fn serialize_byte_slice_into(bytes: &[u8], out: &mut Vec<u8>) {
    out.extend(bytes.len().to_ne_bytes());
    out.extend(bytes);
}

#[inline]
pub(super) fn serialize_oid_into(oid: &gix_hash::ObjectId, out: &mut Vec<u8>) {
    match oid {
        gix_hash::ObjectId::Sha1(hash) => {
            out.push(0);
            out.extend_from_slice(hash);
        }
        _ => unreachable!(), // non-exhaustive enum
    }
}

#[inline]
pub(super) fn deserialize_byte_from(src: &mut &[u8]) -> Result<u8, DeserializeError> {
    if let Some((&byte, rest)) = src.split_first() {
        *src = rest;
        Ok(byte)
    } else {
        Err(DeserializeError)
    }
}

#[inline]
pub(super) fn deserialize_bool_from(src: &mut &[u8]) -> Result<bool, DeserializeError> {
    let byte = deserialize_byte_from(src)?;
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DeserializeError),
    }
}

#[inline]
pub(super) fn deserialize_byte_array_from<const N: usize>(
    src: &mut &[u8],
) -> Result<[u8; N], DeserializeError> {
    let array;
    (array, *src) = src.split_first_chunk().ok_or(DeserializeError)?;
    Ok(*array)
}

#[inline]
pub(super) fn deserialize_byte_slice_from(src: &mut &[u8]) -> Result<Vec<u8>, DeserializeError> {
    let len = usize::from_ne_bytes(deserialize_byte_array_from(src)?);
    if src.len() < len {
        return Err(DeserializeError);
    }

    let data;
    (data, *src) = src.split_at(len);

    Ok(data.to_vec())
}

#[inline]
pub(super) fn deserialize_oid_from(
    src: &mut &[u8],
) -> Result<gix_hash::ObjectId, DeserializeError> {
    let hash_type = deserialize_byte_from(src)?;
    match hash_type {
        0 => {
            let hash = deserialize_byte_array_from(src)?;
            Ok(gix_hash::ObjectId::Sha1(hash))
        }
        _ => Err(DeserializeError),
    }
}
