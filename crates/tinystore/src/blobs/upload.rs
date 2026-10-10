//! A file written a piece at a time: its bytes stay in memory while it may
//! stay inline, then go to a file of `uploads/` as they come, hashed on the
//! way; `commit` publishes it whole, and an upload that ends otherwise leaves
//! nothing.

use std::io::{self, Write};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::change::{Condition, Content, Place, WhenThere};
use super::files::{Files, Options};
use super::info::FileInfo;
use crate::{Error, ErrorKind, Reservation, Result};

/// A content up to it is a row of `bodies`: inline wrote and read faster than
/// a file on Linux and on Windows up to it, and at 64 KiB a file read faster
/// (research blobs-mechanics-2026-09-27).
pub(crate) const INLINE: usize = 16 << 10;

/// An upload's bytes are synced this often, so that no commit waits on
/// gigabytes.
const SYNC_EVERY: u64 = 256 << 20;

/// How long an upload waits for the store's memory for its buffer.
const PATIENCE: Duration = Duration::from_secs(10);

/// A file written a piece at a time, as [`Files::upload`] begins it: bytes go
/// in with `write`, the file appears at `commit` and never before, and an
/// upload dropped without it leaves nothing.
pub struct Upload {
    files: Files,
    path: String,
    options: Options,
    /// The bytes while they may stay inline.
    buffer: Vec<u8>,
    /// The file of `uploads/` the bytes go to past the inline size.
    file: Option<std::fs::File>,
    id: Option<i64>,
    /// Whether the file was renamed into `objects/`, before or after its commit.
    placed: bool,
    hash: Sha256,
    written: u64,
    synced: u64,
    ended: bool,
    _reserved: Reservation,
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Upload({:?}, {} bytes)", self.path, self.written)
    }
}

impl Upload {
    pub(crate) fn start(files: Files, path: String, options: Options) -> Result<Upload> {
        let reserved = files.blobs.memory.reserve(INLINE as u64, "a file's upload", PATIENCE);
        let reserved = reserved.map_err(|error| files.fail(&path, error))?;
        let upload = Upload {
            files,
            path,
            options,
            buffer: Vec::new(),
            file: None,
            id: None,
            placed: false,
            hash: Sha256::new(),
            written: 0,
            synced: 0,
            ended: false,
            _reserved: reserved,
        };
        if let (Some(declared), Some(most)) = (upload.options.size, upload.files.max_file_size)
            && declared > most
        {
            return Err(upload.fail(past_its_bound(declared, most)));
        }
        Ok(upload)
    }

    /// Takes bytes into the upload: in memory while the file may stay inline,
    /// then into its file. An error ends the upload, which leaves nothing.
    pub(crate) fn take(&mut self, bytes: &[u8]) -> Result<()> {
        if self.ended {
            return Err(self.fail(Error::closed("the upload has ended")));
        }
        let taken = self.bound(bytes.len()).and_then(|()| {
            self.hash.update(bytes);
            self.written += bytes.len() as u64;
            if self.file.is_none() && self.fits_inline() {
                self.buffer.extend_from_slice(bytes);
                return Ok(());
            }
            if self.file.is_none() {
                self.spill()?;
            }
            self.write_file(bytes)
        });
        taken.map_err(|error| {
            self.end();
            self.fail(error)
        })
    }

    /// Publishes the file whole, replacing what the path held, and returns
    /// it once it is on disk as the store's durability says.
    pub fn commit(mut self) -> Result<FileInfo> {
        let condition = Condition { if_match: self.options.if_match.clone(), when_there: WhenThere::Replace };
        self.finish(condition)?.ok_or_else(|| Error::internal("a commit that wrote nothing"))
    }

    /// Ends the upload and leaves nothing; dropping it does the same.
    pub fn abort(mut self) {
        self.end();
    }

    /// Publishes the upload's bytes when `condition` holds; says what it
    /// wrote, or nothing when a `create` found a file. A committed upload
    /// lets its id go, a failed one removes its bytes, and one whose outcome
    /// is unknown asks the file whether its content is there first.
    pub(crate) fn finish(&mut self, condition: Condition) -> Result<Option<FileInfo>> {
        if self.ended {
            return Err(self.fail(Error::closed("the upload has ended")));
        }
        let finished = self
            .check_size()
            .and_then(|()| self.content())
            .and_then(|content| self.files.publish(&self.path, &self.options, content, condition));
        self.ended = true;
        let blobs = std::sync::Arc::clone(&self.files.blobs);
        match (&finished, self.id) {
            (Ok(Some(_)), Some(id)) => blobs.ids.let_go(id),
            (Err(error), Some(id)) if error.kind() == ErrorKind::OutcomeUnknown && self.placed => blobs.resolve(id),
            (_, Some(_)) => self.remove_bytes(),
            (_, None) => {}
        }
        finished.map_err(|error| self.fail(error))
    }

    /// The content the upload's bytes are: inline, its body with it, or its
    /// file synced and renamed into `objects/`, so that the commit names bytes
    /// whose name is durable.
    fn content(&mut self) -> Result<Content> {
        let sha256 = self.hash.clone().finalize().to_vec();
        let size = i64::try_from(self.written).map_err(|_| Error::limit("a file past 2⁶³ bytes"))?;
        let Some(file) = self.file.take() else {
            let id = self.files.blobs.take_id()?;
            self.id = Some(id);
            return Ok(Content { id, size, sha256, place: Place::Inline(std::mem::take(&mut self.buffer)) });
        };
        let id = self.id.ok_or_else(|| Error::internal("an upload's file without its id"))?;
        if self.files.blobs.disk.durable() {
            file.sync_data().map_err(|error| Error::io("blobs: sync an upload", error))?;
        }
        drop(file);
        self.files.blobs.disk.place(id)?;
        self.placed = true;
        Ok(Content { id, size, sha256, place: Place::File })
    }

    /// Refuses bytes past the declared size or the files' bound.
    fn bound(&self, more: usize) -> Result<()> {
        let after = self.written + more as u64;
        if let Some(declared) = self.options.size.filter(|declared| after > *declared) {
            return Err(Error::invalid(format!("a body longer than its size of {declared} bytes")));
        }
        if let Some(most) = self.files.max_file_size.filter(|most| after > *most) {
            return Err(past_its_bound(after, most));
        }
        Ok(())
    }

    fn check_size(&self) -> Result<()> {
        match self.options.size {
            Some(declared) if declared != self.written => {
                Err(Error::invalid(format!("a body of {} bytes, shorter than its size of {declared}", self.written)))
            }
            _ => Ok(()),
        }
    }

    fn fits_inline(&self) -> bool {
        self.options.size.is_none_or(|declared| declared <= INLINE as u64) && self.written <= INLINE as u64
    }

    /// Gives the upload its file: an id held, its name in `uploads/`, and the
    /// bytes gathered so far.
    fn spill(&mut self) -> Result<()> {
        let id = self.files.blobs.take_id()?;
        self.id = Some(id);
        self.file = Some(self.files.blobs.disk.create_upload(id)?);
        let gathered = std::mem::take(&mut self.buffer);
        self.write_file(&gathered)
    }

    fn write_file(&mut self, bytes: &[u8]) -> Result<()> {
        let durable = self.files.blobs.disk.durable();
        let file = self.file.as_mut().ok_or_else(|| Error::internal("an upload without its file"))?;
        file.write_all(bytes).map_err(|error| Error::io("blobs: write an upload", error))?;
        if durable && self.written - self.synced >= SYNC_EVERY {
            file.sync_data().map_err(|error| Error::io("blobs: sync an upload", error))?;
            self.synced = self.written;
        }
        Ok(())
    }

    /// Ends the upload, its bytes removed wherever they lie.
    fn end(&mut self) {
        if !self.ended {
            self.ended = true;
            self.remove_bytes();
        }
    }

    /// Removes the upload's bytes and lets its id go. A file of `objects/`
    /// that will not go keeps its id held, so that the settled mark stays
    /// below it until maintenance or the next open removes it.
    fn remove_bytes(&mut self) {
        self.buffer = Vec::new();
        self.file = None;
        let Some(id) = self.id.take() else { return };
        let blobs = &self.files.blobs;
        let removed = if self.placed { blobs.disk.remove_object(id) } else { blobs.disk.remove_upload(id) };
        match removed {
            Ok(()) => blobs.ids.let_go(id),
            Err(_) if self.placed => blobs.ids.doubt(id),
            Err(_) => blobs.ids.let_go(id),
        }
    }

    fn fail(&self, error: Error) -> Error {
        self.files.fail(&self.path, error)
    }
}

impl Write for Upload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.take(bytes).map(|()| bytes.len()).map_err(io::Error::other)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        self.end();
    }
}

fn past_its_bound(bytes: u64, most: u64) -> Error {
    Error::limit(format!("a file of {bytes} bytes, past the files' bound of {most}"))
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
