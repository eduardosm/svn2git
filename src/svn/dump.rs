use std::collections::HashMap;
use std::io::Read as _;

// SVN dump file format described in
// https://svn.apache.org/repos/asf/subversion/trunk/notes/dump-load-format.txt

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum DumpVersion {
    Two,
    Three,
}

impl DumpVersion {
    fn parse(s: &[u8]) -> Option<Self> {
        match s {
            b"2" => Some(Self::Two),
            b"3" => Some(Self::Three),
            _ => None,
        }
    }
}

pub(crate) enum Record {
    Uuid(uuid::Uuid),
    Rev(RevRecord),
    Node(NodeRecord),
}

pub(crate) struct RevRecord {
    pub(crate) rev_no: u32,
    pub(crate) properties: Option<HashMap<Vec<u8>, Vec<u8>>>,
}

pub(crate) struct NodeRecord {
    pub(crate) path: Vec<u8>,
    pub(crate) kind: Option<NodeKind>,
    pub(crate) action: NodeAction,
    pub(crate) copy_from: Option<NodeCopyFrom>,
    pub(crate) properties: Option<NodeProperties>,
    pub(crate) text: Option<NodeText>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeAction {
    Change,
    Add,
    Delete,
    Replace,
}

impl NodeAction {
    fn parse(s: &[u8]) -> Option<Self> {
        match s {
            b"change" => Some(Self::Change),
            b"add" => Some(Self::Add),
            b"delete" => Some(Self::Delete),
            b"replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeKind {
    File,
    Dir,
}

impl NodeKind {
    fn parse(s: &[u8]) -> Option<Self> {
        match s {
            b"file" => Some(Self::File),
            b"dir" => Some(Self::Dir),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct NodeCopyFrom {
    pub(crate) rev: u32,
    pub(crate) path: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct NodeProperties {
    pub(crate) is_delta: bool,
    pub(crate) properties: HashMap<Vec<u8>, Option<Vec<u8>>>,
}

pub(crate) struct NodeText {
    pub(crate) is_delta: bool,
}

fn parse_bool(s: &[u8]) -> Option<bool> {
    match s {
        b"true" => Some(true),
        b"false" => Some(false),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) enum ReadError {
    Io(std::io::Error),
    BrokenHeader,
    InvalidVersion { version: Vec<u8> },
    MissingHeaderEntry { key: Vec<u8> },
    UnexpectedHeaderEntry { key: Vec<u8> },
    InvalidHeaderEntry { key: Vec<u8>, value: Vec<u8> },
    UnknownRecordType,
    MismatchedContentLen,
    BrokenProperties,
}

impl From<std::io::Error> for ReadError {
    #[inline]
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Io(ref e) => write!(f, "failed to read source: {e}"),
            Self::BrokenHeader => write!(f, "broken header"),
            Self::InvalidVersion { ref version } => {
                write!(f, "invalid version: \"{}\"", version.escape_ascii())
            }
            Self::MissingHeaderEntry { ref key } => {
                write!(f, "missing header entry: \"{}\"", key.escape_ascii())
            }
            Self::UnexpectedHeaderEntry { ref key } => {
                write!(f, "unexpected header entry: \"{}\"", key.escape_ascii())
            }
            Self::InvalidHeaderEntry { ref key, ref value } => write!(
                f,
                "invalid value header entry \"{}\": \"{}\"",
                key.escape_ascii(),
                value.escape_ascii(),
            ),
            Self::UnknownRecordType => write!(f, "unknown record type"),
            Self::MismatchedContentLen => write!(f, "mismatched content length"),
            Self::BrokenProperties => write!(f, "broken properties"),
        }
    }
}

pub(crate) struct DumpReader<'a> {
    source: &'a mut dyn std::io::BufRead,
    version: DumpVersion,
    rem_text_len: u64,
}

impl<'a> DumpReader<'a> {
    pub(crate) fn new(source: &'a mut dyn std::io::BufRead) -> Result<Self, ReadError> {
        let header = parse_header(source)?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;

        let version_key = b"SVN-fs-dump-format-version";
        let raw_version =
            header
                .get(version_key.as_slice())
                .ok_or_else(|| ReadError::MissingHeaderEntry {
                    key: version_key.to_vec(),
                })?;
        let version = DumpVersion::parse(raw_version).ok_or_else(|| ReadError::InvalidVersion {
            version: raw_version.clone(),
        })?;

        Ok(Self {
            source,
            version,
            rem_text_len: 0,
        })
    }

    fn has_deltas(&self) -> bool {
        match self.version {
            DumpVersion::Two => false,
            DumpVersion::Three => true,
        }
    }

    pub(crate) fn next_record(&mut self) -> Result<Option<Record>, ReadError> {
        assert_eq!(
            self.rem_text_len, 0,
            "text stream has not been consumed yet",
        );

        let Some(header) = parse_header(self.source)? else {
            return Ok(None);
        };

        let uuid_key = b"UUID";
        let rev_no_key = b"Revision-number";
        let node_path_key = b"Node-path";
        let prop_content_len_key = b"Prop-content-length";
        let text_content_len_key = b"Text-content-length";
        let content_len_key = b"Content-length";

        let raw_uuid = header.get(uuid_key.as_slice());
        let raw_rev_no = header.get(rev_no_key.as_slice());
        let raw_node_path = header.get(node_path_key.as_slice());

        let type_cnt = usize::from(raw_uuid.is_some())
            + usize::from(raw_rev_no.is_some())
            + usize::from(raw_node_path.is_some());
        if type_cnt != 1 {
            return Err(ReadError::UnknownRecordType);
        }

        if let Some(raw_uuid) = raw_uuid {
            let content_len = header
                .get(content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            if content_len.unwrap_or(0) != 0 {
                return Err(ReadError::MismatchedContentLen);
            }

            if let Ok(uuid) = uuid::Uuid::try_parse_ascii(raw_uuid) {
                Ok(Some(Record::Uuid(uuid)))
            } else {
                Err(ReadError::InvalidHeaderEntry {
                    key: uuid_key.to_vec(),
                    value: raw_uuid.clone(),
                })
            }
        } else if let Some(raw_rev_no) = raw_rev_no {
            let rev_no = std::str::from_utf8(raw_rev_no)
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| ReadError::InvalidHeaderEntry {
                    key: rev_no_key.to_vec(),
                    value: raw_rev_no.clone(),
                })?;

            let prop_content_len = header
                .get(prop_content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: prop_content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            let content_len = header
                .get(content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            if prop_content_len.unwrap_or(0) != content_len.unwrap_or(0) {
                return Err(ReadError::MismatchedContentLen);
            }

            let properties = prop_content_len
                .map(|prop_content_len| {
                    let mut prop_stream = std::io::Read::take(&mut self.source, prop_content_len);
                    match parse_properties(&mut prop_stream, false) {
                        Ok(props) => {
                            if prop_stream.limit() != 0 {
                                Err(ReadError::BrokenProperties)
                            } else {
                                Ok(props.into_iter().map(|(k, v)| (k, v.unwrap())).collect())
                            }
                        }
                        Err(e) => match e.kind() {
                            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
                                Err(ReadError::BrokenProperties)
                            }
                            _ => Err(ReadError::Io(e)),
                        },
                    }
                })
                .transpose()?;

            Ok(Some(Record::Rev(RevRecord { rev_no, properties })))
        } else if let Some(raw_node_path) = raw_node_path {
            let kind_key = b"Node-kind";
            let kind = header
                .get(kind_key.as_slice())
                .map(|raw| {
                    NodeKind::parse(raw).ok_or_else(|| ReadError::InvalidHeaderEntry {
                        key: kind_key.to_vec(),
                        value: raw.clone(),
                    })
                })
                .transpose()?;

            let action_key = b"Node-action";
            let raw_action =
                header
                    .get(action_key.as_slice())
                    .ok_or_else(|| ReadError::MissingHeaderEntry {
                        key: action_key.to_vec(),
                    })?;
            let action =
                NodeAction::parse(raw_action).ok_or_else(|| ReadError::InvalidHeaderEntry {
                    key: action_key.to_vec(),
                    value: raw_action.clone(),
                })?;

            let copy_from_rev_key = b"Node-copyfrom-rev";
            let copy_from_path_key = b"Node-copyfrom-path";

            let raw_copy_from_rev = header.get(copy_from_rev_key.as_slice());
            let raw_copy_from_path = header.get(copy_from_path_key.as_slice());
            let copy_from = match (raw_copy_from_rev, raw_copy_from_path) {
                (None, None) => None,
                (Some(raw_copy_from_rev), Some(raw_copy_from_path)) => {
                    let copy_from_rev = std::str::from_utf8(raw_copy_from_rev)
                        .ok()
                        .and_then(|s| s.parse::<u32>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: copy_from_rev_key.to_vec(),
                            value: raw_copy_from_rev.clone(),
                        })?;
                    Some(NodeCopyFrom {
                        rev: copy_from_rev,
                        path: raw_copy_from_path.clone(),
                    })
                }
                (Some(_), None) => {
                    return Err(ReadError::MissingHeaderEntry {
                        key: copy_from_path_key.to_vec(),
                    });
                }
                (None, Some(_)) => {
                    return Err(ReadError::MissingHeaderEntry {
                        key: copy_from_rev_key.to_vec(),
                    });
                }
            };

            let prop_content_len = header
                .get(prop_content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: prop_content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            let text_content_len = header
                .get(text_content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: text_content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            let content_len = header
                .get(content_len_key.as_slice())
                .map(|raw| {
                    std::str::from_utf8(raw)
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or_else(|| ReadError::InvalidHeaderEntry {
                            key: content_len_key.to_vec(),
                            value: raw.clone(),
                        })
                })
                .transpose()?;

            let expected_content_len = prop_content_len
                .unwrap_or(0)
                .checked_add(text_content_len.unwrap_or(0))
                .ok_or(ReadError::MismatchedContentLen)?;
            if content_len.unwrap_or(0) != expected_content_len {
                return Err(ReadError::MismatchedContentLen);
            }

            let can_have_deltas = self.has_deltas();
            let properties = prop_content_len
                .map(|prop_content_len| {
                    let prop_delta_key = b"Prop-delta";
                    let prop_delta = header
                        .get(prop_delta_key.as_slice())
                        .map(|raw| {
                            parse_bool(raw).ok_or_else(|| ReadError::InvalidHeaderEntry {
                                key: prop_delta_key.to_vec(),
                                value: raw.clone(),
                            })
                        })
                        .transpose()?;

                    let mut has_deltas = false;
                    if let Some(prop_delta) = prop_delta {
                        if can_have_deltas {
                            has_deltas = prop_delta;
                        } else {
                            return Err(ReadError::UnexpectedHeaderEntry {
                                key: prop_delta_key.to_vec(),
                            });
                        }
                    }

                    let mut prop_stream = (&mut self.source).take(prop_content_len);
                    match parse_properties(&mut prop_stream, has_deltas) {
                        Ok(props) => {
                            if prop_stream.limit() != 0 {
                                Err(ReadError::BrokenProperties)
                            } else {
                                Ok(NodeProperties {
                                    is_delta: has_deltas,
                                    properties: props,
                                })
                            }
                        }
                        Err(e) => match e.kind() {
                            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof => {
                                Err(ReadError::BrokenProperties)
                            }
                            _ => Err(ReadError::Io(e)),
                        },
                    }
                })
                .transpose()?;

            let text = text_content_len
                .map(|text_content_len| {
                    let text_delta_key = b"Text-delta";
                    let text_delta = header
                        .get(text_delta_key.as_slice())
                        .map(|raw| {
                            parse_bool(raw).ok_or_else(|| ReadError::InvalidHeaderEntry {
                                key: text_delta_key.to_vec(),
                                value: raw.clone(),
                            })
                        })
                        .transpose()?;

                    let mut has_deltas = false;
                    if let Some(text_delta) = text_delta {
                        if can_have_deltas {
                            has_deltas = text_delta;
                        } else {
                            return Err(ReadError::UnexpectedHeaderEntry {
                                key: text_delta_key.to_vec(),
                            });
                        }
                    }

                    self.rem_text_len = text_content_len;
                    Ok(NodeText {
                        is_delta: has_deltas,
                    })
                })
                .transpose()?;

            Ok(Some(Record::Node(NodeRecord {
                path: raw_node_path.clone(),
                kind,
                action,
                copy_from,
                properties,
                text,
            })))
        } else {
            Err(ReadError::UnknownRecordType)
        }
    }

    #[inline]
    pub(crate) fn remaining_text_len(&self) -> u64 {
        self.rem_text_len
    }

    pub(crate) fn read_text(&mut self, buf: &mut [u8]) -> Result<(), std::io::Error> {
        let len_u64 = u64::try_from(buf.len())
            .ok()
            .filter(|&l| l <= self.rem_text_len)
            .expect("buffer too large");
        self.source.read_exact(buf)?;
        self.rem_text_len -= len_u64;
        Ok(())
    }
}

type RecordHeader = HashMap<Vec<u8>, Vec<u8>>;

fn parse_header(r: &mut dyn std::io::BufRead) -> Result<Option<RecordHeader>, ReadError> {
    let mut buf = Vec::new();
    r.read_until(b'\n', &mut buf)?;
    while buf == b"\n" {
        buf.clear();
        r.read_until(b'\n', &mut buf)?;
    }
    if buf.is_empty() {
        return Ok(None);
    }
    let mut map = HashMap::new();
    while buf != b"\n" {
        let line = buf.strip_suffix(b"\n").ok_or(ReadError::BrokenHeader)?;

        let sep_pos = line
            .windows(2)
            .position(|n| n == b": ")
            .ok_or(ReadError::BrokenHeader)?;
        map.insert(line[..sep_pos].to_vec(), line[(sep_pos + 2)..].to_vec());

        buf.clear();
        r.read_until(b'\n', &mut buf)?;
    }

    Ok(Some(map))
}

type Properties = HashMap<Vec<u8>, Option<Vec<u8>>>;

fn parse_properties(
    r: &mut dyn std::io::BufRead,
    is_delta: bool,
) -> Result<Properties, std::io::Error> {
    let mut buf = Vec::new();
    let mut props = HashMap::new();
    loop {
        buf.clear();
        r.read_until(b'\n', &mut buf)?;
        let line = buf
            .strip_suffix(b"\n")
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;

        if line == b"PROPS-END" {
            break;
        }

        if let Some(line_rem) = line.strip_prefix(b"K ") {
            let key_len = std::str::from_utf8(line_rem)
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;

            let mut key = vec![0; key_len];
            r.read_exact(&mut key)?;

            let mut tmp = [0];
            r.read_exact(&mut tmp)?;
            if tmp != *b"\n" {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }

            buf.clear();
            r.read_until(b'\n', &mut buf)?;
            let line = buf
                .strip_suffix(b"\n")
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))?;

            let value_len = line
                .strip_prefix(b"V ")
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;

            let mut value = vec![0; value_len];
            r.read_exact(&mut value)?;

            r.read_exact(&mut tmp)?;
            if tmp != *b"\n" {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }

            props.insert(key, Some(value));
        } else if let Some(line_rem) = line.strip_prefix(b"D ") {
            if !is_delta {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }

            let key_len = std::str::from_utf8(line_rem)
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;

            let mut key = vec![0; key_len];
            r.read_exact(&mut key)?;

            let mut tmp = [0];
            r.read_exact(&mut tmp)?;
            if tmp != *b"\n" {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
            }

            props.insert(key, None);
        } else {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
    }

    Ok(props)
}
