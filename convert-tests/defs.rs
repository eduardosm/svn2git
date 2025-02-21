use std::collections::{BTreeMap, BTreeSet};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Test {
    #[serde(rename = "svn-dump-source", default = "dump_source_uncompressed")]
    pub(crate) svn_dump_source: SvnDumpSource,
    #[serde(rename = "svn-dump-version", default = "dump_version_2")]
    pub(crate) svn_dump_version: SvnDumpVersion,
    #[serde(rename = "svn-uuid")]
    pub(crate) svn_uuid: Option<String>,
    #[serde(rename = "svn-revs")]
    pub(crate) svn_revs: Vec<SvnRev>,
    #[serde(rename = "conv-params")]
    pub(crate) conv_params: String,
    #[serde(rename = "user-map")]
    pub(crate) user_map: Option<String>,
    #[serde(rename = "git-repack", default = "false_")]
    pub(crate) git_repack: bool,
    #[serde(rename = "failed", default = "false_")]
    pub(crate) failed: bool,
    #[serde(rename = "logs")]
    pub(crate) logs: Option<String>,
    #[serde(rename = "git-tags", default = "Vec::new")]
    pub(crate) git_tags: Vec<GitTag>,
    #[serde(rename = "git-refs")]
    pub(crate) git_refs: Option<BTreeSet<String>>,
    #[serde(rename = "git-revs", default = "Vec::new")]
    pub(crate) git_revs: Vec<GitRev>,
}

#[derive(serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SvnDumpSource {
    #[serde(rename = "uncompressed")]
    Uncompressed,
    #[serde(rename = "compressed-gzip")]
    CompressedGzip,
    #[serde(rename = "compressed-bzip2")]
    CompressedBzip2,
    #[serde(rename = "compressed-xz")]
    CompressedXz,
    #[serde(rename = "compressed-zstd")]
    CompressedZstd,
    #[serde(rename = "compressed-lz4")]
    CompressedLz4,
}

#[inline(always)]
fn dump_source_uncompressed() -> SvnDumpSource {
    SvnDumpSource::Uncompressed
}

#[derive(serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SvnDumpVersion {
    #[serde(rename = "2")]
    Two,
    #[serde(rename = "3")]
    Three,
}

#[inline(always)]
fn dump_version_2() -> SvnDumpVersion {
    SvnDumpVersion::Two
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SvnRev {
    pub(crate) no: Option<u32>,
    #[serde(default = "BTreeMap::new")]
    pub(crate) props: BTreeMap<String, String>,
    #[serde(default = "Vec::new")]
    pub(crate) nodes: Vec<SvnNode>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SvnNode {
    pub(crate) path: String,
    pub(crate) kind: SvnNodeKind,
    pub(crate) action: SvnNodeAction,
    #[serde(rename = "copy-from-path")]
    pub(crate) copy_from_path: Option<String>,
    #[serde(rename = "copy-from-rev")]
    pub(crate) copy_from_rev: Option<u32>,
    #[serde(rename = "prop-delta")]
    pub(crate) prop_delta: Option<bool>,
    #[serde(rename = "text-delta")]
    pub(crate) text_delta: Option<bool>,
    pub(crate) props: Option<BTreeMap<String, Option<String>>>,
    pub(crate) text: Option<Bytes>,
}

#[derive(serde::Deserialize)]
pub(crate) enum SvnNodeKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "dir")]
    Dir,
}

#[derive(serde::Deserialize)]
pub(crate) enum SvnNodeAction {
    #[serde(rename = "change")]
    Change,
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "delete")]
    Delete,
    #[serde(rename = "replace")]
    Replace,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitTag {
    pub(crate) tag: String,
    pub(crate) rev: String,
    pub(crate) tagger: Option<GitSignature>,
    pub(crate) message: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitRev {
    pub(crate) rev: String,
    pub(crate) author: Option<GitSignature>,
    pub(crate) committer: Option<GitSignature>,
    pub(crate) message: Option<String>,
    pub(crate) same: Option<Vec<String>>,
    pub(crate) parents: Option<Vec<String>>,
    pub(crate) tree: Option<BTreeMap<String, GitTreeEntry>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitSignature {
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) time: Option<GitTime>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitTime {
    pub(crate) seconds: i64,
    pub(crate) offset: u32,
    pub(crate) sign: GitTimeSign,
}

#[derive(PartialEq, Eq, serde::Deserialize)]
pub(crate) enum GitTimeSign {
    #[serde(rename = "plus")]
    Plus,
    #[serde(rename = "minus")]
    Minus,
}

impl std::fmt::Display for GitTimeSign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitTimeSign::Plus => f.write_str("+"),
            GitTimeSign::Minus => f.write_str("-"),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum GitTreeEntry {
    #[serde(rename = "normal")]
    Normal { data: String },
    #[serde(rename = "exec")]
    Exec { data: String },
    #[serde(rename = "symlink")]
    Symlink { target: String },
    #[serde(rename = "dir")]
    Dir,
}

pub(crate) struct Bytes(Vec<u8>);

impl Bytes {
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl std::ops::Deref for Bytes {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl<'de> serde::Deserialize<'de> for Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BytesVisitor).map(Self)
    }
}

struct BytesVisitor;

impl<'de> serde::de::Visitor<'de> for BytesVisitor {
    type Value = Vec<u8>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a string or byte array")
    }

    fn visit_str<E>(self, v: &str) -> Result<Vec<u8>, E>
    where
        E: serde::de::Error,
    {
        Ok(v.as_bytes().to_vec())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into_bytes())
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.to_vec())
    }

    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(byte) = seq.next_element()? {
            bytes.push(byte);
        }
        Ok(bytes)
    }
}

#[inline(always)]
fn false_() -> bool {
    false
}
