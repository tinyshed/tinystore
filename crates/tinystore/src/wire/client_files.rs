//! A shell's writes to a set of files: `files put` and `files delete`.

use std::io::{Read, Write};

use super::{Client, PIECE};
use crate::blobs::FileInfo;
use crate::clock::from_unix_millis;
use crate::wire::codec::Message as _;
use crate::wire::protocol::{self as wire, method};
use crate::{Error, Result};

/// What a write carries beside its path and its bytes.
const WRITE_FIELDS: usize = 256;

impl<S: Read + Write> Client<S> {
    /// The handle of the set of files `name`, made the first time.
    pub fn files(&mut self, name: &str) -> Result<u64> {
        self.handle(method::BLOBS_OPEN, &wire::BlobsOpen { name: name.to_owned(), ..wire::BlobsOpen::default() })
    }

    /// Writes the file at `path` whole, its `size` bytes read from `bytes`: in
    /// the request when they fit one, in pieces after it otherwise.
    pub fn put(
        &mut self,
        files: u64,
        path: &str,
        (bytes, size): (&mut dyn Read, u64),
        content_type: Option<&str>,
    ) -> Result<FileInfo> {
        let mut write = wire::BlobsWrite {
            handle: files,
            path: path.to_owned(),
            content_type: content_type.map(str::to_owned),
            size: Some(size),
            ..wire::BlobsWrite::default()
        };
        let answer = if size as usize + path.len() + WRITE_FIELDS <= self.max_body {
            bytes.read_to_end(&mut write.bytes).map_err(|error| Error::io("the file's bytes", error))?;
            self.call(method::BLOBS_PUT, &write)?
        } else {
            let mut piece = vec![0; PIECE];
            self.exchange(method::BLOBS_UPLOAD, &write, &mut || {
                let read = bytes.read(&mut piece).map_err(|error| Error::io("the file's bytes", error))?;
                Ok((read > 0).then(|| wire::BlobsPiece { bytes: piece[..read].to_vec() }.encode()))
            })?
        };
        let written = super::decoded::<wire::BlobsWritten>(&answer)?;
        let info = written.info.ok_or_else(|| Error::corrupt("a put the server answered with no file"))?;
        Ok(FileInfo {
            path: info.path,
            size: info.size,
            etag: info.etag,
            content_type: info.content_type,
            last_modified: from_unix_millis(info.last_modified),
            expires: info.expires.map(from_unix_millis),
            meta: info.meta.unwrap_or_default(),
        })
    }

    /// Deletes the file at `path`: whether there was one.
    pub fn delete_file(&mut self, files: u64, path: &str) -> Result<bool> {
        let at = wire::BlobsAt { handle: files, folder: Vec::new(), path: path.to_owned() };
        if self.answer::<wire::BlobsHead>(method::BLOBS_HEAD, &at)?.info.is_none() {
            return Ok(false);
        }
        let delete = wire::BlobsDelete { handle: files, path: path.to_owned(), ..wire::BlobsDelete::default() };
        self.call(method::BLOBS_DELETE, &delete).map(|_| true)
    }
}
