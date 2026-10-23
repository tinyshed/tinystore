//! A file's checkpoints, off its writer's turn.
//!
//! SQLite checkpoints a WAL inside the commit that leaves it 1000 frames
//! long, the writer held: it copies the frames' pages into the file and, at
//! either durability, syncs the WAL and then the file. Every write queued
//! waits for it. Under 64 callers setting keys those commits took 10 to 100
//! ms on a busy disk, and the store's sets a second were a fifth of what the
//! same writes made with no checkpoint at all (issue 7).
//!
//! So the writer keeps none of its own until the WAL is [`HARD`] frames long.
//! A commit that leaves it [`SOFT`] frames long asks for one, and the next
//! thread through the file that holds no turn of the writer runs it, PASSIVE,
//! on a connection of its own, beside the writes going on: a caller whose
//! write was answered, or one that queued a write behind a commit under way.
//! No thread is started for it, and a store without background work
//! checkpoints the same way. One runs at a time, as often as callers come
//! while the WAL is long, which SQLite's own did after every commit too.
//!
//! ```text
//! the writer: commit ── commit, the WAL past SOFT: due ── commit ── commit ── …
//! a caller:                         └── answered ── checkpoint ── returns
//! ```
//!
//! A WAL starts over at the first write after a checkpoint copied all of it.
//! Writes that never pause long enough for one to catch up leave it growing
//! until [`HARD`], where the writer checkpoints what the last one left, as
//! SQLite did, and the next write starts it over. A file whose commits a
//! capture hands to a node's log, and a copy, whose applier copies frames
//! only once their position is synced, run their own checkpoints, never these.
//!
//! Reads that never stop would keep the WAL growing past [`HARD`] for good:
//! a read begun before the last commit holds a snapshot of the frames before
//! it, and a passive checkpoint copies no further, so it never copies all of
//! the WAL. So at [`HARD`] the writer's checkpoint is a restart instead,
//! which waits [`RESTART_WAIT`] at most for those reads to end, so that the
//! next write starts the WAL over; a read begun after its copy reads the file
//! alone and keeps nothing waiting. A read that outlasts the wait is waited
//! for again only twice as far past the bound.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::Duration;

use rusqlite::Connection;

use super::Config;
use super::capture::{CHECKPOINT_AT, hook_wal, take_wal_end};
use super::connection::{self, Role};
use super::given::own;

/// Frames at which a commit asks for a checkpoint off the writer's turn:
/// SQLite's own automatic checkpoint.
pub(super) const SOFT: u32 = CHECKPOINT_AT;
/// Frames at which the writer checkpoints the WAL itself: 128 MiB of 4 KiB
/// pages, which only writes that never pause reach. Each time they do, the
/// writer waits for two syncs: under 64 callers on a busy disk, a bound of
/// 8,000 frames held sets to three quarters of what 32,000 let through, and
/// their 99th percentile to four times as long (issue 7).
pub(super) const HARD: u32 = 32 * SOFT;

/// How long a restart of the WAL at [`HARD`] waits for the reads begun
/// before it copied the WAL's last frame, the writer held meanwhile.
const RESTART_WAIT: Duration = Duration::from_millis(100);

const CHECKPOINT: &str = own!("pragma wal_checkpoint(passive)");
const RESTART: &str = own!("pragma wal_checkpoint(restart)");

/// Where a file's checkpoints run, and whether one is due.
pub(crate) struct Checkpoints {
    path: PathBuf,
    config: Arc<Config>,
    /// Off for a copy and a file a capture logs, which checkpoint their WAL
    /// themselves, and once the file closes.
    on: AtomicBool,
    due: AtomicBool,
    /// The frames past [`HARD`] at which the writer next restarts the WAL,
    /// once a restart waited in vain for a read; 0 when none did.
    restart_from: AtomicU32,
    /// The connection checkpoints off the writer's turn run on, opened at the
    /// first: a reader of the pool's kind that serves no read.
    connection: Mutex<Option<Connection>>,
}

impl Checkpoints {
    /// The checkpoints of the file at `path`, whose `writer` keeps none of
    /// SQLite's own from here on.
    pub(crate) fn of(path: &Path, config: &Arc<Config>, writer: &Connection) -> Checkpoints {
        hook_wal(writer);
        Checkpoints::new(path, config, true)
    }

    /// Checkpoints that never run: a copy's, and those of a file whose
    /// capture checkpoints it.
    pub(crate) fn none(path: &Path, config: &Arc<Config>) -> Checkpoints {
        Checkpoints::new(path, config, false)
    }

    fn new(path: &Path, config: &Arc<Config>, on: bool) -> Checkpoints {
        Checkpoints {
            path: path.to_owned(),
            config: Arc::clone(config),
            on: AtomicBool::new(on),
            due: AtomicBool::new(false),
            restart_from: AtomicU32::new(0),
            connection: Mutex::new(None),
        }
    }

    /// Forgets where an earlier commit on this thread left a WAL, so that
    /// what [`Checkpoints::committed`] reads is the next commit's.
    pub(crate) fn arm(&self) {
        take_wal_end();
    }

    /// Reads where the commit just made on `writer`, still held, left the WAL:
    /// at [`HARD`] frames the writer checkpoints it now, and at [`SOFT`] one
    /// is due. A checkpoint that fails leaves the WAL as long, and the next
    /// commit asks again.
    pub(crate) fn committed(&self, writer: &Connection) {
        let Some(end) = take_wal_end() else { return };
        if !self.on.load(Ordering::SeqCst) {
            return;
        }
        if end >= HARD {
            return self.at_the_bound(writer, end);
        }
        // the WAL started over since a restart last waited in vain
        self.restart_from.store(0, Ordering::Relaxed);
        if end >= SOFT {
            self.due.store(true, Ordering::SeqCst);
        }
    }

    /// Restarts the WAL `end` frames long on the writer, or, while a read
    /// that outlasted the last restart's wait may still run, copies what the
    /// reads leave it to copy.
    fn at_the_bound(&self, writer: &Connection, end: u32) {
        if end < self.restart_from.load(Ordering::Relaxed) {
            return passive(writer);
        }
        let started_over = matches!(restart(writer, self.config.busy_timeout), Ok((0, _, _)));
        let next = if started_over { 0 } else { restart_again_at(end) };
        self.restart_from.store(next, Ordering::Relaxed);
    }

    /// Runs the checkpoint due, if one is and none runs: called on a thread
    /// that holds no turn of the writer, which goes on meanwhile.
    pub(crate) fn run_due(&self) {
        if !self.due.load(Ordering::Relaxed) {
            return;
        }
        let mut connection = match self.connection.try_lock() {
            Ok(connection) => connection,
            Err(TryLockError::WouldBlock) => return,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };
        if !self.due.swap(false, Ordering::SeqCst) || !self.on.load(Ordering::SeqCst) {
            return;
        }
        if connection.is_none() {
            *connection = connection::open(&self.path, &self.config, Role::Reader).ok();
        }
        if let Some(connection) = connection.as_ref() {
            passive(connection);
        }
    }

    /// Runs no checkpoint from here on, and closes the connection they ran
    /// on once the one running ends.
    pub(crate) fn stop(&self) {
        self.on.store(false, Ordering::SeqCst);
        self.due.store(false, Ordering::SeqCst);
        self.connection.lock().unwrap_or_else(PoisonError::into_inner).take();
    }
}

/// Copies what of the WAL no reader still needs into the file, waiting for
/// nobody: another connection's checkpoint under way leaves this one nothing
/// to do, and one that fails, a WAL as long as it was.
fn passive(connection: &Connection) {
    let _ = connection.query_row(CHECKPOINT, [], |_| Ok(()));
}

/// Copies all of the WAL into the file on `writer`, held, whose busy timeout
/// is `busy_timeout`, and waits [`RESTART_WAIT`] at most for the reads begun
/// before it copied the last frame, so that the next write starts the WAL
/// over. Its row is SQLite's: 1 when a read kept it from that, then the
/// WAL's frames and those copied.
pub(super) fn restart(writer: &Connection, busy_timeout: Duration) -> rusqlite::Result<(i64, i64, i64)> {
    writer.busy_timeout(RESTART_WAIT)?;
    let restarted = writer.query_row(RESTART, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
    writer.busy_timeout(busy_timeout)?;
    restarted
}

/// The frames at which a restart that waited in vain at `end` is tried
/// again: twice as far past [`HARD`] each time, so that a long read costs the
/// writer a few waits rather than one a commit.
pub(super) fn restart_again_at(end: u32) -> u32 {
    end.saturating_add(end.saturating_sub(HARD).max(SOFT))
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
