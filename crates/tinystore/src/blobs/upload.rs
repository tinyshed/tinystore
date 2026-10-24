//! A file written a piece at a time: its bytes stay in memory while it may
//! stay inline, then go to a file of `uploads/` as they come, hashed on the
//! way; `commit` publishes it whole, and an upload that ends otherwise leaves
//! nothing.

use std::io::{self, Write};

use sha2::{Digest, Sha256};

use super::change::{Condition, Content, Place, WhenThere};
#[cfg(feature = "async")]
use super::files::Done;
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

/// How often an upload asks the disk how much is free as its bytes go to it.
const ROOM_EVERY: u64 = 64 << 20;

use crate::memory::PATIENCE;

/// The memory an upload holds from its start to its end: its bytes while
/// they may stay inline.
const MEMORY: u64 = INLINE as u64;
/// What a refusal of that memory names.
const WHAT: &str = "a file's upload";

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
    /// Whether its bytes went to a file of `uploads/`.
    spilled: bool,
    /// Whether the file was renamed into `objects/`, before or after its commit.
    placed: bool,
    hash: Sha256,
    written: u64,
    synced: u64,
    /// The bytes its file holds.
    on_disk: u64,
    /// `written` when the disk was last asked for room.
    roomed: u64,
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
        let reserved = files.blobs.memory.reserve(MEMORY, WHAT, PATIENCE);
        let reserved = reserved.map_err(|error| files.fail(&path, error))?;
        Upload::reserved(files, path, options, reserved)
    }

    /// Takes the memory an upload holds as [`Upload::start`] does, waiting on
    /// no thread: `done` is given it once it fits, on whichever thread gives
    /// memory back, or here when it fits now. What this returns takes the
    /// waiter out of line when it is dropped unanswered.
    #[cfg(feature = "async")]
    pub(super) fn reserve_then(
        files: &Files,
        done: impl FnOnce(Result<Reservation>) + Send + 'static,
    ) -> crate::memory::Waiting {
        files.blobs.memory.reserve_then(MEMORY, WHAT, PATIENCE, done)
    }

    /// An upload that holds `reserved`, the memory it takes as it starts.
    pub(crate) fn reserved(files: Files, path: String, options: Options, reserved: Reservation) -> Result<Upload> {
        let upload = Upload {
            files,
            path,
            options,
            buffer: Vec::new(),
            file: None,
            id: None,
            spilled: false,
            placed: false,
            hash: Sha256::new(),
            written: 0,
            synced: 0,
            on_disk: 0,
            roomed: 0,
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
            #[cfg(feature = "replication")]
            if self.files.is_copy() {
                return self.gather(bytes);
            }
            if self.file.is_none() && self.fits_inline_after(0) {
                self.buffer.extend_from_slice(bytes);
                return Ok(());
            }
            self.keep_room()?;
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

    /// Whether taking `more` bytes keeps them in memory and touches no disk:
    /// what an awaited upload takes in its poll, the rest on the store's pool.
    #[cfg(feature = "async")]
    pub(crate) fn stays_in_memory(&self, more: usize) -> bool {
        self.ended || (!self.spilled && self.fits_inline_after(more as u64))
    }

    /// Publishes the file, or with `create` only where no file is; says what
    /// it wrote, nothing when a create found a file. The wire's last DATA,
    /// and an awaited commit on the store's pool.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn publish(mut self, create: bool) -> Result<Option<FileInfo>> {
        let condition = self.condition(create);
        self.finish(condition)
    }

    /// Publishes the file whole, replacing what the path held, and returns
    /// it once it is on disk as the store's durability says.
    pub fn commit(mut self) -> Result<FileInfo> {
        let condition = self.condition(false);
        self.finish(condition)?.ok_or_else(|| Error::internal("a commit that wrote nothing"))
    }

    /// A content id for the upload's bytes, taken now, when they are inline
    /// and the block reserved has one left: what lets an awaited commit
    /// submit in its poll. None when the bytes need the disk, a copy's home or
    /// the next block of ids, which are the store's pool's.
    #[cfg(feature = "async")]
    pub(crate) fn inline_id(&mut self) -> Option<i64> {
        #[cfg(feature = "replication")]
        if self.files.is_copy() {
            return None;
        }
        if self.ended || self.spilled {
            return None;
        }
        let id = self.files.blobs.ids.try_take()?;
        self.id = Some(id);
        Some(id)
    }

    /// Publishes the upload's inline bytes under the id [`Upload::inline_id`]
    /// took, as [`Upload::publish`] does, submitted: `done` is given what it
    /// wrote once its commit ends, on the thread that answers the commit. The
    /// upload holds its memory until then, and lets its id go whatever the
    /// outcome, since no file holds its bytes.
    #[cfg(feature = "async")]
    pub(crate) fn publish_inline_then(mut self, create: bool, done: impl Done<Option<FileInfo>>) {
        let ready = self.id.ok_or_else(|| Error::internal("an inline publication without its id")).and_then(|id| {
            self.check_size()?;
            Ok((id, self.checksum()?))
        });
        let (id, (sha256, size)) = match ready {
            Ok(ready) => ready,
            Err(error) => {
                self.end();
                return done(Err(self.fail(error)));
            }
        };
        self.ended = true;
        let content = Content { id, size, sha256, place: Place::Inline(std::mem::take(&mut self.buffer)) };
        let condition = self.condition(create);
        let (bytes, publication) = self.files.publication(&self.path, &self.options, content, condition);
        let blobs = std::sync::Arc::clone(&self.files.blobs);
        blobs.file.submit(bytes, publication, move |published| {
            self.files.blobs.ids.let_go(id);
            if let Ok((info, _)) = &published {
                self.files.count_written(info.as_ref());
            }
            done(published.map_err(|error| self.fail(error)));
        });
    }

    fn condition(&self, create: bool) -> Condition {
        let when_there = if create { WhenThere::Skip } else { WhenThere::Replace };
        Condition { if_match: self.options.if_match.clone(), when_there }
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
        #[cfg(feature = "replication")]
        if self.files.is_copy() {
            let passed = self.check_size().and_then(|()| self.pass_on(condition));
            self.ended = true;
            self.buffer = Vec::new();
            return passed.map_err(|error| self.fail(error));
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
        let (sha256, size) = self.checksum()?;
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
        // placed before its directory's sync, so that a sync that fails removes
        // the file from objects/, where no walk at or below the settled mark goes
        self.placed = true;
        self.files.blobs.disk.sync_placed(id)?;
        Ok(Content { id, size, sha256, place: Place::File })
    }

    /// The SHA-256 of the bytes taken, and how many they are.
    fn checksum(&self) -> Result<(Vec<u8>, i64)> {
        let size = i64::try_from(self.written).map_err(|_| Error::limit("a file past 2⁶³ bytes"))?;
        Ok((self.hash.clone().finalize().to_vec(), size))
    }

    /// Gathers a copy's bytes, which go to its home whole at the commit.
    #[cfg(feature = "replication")]
    fn gather(&mut self, bytes: &[u8]) -> Result<()> {
        if self.written > super::forward::GATHERED {
            return Err(Error::limit(format!(
                "a file past {} MiB written on a copy, whose home takes it whole: write it at the home",
                super::forward::GATHERED >> 20
            )));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Passes a copy's file to its home, and reads what it holds there once
    /// this copy holds the commit.
    #[cfg(feature = "replication")]
    fn pass_on(&self, condition: Condition) -> Result<Option<FileInfo>> {
        let create = matches!(condition.when_there, WhenThere::Skip);
        let options = Options { if_match: condition.if_match, ..self.options.clone() };
        let call = super::forward::Call::Put { path: &self.path, options: &options, bytes: &self.buffer, create };
        if !self.files.pass_on(&call)? {
            return Ok(None);
        }
        self.files
            .head_here(&self.path)?
            .map(Some)
            .ok_or_else(|| Error::internal("a file its home wrote that this copy lacks"))
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

    /// Refuses bytes that would leave the disk less free than the store keeps:
    /// asked as the upload first goes to a file, for what its declared size
    /// still brings, and again every ROOM_EVERY bytes. What is stored already
    /// is never removed to make room.
    fn keep_room(&mut self) -> Result<()> {
        if self.file.is_some() && self.written - self.roomed < ROOM_EVERY {
            return Ok(());
        }
        self.roomed = self.written;
        let coming = self.options.size.unwrap_or(0).max(self.written) - self.on_disk;
        self.files.blobs.room(coming)
    }

    /// Whether the bytes taken and `more` stay inline, as the size declared
    /// lets them.
    fn fits_inline_after(&self, more: u64) -> bool {
        let inline = INLINE as u64;
        self.options.size.is_none_or(|declared| declared <= inline) && self.written.saturating_add(more) <= inline
    }

    /// Gives the upload its file: an id held, its name in `uploads/`, and the
    /// bytes gathered so far.
    fn spill(&mut self) -> Result<()> {
        let id = self.files.blobs.take_id()?;
        self.id = Some(id);
        // before its creation, so that one that failed half way is removed as well
        self.spilled = true;
        self.file = Some(self.files.blobs.disk.create_upload(id)?);
        let gathered = std::mem::take(&mut self.buffer);
        self.write_file(&gathered)
    }

    fn write_file(&mut self, bytes: &[u8]) -> Result<()> {
        let durable = self.files.blobs.disk.durable();
        let file = self.file.as_mut().ok_or_else(|| Error::internal("an upload without its file"))?;
        file.write_all(bytes).map_err(|error| Error::io("blobs: write an upload", error))?;
        self.on_disk += bytes.len() as u64;
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
    /// below it until maintenance or the next open removes it. Bytes that
    /// stayed in memory touch no disk.
    fn remove_bytes(&mut self) {
        self.buffer = Vec::new();
        self.file = None;
        let Some(id) = self.id.take() else { return };
        let blobs = &self.files.blobs;
        let removed = match (self.placed, self.spilled) {
            (true, _) => blobs.disk.remove_object(id),
            (false, true) => blobs.disk.remove_upload(id),
            (false, false) => Ok(()),
        };
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
