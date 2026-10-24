//! A file as an awaited get finds it, its bytes read on the store's pool, or
//! in the poll when they are kept inline, and handed out as a `Stream`.

use std::fmt;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::task::{Context, Poll, ready};

use futures_core::Stream;

use crate::Result;
use crate::aio::run;
use crate::blobs::{FileInfo, PIECE, position_after};
use crate::pool::Pool;

/// A file and its bytes as an awaited `get` found it, which stay what they
/// were then through a replace, a delete or an expiry.
///
/// It is a `Stream` of its bytes, 256 KiB a piece, from where
/// [`StoredFile::seek`] put it: from the first byte it is checked against the
/// SHA-256 taken when it was written, its last piece held back until it
/// matches and a changed byte failing `corrupt`; from anywhere else it is a
/// range, and not checked. The stream ends after an error, until a seek.
///
/// A file on disk is read on the store's pool, a piece at a time, and once
/// the store closed its stream ends with `Closed`; one kept inline is read
/// in the poll, from memory.
pub struct StoredFile {
    info: FileInfo,
    /// The blocking reader, which the pool's reads and a seek's next read
    /// share: a read in order takes it alone, as its hash moves with it.
    file: Arc<RwLock<crate::blobs::StoredFile>>,
    inline: bool,
    /// What its errors call it, as the blocking reader's do:
    /// `files avatars: "users/42/1.png"`.
    label: String,
    pool: Arc<Pool>,
    /// Where the stream's next piece begins.
    position: u64,
    /// The piece the stream reads on the pool now.
    piece: Option<Piece>,
    ended: bool,
}

/// A piece of a file's stream, as it is read.
type Piece = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send>>;

impl fmt::Debug for StoredFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StoredFile({}, at {})", self.label, self.position)
    }
}

impl StoredFile {
    pub(super) fn new(file: crate::blobs::StoredFile, pool: Arc<Pool>) -> StoredFile {
        let (info, inline, label) = (file.info().clone(), file.is_inline(), file.label().to_owned());
        let file = Arc::new(RwLock::new(file));
        StoredFile { info, file, inline, label, pool, position: 0, piece: None, ended: false }
    }

    pub fn info(&self) -> &FileInfo {
        &self.info
    }

    /// The whole file from its first byte, checked; it holds every byte in
    /// memory, so a large file is read as a stream. It leaves where the
    /// stream reads next as it was. Dropped, it stops between two pieces of
    /// 256 KiB.
    pub fn read_all(&self) -> impl Future<Output = Result<Vec<u8>>> + Send + 'static {
        let (file, inline) = (Arc::clone(&self.file), self.inline);
        let reading = Arc::clone(&self.file);
        let read = run::read(&self.pool, self.what("a whole read"), move || alone(&reading).read_all());
        async move {
            if inline {
                return alone(&file).read_all();
            }
            read.await
        }
    }

    /// Reads at `offset` without moving where the stream reads next; like
    /// any range it is not checked. A file on disk is read on the store's
    /// pool into a buffer of its own and copied into `bytes` once it is
    /// done, so that a future dropped mid-read leaves no thread writing into
    /// memory the program has taken back.
    pub fn read_at<'a>(&self, bytes: &'a mut [u8], offset: u64) -> impl Future<Output = io::Result<usize>> + Send + 'a {
        let (file, inline) = (Arc::clone(&self.file), self.inline);
        let wanted =
            usize::try_from(self.info.size.saturating_sub(offset)).map_or(bytes.len(), |left| left.min(bytes.len()));
        let reading = Arc::clone(&self.file);
        let read = run::read(&self.pool, self.what("a read of a range"), move || {
            let mut range = vec![0; wanted];
            Ok(shared(&reading).read_at(&mut range, offset).map(|read| {
                range.truncate(read);
                range
            }))
        });
        async move {
            if inline {
                return shared(&file).read_at(bytes, offset);
            }
            let range = read.await.map_err(io::Error::other)??;
            bytes[..range.len()].copy_from_slice(&range);
            Ok(range.len())
        }
    }

    /// Moves where the stream reads next, as [`std::io::Seek`] does: a plain
    /// call, which lets go of a piece read for the place it left.
    pub fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.position = position_after(self.position, self.info.size, to)?;
        self.piece = None;
        self.ended = false;
        Ok(self.position)
    }

    /// The stream's piece from `at`: read in the poll when the file is
    /// inline, on the pool otherwise.
    fn read_piece(&self, at: u64) -> Piece {
        let reading = Arc::clone(&self.file);
        let read = move || alone(&reading).read_piece(at, PIECE);
        if self.inline {
            return Box::pin(std::future::ready(read()));
        }
        Box::pin(run::read(&self.pool, self.what("a piece of its stream"), read))
    }

    fn what(&self, verb: &str) -> String {
        format!("{}: {verb}", self.label)
    }
}

impl Stream for StoredFile {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Result<Vec<u8>>>> {
        if self.ended || self.position >= self.info.size {
            return Poll::Ready(None);
        }
        if self.piece.is_none() {
            let piece = self.read_piece(self.position);
            self.piece = Some(piece);
        }
        let Some(piece) = self.piece.as_mut() else { return Poll::Ready(None) };
        let read = ready!(piece.as_mut().poll(context));
        self.piece = None;
        match &read {
            Ok(bytes) => self.position += bytes.len() as u64,
            Err(_) => self.ended = true,
        }
        Poll::Ready(Some(read))
    }
}

/// The reader for a read that moves it, a piece in order or a whole read:
/// one at a time, on the pool or, for an inline file, in the poll, where no
/// read of the pool ever holds it.
fn alone(file: &RwLock<crate::blobs::StoredFile>) -> RwLockWriteGuard<'_, crate::blobs::StoredFile> {
    file.write().unwrap_or_else(PoisonError::into_inner)
}

/// The reader for a range, which several read at once.
fn shared(file: &RwLock<crate::blobs::StoredFile>) -> RwLockReadGuard<'_, crate::blobs::StoredFile> {
    file.read().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
