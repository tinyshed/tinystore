//! A backup over the protocol: the zip a store's backup writes, sent as pieces
//! of DATA within the client's credit, then what it holds. The backup writes
//! on a worker into a few pieces held here, and waits while they are all
//! held; the client's credit sends them, and a client that cancels, leaves,
//! or gives no credit for a minute ends the backup, whose zip it never had
//! whole.
//!
//! ```text
//! RESPONSE · DATA backup.Piece … · END backup.Written {files, bytes, keyLeftOut}
//! ```

use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::Link;
use super::codec::Message;
use super::protocol::{BackupPiece, BackupWrite, BackupWritten, Failure};
use crate::Store;
use crate::sync::lock;

/// The pieces a backup holds before it waits for the client: what keeps its
/// worker writing while the client reads those before.
const HELD: usize = 4;
/// The bytes of a piece at most, within what one message carries.
const PIECE: usize = 256 << 10;
/// What a piece's message holds beside its bytes.
const PIECE_FIELDS: usize = 64;
/// How long a backup waits for its client's credit, from the last it was
/// given or from when the wait began, whichever is later, before it stops:
/// a slow client that grants steadily is never stopped. A minute is long
/// enough for a client that writes the zip to a slow disk, and short enough
/// that one that stopped reading does not keep a copy of the store on disk,
/// and a worker of the pool, for long. A test waits a second.
const PATIENCE: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(60) };

/// The backups a connection's streams are sending, by stream.
#[derive(Default)]
pub(crate) struct Backups {
    going: Arc<Mutex<HashMap<u32, Arc<Sending>>>>,
}

impl Backups {
    pub(crate) fn grant(&self, stream: u32, credit: u32) {
        let sending = lock(&self.going).get(&stream).cloned();
        if let Some(sending) = sending {
            sending.grant(credit);
            forget(&self.going, &sending);
        }
    }

    /// Ends a backup its client cancelled; says whether there was one.
    pub(crate) fn cancel(&self, stream: u32) -> bool {
        let sending = lock(&self.going).remove(&stream);
        sending.is_some_and(|sending| sending.end(Err(Failure::cancelled("the client cancelled its backup"))))
    }

    /// Ends every backup, the connection gone.
    pub(crate) fn end_all(&self, why: &str) {
        let going: Vec<Arc<Sending>> = lock(&self.going).drain().map(|(_, sending)| sending).collect();
        for sending in going {
            sending.end(Err(Failure::unavailable(why)));
        }
    }
}

/// A backup `body` asks for, to send on `stream` once its RESPONSE has gone:
/// [`Sending::begin`] starts it.
pub(crate) fn asked(backups: &Backups, stream: u32, body: &[u8], link: Link) -> Result<Arc<Sending>, Failure> {
    let asked = BackupWrite::decode(body)?;
    let piece = PIECE.min(link.max_body.saturating_sub(PIECE_FIELDS)).max(1);
    let state = Mutex::new(SendState { credit: link.credit, ..SendState::default() });
    let sending = Arc::new(Sending { stream, asked, piece, state, room: Condvar::new(), link });
    lock(&backups.going).insert(stream, Arc::clone(&sending));
    Ok(sending)
}

/// A backup's zip going out: the pieces its worker wrote and the client has
/// no credit for yet, and how it ends.
pub(crate) struct Sending {
    stream: u32,
    asked: BackupWrite,
    piece: usize,
    state: Mutex<SendState>,
    /// What wakes the worker once a held piece has gone or the stream ended.
    room: Condvar,
    link: Link,
}

#[derive(Default)]
struct SendState {
    credit: u64,
    /// When the client last gave credit, none before its first grant.
    granted: Option<Instant>,
    held: VecDeque<Vec<u8>>,
    /// The last frame, once the backup is written, sent after its pieces.
    last: Option<Result<Vec<u8>, Failure>>,
    ended: bool,
}

impl Sending {
    /// Writes the backup on a worker, its pieces sent as the credit takes them.
    pub(crate) fn begin(self: &Arc<Self>, store: &Store, backups: &Backups) {
        let (store, sending, going) = (store.clone(), Arc::clone(self), Arc::clone(&backups.going));
        (self.link.spawn)(Box::new(move || {
            let mut backup = store.backup();
            for name in &sending.asked.include {
                backup = backup.include(name);
            }
            if sending.asked.include_encryption_key {
                backup = backup.include_encryption_key();
            }
            let written = backup.write_to(Pieces { sending: &sending, piece: Vec::new() });
            let last = written.map(|backup| {
                BackupWritten { files: backup.files as u64, bytes: backup.bytes, key_left_out: backup.key_left_out }
                    .encode()
            });
            sending.finish(last.map_err(Failure::from));
            forget(&going, &sending);
        }));
    }

    fn grant(&self, credit: u32) {
        let mut state = lock(&self.state);
        state.credit += u64::from(credit);
        state.granted = Some(Instant::now());
        self.flush(&mut state);
    }

    /// Holds a piece of the zip, waiting while `HELD` are; an error once the
    /// stream has ended, which ends the backup. A wait that saw no credit for
    /// [`PATIENCE`] ends the stream `cancelled`, as a read whose timeout
    /// passed ends, so that the snapshot goes and the worker is free.
    fn push(&self, body: Vec<u8>) -> io::Result<()> {
        let mut state = lock(&self.state);
        let began = Instant::now();
        while state.held.len() >= HELD && !state.ended {
            let since = state.granted.map_or(began, |granted| granted.max(began));
            let left = (since + PATIENCE).saturating_duration_since(Instant::now());
            if left.is_zero() {
                let why = format!("the backup's reader took nothing for {PATIENCE:?}, and the backup stopped");
                self.end_in(&mut state, Err(Failure::cancelled(why)));
                break;
            }
            state = self.room.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        if state.ended {
            return Err(io::Error::other("the stream of the backup ended"));
        }
        state.held.push_back(body);
        self.flush(&mut state);
        Ok(())
    }

    /// Sends what the credit takes, then the last frame once nothing is held.
    fn flush(&self, state: &mut SendState) {
        while state.held.front().is_some_and(|body| body.len() as u64 <= state.credit) {
            let body = state.held.pop_front().unwrap_or_default();
            state.credit -= body.len() as u64;
            (self.link.send)(body);
            self.room.notify_all();
        }
        if state.held.is_empty()
            && !state.ended
            && let Some(last) = state.last.take()
        {
            state.ended = true;
            (self.link.finish)(last);
        }
    }

    /// The backup written, or failed: a failure ends the stream at once, and
    /// the end of one written goes after its pieces.
    fn finish(&self, last: Result<Vec<u8>, Failure>) {
        let mut state = lock(&self.state);
        if last.is_err() {
            state.held.clear();
        }
        state.last = Some(last);
        self.flush(&mut state);
    }

    /// Ends the stream with `last` unless it ended, waking a worker that
    /// waits for room; says whether it was still going.
    fn end(&self, last: Result<Vec<u8>, Failure>) -> bool {
        self.end_in(&mut lock(&self.state), last)
    }

    /// [`Sending::end`], its state held already.
    fn end_in(&self, state: &mut SendState, last: Result<Vec<u8>, Failure>) -> bool {
        if std::mem::replace(&mut state.ended, true) {
            return false;
        }
        state.held.clear();
        self.room.notify_all();
        (self.link.finish)(last);
        true
    }
}

/// The zip a backup writes, cut into pieces of DATA.
struct Pieces<'a> {
    sending: &'a Sending,
    piece: Vec<u8>,
}

impl Write for Pieces<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let taken = bytes.len().min(self.sending.piece - self.piece.len());
        self.piece.extend_from_slice(&bytes[..taken]);
        if self.piece.len() == self.sending.piece {
            self.flush()?;
        }
        Ok(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.piece.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.piece);
        self.sending.push(BackupPiece { bytes }.encode())
    }
}

/// Lets a backup's stream go once it has ended: its pieces held after the
/// backup was written still go as credit comes, so it stays until its last
/// frame.
fn forget(going: &Mutex<HashMap<u32, Arc<Sending>>>, sending: &Arc<Sending>) {
    if !lock(&sending.state).ended {
        return;
    }
    let mut going = lock(going);
    if going.get(&sending.stream).is_some_and(|held| Arc::ptr_eq(held, sending)) {
        going.remove(&sending.stream);
    }
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
