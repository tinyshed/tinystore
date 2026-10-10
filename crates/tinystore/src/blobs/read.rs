//! A file as a read finds it: its fields and the bytes it held then, which a
//! replace, a delete or an expiry after do not change.

use std::io::{self, Read, Seek, SeekFrom};

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::change::hidden;
use super::disk::{being_removed, read_at};
use super::files::Files;
use super::info::{FileInfo, FileRow, file_columns};
use crate::sqlite::sql_error;
use crate::{Error, Result};

/// Lookups a `get` makes while the path changes under it.
const LOOKUPS: usize = 3;

const SELECT_OPEN: &str = concat!(
    "select ",
    file_columns!(),
    ", f.content, c.id, c.inline, c.damaged, b.bytes
    from files as f left join contents as c on c.id = f.content
        left join bodies as b on c.inline and b.id = f.content
    where f.set_id = ?1 and f.path = ?2 and (f.expires is null or f.expires > ?3) and not ",
    hidden!()
);

/// A file and its bytes as [`Files::get`] found it. A whole read, from its
/// first byte to its last in order, is checked against the SHA-256 taken when
/// it was written, and its last bytes are held back until it matches; a range
/// is not checked.
pub struct StoredFile {
    info: FileInfo,
    body: Body,
    /// What an error calls the file: `files avatars: "1.png"`.
    label: String,
    sha256: Vec<u8>,
    /// Where the next read begins.
    position: u64,
    /// The hash of the bytes read in order from the first, and how far.
    hash: Sha256,
    hashed: u64,
}

enum Body {
    /// An inline file's bytes, checked when it was found.
    Inline(Vec<u8>),
    /// A file held open: the engine can remove it, and this reader reads on.
    File(std::fs::File),
}

impl std::fmt::Debug for StoredFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StoredFile({})", self.label)
    }
}

/// What a lookup of a path found: its row, and its content's, which a
/// missing content row leaves empty.
struct Found {
    row: FileRow,
    content: i64,
    named: Option<i64>,
    inline: Option<bool>,
    damaged: Option<bool>,
    body: Option<Vec<u8>>,
}

impl StoredFile {
    /// The file at a whole path and its bytes. A file gone between the lookup
    /// and its open went because its path changed, so the path is looked up
    /// again, three lookups in all.
    pub(crate) fn open(files: &Files, whole: &str) -> Result<Option<StoredFile>> {
        let mut failed = None;
        for _ in 0..LOOKUPS {
            let Some(found) = lookup(files, whole)? else { return Ok(None) };
            if failed == Some(found.content) {
                return Err(Error::corrupt(format!("the file of content {} is missing", found.content)));
            }
            match StoredFile::of(files, whole, found) {
                Err(Gone(content)) => failed = Some(content),
                Ok(opened) => return opened.map(Some),
            }
        }
        Err(Error::new(crate::ErrorKind::Conflict, format!("the path changed {LOOKUPS} times while it was read")))
    }

    fn of(files: &Files, whole: &str, found: Found) -> std::result::Result<Result<StoredFile>, Gone> {
        let content = found.content;
        let checked = (|| {
            if found.named.is_none() {
                return Err(Error::corrupt(format!("content {content}, which the file names, is missing")));
            }
            if found.damaged == Some(true) {
                return Err(Error::corrupt(format!("the scrub found content {content} changed or missing")));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            return Ok(Err(error));
        }
        let sha256 = found.row.sha256.clone();
        let size = found.row.size;
        let body = if found.inline == Some(true) {
            let body = found.body.unwrap_or_default();
            if i64::try_from(body.len()).ok() != Some(size) || Sha256::digest(&body).as_slice() != sha256.as_slice() {
                return Ok(Err(Error::corrupt(format!("the bytes of content {content} do not hash to its SHA-256"))));
            }
            Body::Inline(body)
        } else {
            match std::fs::File::open(files.blobs.disk.object_path(content)) {
                Ok(file) => Body::File(file),
                Err(error) if error.kind() == io::ErrorKind::NotFound || being_removed(&error) => {
                    return Err(Gone(content));
                }
                Err(error) => return Ok(Err(Error::io("blobs: open a file", error))),
            }
        };
        let info = match found.row.info(&files.folder) {
            Ok(info) => info,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(StoredFile {
            info,
            body,
            label: format!("{}: {whole:?}", files.describe()),
            sha256,
            position: 0,
            hash: Sha256::new(),
            hashed: 0,
        }))
    }

    pub fn info(&self) -> &FileInfo {
        &self.info
    }

    /// The whole file from its first byte, checked; it holds every byte in
    /// memory, so a large file is read as a stream.
    pub fn read_all(&mut self) -> Result<Vec<u8>> {
        self.seek(SeekFrom::Start(0)).map_err(|error| Error::io("blobs: read a file", error))?;
        let mut bytes = Vec::with_capacity(usize::try_from(self.info.size).unwrap_or(0));
        self.read_to_end(&mut bytes).map_err(|error| self.error_of(error))?;
        Ok(bytes)
    }

    /// Reads at `offset` without moving where the next read begins, from
    /// several threads at once; like any range it is not checked.
    pub fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        if offset >= self.info.size {
            return Ok(0);
        }
        let wanted = usize::try_from(self.info.size - offset).map_or(bytes.len(), |left| left.min(bytes.len()));
        let read = match &self.body {
            Body::Inline(body) => {
                let at = usize::try_from(offset).unwrap_or(usize::MAX);
                bytes[..wanted].copy_from_slice(&body[at..at + wanted]);
                wanted
            }
            Body::File(file) => read_at(file, &mut bytes[..wanted], offset)?,
        };
        if read == 0 && wanted > 0 {
            return Err(io::Error::other(self.corrupt("its file ends before it does")));
        }
        Ok(read)
    }

    /// Hashes the bytes of a read that carries on from the bytes hashed
    /// before it, and compares the hash once they reach the last byte:
    ///
    /// ```text
    /// read [0, 512), seek 0, read [0, 4096) → hashed to 4096, the first 512 once
    /// seek 1000, read                        → a range: not hashed
    /// ```
    fn check(&mut self, bytes: &[u8]) -> io::Result<()> {
        let end = self.position + bytes.len() as u64;
        if self.position > self.hashed || end <= self.hashed {
            return Ok(());
        }
        let from = usize::try_from(self.hashed - self.position).unwrap_or(0);
        self.hash.update(&bytes[from..]);
        self.hashed = end;
        if self.hashed == self.info.size && self.hash.clone().finalize().as_slice() != self.sha256.as_slice() {
            return Err(io::Error::other(self.corrupt("its bytes do not hash to its SHA-256")));
        }
        Ok(())
    }

    fn corrupt(&self, why: &str) -> Error {
        Error::corrupt(why).within(&self.label)
    }

    /// The store's error an `io::Error` of this reader carries, or one of its own.
    fn error_of(&self, error: io::Error) -> Error {
        match error.into_inner().map(|inner| inner.downcast::<Error>()) {
            Some(Ok(error)) => *error,
            Some(Err(inner)) => Error::io("blobs: read a file", io::Error::other(inner)).within(&self.label),
            None => Error::internal("blobs: a read failed").within(&self.label),
        }
    }
}

/// A file gone between its lookup and its open, by its content's id.
struct Gone(i64);

impl Read for StoredFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.info.size {
            return Ok(0);
        }
        let read = self.read_at(bytes, self.position)?;
        self.check(&bytes[..read])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for StoredFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::Current(by) => self.position.checked_add_signed(by),
            SeekFrom::End(by) => self.info.size.checked_add_signed(by),
        };
        let position =
            position.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a seek before the file's start"))?;
        self.position = position;
        Ok(position)
    }
}

fn lookup(files: &Files, whole: &str) -> Result<Option<Found>> {
    let now = files.blobs.now();
    files.blobs.file.read(|connection| {
        connection
            .prepare_cached(SELECT_OPEN)
            .and_then(|mut statement| {
                statement
                    .query_row(params![files.set, whole, now], |row| {
                        Ok(Found {
                            row: FileRow::read(row)?,
                            content: row.get(7)?,
                            named: row.get(8)?,
                            inline: row.get(9)?,
                            damaged: row.get(10)?,
                            body: row.get(11)?,
                        })
                    })
                    .optional()
            })
            .map_err(|error| sql_error("blobs: a file's lookup", error))
    })
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
