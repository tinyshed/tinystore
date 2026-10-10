//! What a file carries, as a head, a list and a read hand it back.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::SystemTime;

use super::engine::Blobs;
use super::paths::Folder;
use crate::{Error, Result};

/// A file's fields: what serving it needs, and nothing a query would search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileInfo {
    /// Its path, relative to the folder it was asked from.
    pub path: String,
    pub size: u64,
    /// The SHA-256 of its bytes in hex, quoted: an HTTP `ETag` as it is.
    pub etag: String,
    pub content_type: String,
    pub last_modified: SystemTime,
    pub expires: Option<SystemTime>,
    pub meta: BTreeMap<String, String>,
}

/// What a folder holds now: its files and their bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub count: u64,
    pub size: u64,
}

/// A page of a folder's files, in the byte order of their paths, and where
/// the next page starts when one follows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub files: Vec<FileInfo>,
    pub next: Option<String>,
}

/// The type a file without one is written with.
pub(crate) const OCTETS: &str = "application/octet-stream";

/// A file's row as a head or a list reads it.
pub(crate) struct FileRow {
    pub(crate) path: String,
    pub(crate) size: i64,
    pub(crate) sha256: Vec<u8>,
    pub(crate) content_type: String,
    pub(crate) modified: i64,
    pub(crate) expires: Option<i64>,
    pub(crate) meta: Option<String>,
}

/// The columns a [`FileRow`] reads, of a file aliased `f`.
macro_rules! file_columns {
    () => {
        "f.path, f.size, f.sha256, f.type, f.modified, f.expires, f.meta"
    };
}
pub(crate) use file_columns;

impl FileRow {
    pub(crate) fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
        Ok(FileRow {
            path: row.get(0)?,
            size: row.get(1)?,
            sha256: row.get(2)?,
            content_type: row.get(3)?,
            modified: row.get(4)?,
            expires: row.get(5)?,
            meta: row.get(6)?,
        })
    }

    pub(crate) fn info(self, folder: &Folder) -> Result<FileInfo> {
        Ok(FileInfo {
            path: folder.relative(&self.path).to_owned(),
            size: u64::try_from(self.size).map_err(|_| Error::corrupt(format!("a file of {} bytes", self.size)))?,
            etag: etag_of(&self.sha256),
            content_type: self.content_type,
            last_modified: Blobs::time(self.modified),
            expires: self.expires.map(Blobs::time),
            meta: meta_of(self.meta.as_deref())?,
        })
    }
}

/// The ETag of bytes whose SHA-256 is `sha256`: its hex, quoted.
pub(crate) fn etag_of(sha256: &[u8]) -> String {
    let mut etag = String::with_capacity(2 + sha256.len() * 2);
    etag.push('"');
    for byte in sha256 {
        let _ = write!(etag, "{byte:02x}");
    }
    etag.push('"');
    etag
}

/// Whether an ETag a caller gave names bytes whose SHA-256 is `sha256`;
/// quoted or not.
pub(crate) fn matches(given: &str, sha256: &[u8]) -> bool {
    given.trim_matches('"').eq_ignore_ascii_case(etag_of(sha256).trim_matches('"'))
}

pub(crate) fn meta_of(text: Option<&str>) -> Result<BTreeMap<String, String>> {
    text.map_or_else(
        || Ok(BTreeMap::new()),
        |text| serde_json::from_str(text).map_err(|error| Error::corrupt(format!("a file's meta: {error}"))),
    )
}

pub(crate) fn meta_text(meta: &BTreeMap<String, String>) -> Option<String> {
    (!meta.is_empty()).then(|| serde_json::to_string(meta).expect("a map of strings writes as JSON"))
}
