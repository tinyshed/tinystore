//! A file written a piece at a time, awaited: its pieces taken in the poll
//! while the file may stay inline, then on the store's pool in a line of the
//! upload's own, so that they land in the order the program gave them.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{answering, describe, label};
use crate::aio::oneshot::pending;
use crate::aio::run;
use crate::blobs::FileInfo;
use crate::polled::{self, Lead};
use crate::pool::Pool;
use crate::sync::lock;
use crate::{Error, Result};

/// A file written a piece at a time, as an awaited `upload` begins it: bytes
/// go in with `write`, the file appears at `commit` and never before, and an
/// upload dropped without it leaves nothing.
pub struct Upload {
    /// The blocking upload, which a piece on the pool holds while it writes,
    /// and none once its commit took it.
    upload: Arc<Mutex<Option<crate::blobs::Upload>>>,
    files: crate::blobs::Files,
    pool: Arc<Pool>,
    lead: Lead,
    /// The pool's line its pieces are written in, from the first that went
    /// to the pool: each call after it waits there for the one before.
    line: Option<String>,
    /// Its path within the files' folder, as the program gave it.
    path: String,
}

impl fmt::Debug for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Upload({})", label(&self.files, &self.path))
    }
}

impl Upload {
    pub(super) fn new(
        upload: crate::blobs::Upload,
        files: crate::blobs::Files,
        path: String,
        pool: Arc<Pool>,
    ) -> Upload {
        let lead = run::lead_on(&pool);
        Upload { upload: Arc::new(Mutex::new(Some(upload))), files, pool, lead, line: None, path }
    }

    /// Takes the whole piece into the upload: in memory, in the poll, while
    /// the file may stay inline, then on the store's pool into its file of
    /// `uploads/`. Once polled its piece is written whether or not anyone
    /// still awaits it, and the next call waits for it. An error ends the
    /// upload, which leaves nothing.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.line.is_none() {
            if let Some(taken) = self.take_in_memory(bytes) {
                return taken;
            }
            self.line = Some(next_line());
        }
        let (upload, piece) = (Arc::clone(&self.upload), bytes.to_vec());
        let take = move || lock(&upload).as_mut().ok_or_else(ended)?.take(&piece);
        run::blocking(&self.pool, self.line.clone(), self.what("a write"), take).await
    }

    /// Publishes the file whole, replacing what the path held, once it is on
    /// disk as the store's durability says: submitted in the poll when its
    /// bytes stayed inline, synced, renamed and committed on the store's pool
    /// otherwise, and on a copy passed to its home from a thread of its own.
    /// Once polled it commits whether or not anyone still awaits it; dropped
    /// unpolled, the upload aborts.
    pub async fn commit(mut self) -> Result<FileInfo> {
        let what = self.what("a commit");
        let published = if let Some(upload) = self.inline_now() {
            let (answer, answered) = pending(what);
            let done = answering(&self.files, &self.pool, answer);
            polled::leading_on(&self.lead, || upload.publish_inline_then(false, done));
            answered.await
        } else if super::passed_home(&self.files) {
            let line = self.line.get_or_insert_with(next_line).clone();
            let upload = Arc::clone(&self.upload);
            let take = move || lock(&upload).take().ok_or_else(ended);
            let publish = |upload: crate::blobs::Upload| upload.publish(false);
            run::passed_on_in_line(&self.pool, line, what, (take, publish)).await
        } else {
            let line = self.line.get_or_insert_with(next_line).clone();
            let upload = Arc::clone(&self.upload);
            let publish = move || lock(&upload).take().ok_or_else(ended)?.publish(false);
            run::blocking(&self.pool, Some(line), what, publish).await
        };
        published?.ok_or_else(|| Error::internal("a commit that wrote nothing"))
    }

    /// Ends the upload and leaves nothing; dropping it does the same. A file
    /// of `uploads/` is removed on the store's pool, after the pieces still
    /// being written.
    pub fn abort(self) {
        drop(self);
    }

    /// Takes `bytes` in the poll when they stay in memory. Before the first
    /// piece goes to the pool nothing else holds the upload, so its lock is
    /// free here.
    fn take_in_memory(&self, bytes: &[u8]) -> Option<Result<()>> {
        let mut held = lock(&self.upload);
        let upload = held.as_mut()?;
        upload.stays_in_memory(bytes.len()).then(|| upload.take(bytes))
    }

    /// What its errors call a call on it: `files reports: "report.csv": a write`.
    fn what(&self, verb: &str) -> String {
        describe(&self.files, &self.path, verb)
    }

    /// The upload, taken to commit in the poll, when its bytes stayed in
    /// memory and a content id is free now.
    fn inline_now(&mut self) -> Option<crate::blobs::Upload> {
        if self.line.is_some() {
            return None;
        }
        let mut held = lock(&self.upload);
        held.as_mut()?.inline_id()?;
        held.take()
    }
}

impl Drop for Upload {
    /// An upload that never went to the pool holds its bytes in memory, and
    /// drops them here; one that did is ended on the pool, in its line.
    fn drop(&mut self) {
        let Some(line) = &self.line else { return };
        let upload = Arc::clone(&self.upload);
        self.pool.run_in_line(line, Box::new(move || drop(lock(&upload).take())));
    }
}

fn ended() -> Error {
    Error::closed("the upload has ended")
}

/// A line of the pool no other upload writes in.
fn next_line() -> String {
    static UPLOADS: AtomicU64 = AtomicU64::new(0);
    format!("blobs: an upload, {}", UPLOADS.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
