//! The uses reads record of a set's files: a file's term from its last use on
//! a set with `idle`, and its rank, the order a ceiling evicts in. A read
//! waits for no commit: its use is kept here and written with the others
//! once a second, a batch a commit, so that a flood of reads costs a write a
//! second. An idle file with less than a minute left is renewed before its
//! read answers.
//!
//! A use is written at most once an hour, or once a thirtieth of `idle` on a
//! set with one, bound to the row its read saw, so that it never moves a
//! file written again, renewed or removed since. Its rank:
//!
//! | The file's last use        | Its rank              |
//! |----------------------------|-----------------------|
//! | its write, no use recorded | its modified time     |
//! | a read after its write     | that read and a day   |
//!
//! So a file written and never read again goes before every file read in the
//! day since, and a file read goes by its last read; a day, not for good, so
//! that a file read a year ago still goes before one written this morning.
//! research's files-cache-eviction-2026-11-17 measured a day's head start
//! from the first read against plain LRU and against one from the second.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::params;

use super::files::Files;
use crate::Result;
use crate::sqlite::{File, Tx, sql_error};
use crate::sync::lock;

/// The head start of a file read since its write: its rank is its last read and this.
/// Plain LRU, the last use alone, is a head start of 0.
const HEAD_START: i64 = 24 * HOUR;

/// The least time between two uses written of a file of a set without `idle`.
const HOUR: i64 = 60 * 60 * 1000;

/// An idle file is renewed once a thirtieth of its term has passed since it
/// last was, and before its read returns when it has less than a minute left.
const REFRESHES: i64 = 30;
const RENEW_AT_ONCE: i64 = 60_000;

/// How often the uses reads asked for are written.
pub(crate) const USES: Duration = Duration::from_secs(1);

/// Uses that may wait at once; past them a file's next read asks again.
const MOST_WAITING: usize = 100_000;

/// Uses one commit writes.
const BATCH: usize = 10_000;

const RECORD_USE: &str = "update _tinystore_blobs_files
    set rank = ?4 + ?5, used = ?4, expires = coalesce(?6, expires)
    where set_id = ?1 and path = ?2 and revision = ?3 and (expires is null or expires > ?7)";

/// Whether the set of a file aliased `f` keeps a ceiling, as a read's lookup
/// asks its set's row, so that a handle opened before another process wrote
/// the ceiling records its uses from then on.
macro_rules! ceiled {
    () => {
        "(select s.max_size is not null from _tinystore_blobs_sets as s where s.id = f.set_id)"
    };
}
pub(crate) use ceiled;

/// What a read saw of a file's row, which the use it records is bound to,
/// and whether its set keeps a ceiling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Seen {
    pub(crate) revision: i64,
    pub(crate) modified: i64,
    pub(crate) used: Option<i64>,
    pub(crate) expires: Option<i64>,
    pub(crate) ceiled: bool,
}

/// A use a read asked for: of the row it saw, at its time, and on an idle set
/// the term it renews the file to.
#[derive(Clone, Debug)]
pub(crate) struct Use {
    /// The set's name, which a copy's home opens it by.
    #[cfg_attr(not(feature = "replication"), expect(dead_code, reason = "only a copy passes its uses home"))]
    pub(crate) name: Arc<str>,
    pub(crate) revision: i64,
    pub(crate) used: i64,
    pub(crate) until: Option<i64>,
}

/// The uses waiting for the next flush, the latest one a file.
#[derive(Default)]
pub(crate) struct Uses(Mutex<HashMap<(i64, String), Use>>);

impl std::fmt::Debug for Uses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Uses").field("waiting", &lock(&self.0).len()).finish()
    }
}

impl Uses {
    pub(crate) fn ask(&self, set: i64, path: &str, asked: Use) {
        let mut waiting = lock(&self.0);
        let key = (set, path.to_owned());
        if waiting.len() >= MOST_WAITING && !waiting.contains_key(&key) {
            return;
        }
        waiting.insert(key, asked);
    }

    /// Writes the uses asked for and says how many files they reached. A
    /// failed commit keeps its uses for the next flush, unless a read asked
    /// again since.
    pub(crate) fn flush(&self, file: &File, now: impl Fn() -> i64 + Clone + Send + 'static) -> Result<usize> {
        let asked: Vec<((i64, String), Use)> = lock(&self.0).drain().collect();
        let mut recorded = 0;
        for (index, batch) in asked.chunks(BATCH).enumerate() {
            let (batch, now) = (batch.to_vec(), now.clone());
            let written = file.write(0, move |tx| {
                let now = now();
                let mut recorded = 0;
                for ((set, path), asked) in &batch {
                    recorded += usize::from(write(tx, *set, path, asked, now)?);
                }
                Ok(recorded)
            });
            match written {
                Ok(written) => recorded += written,
                Err(error) => {
                    let mut waiting = lock(&self.0);
                    for (key, asked) in &asked[index * BATCH..] {
                        waiting.entry(key.clone()).or_insert_with(|| asked.clone());
                    }
                    return Err(error);
                }
            }
        }
        Ok(recorded)
    }

    /// The uses waiting, taken out: a copy's, which its home writes.
    #[cfg(feature = "replication")]
    pub(crate) fn take(&self) -> Vec<((i64, String), Use)> {
        lock(&self.0).drain().collect()
    }
}

/// Writes one use at `now`, as its commit is built; says whether its row
/// was still the one its read saw.
pub(crate) fn write(tx: &Tx<'_>, set: i64, path: &str, asked: &Use, now: i64) -> Result<bool> {
    tx.prepare_cached(RECORD_USE)
        .and_then(|mut update| {
            update.execute(params![set, path, asked.revision, asked.used, HEAD_START, asked.until, now])
        })
        .map(|changed| changed > 0)
        .map_err(|error| sql_error("blobs: a file's use", error))
}

impl Files {
    /// Records that a read used the file it found at a whole path, when the
    /// set's term or ceiling keeps uses and one is due; says the file's new
    /// term when it was renewed before the read returns.
    pub(super) fn used(&self, whole: &str, seen: &Seen) -> Result<Option<i64>> {
        let Some((asked, at_once)) = self.use_due(seen) else { return Ok(None) };
        if !at_once {
            self.blobs.uses.ask(self.set, whole, asked);
            return Ok(None);
        }
        // an awaited read in its poll waits for no commit: it runs again on the pool
        crate::polled::may_wait(|| format!("{}: {whole:?}: a renewal", self.describe()))?;
        #[cfg(feature = "replication")]
        if self.is_copy() {
            return Ok(self.use_at_home(whole, asked));
        }
        let (set, path, until, written) = (self.set, whole.to_owned(), asked.until, asked.clone());
        let blobs = Arc::clone(&self.blobs);
        match self.blobs.file.write(0, move |tx| write(tx, set, &path, &written, blobs.now())) {
            Ok(true) => Ok(until),
            Ok(false) => Ok(None),
            Err(_) => {
                self.blobs.uses.ask(self.set, whole, asked);
                Ok(None)
            }
        }
    }

    /// Passes a copy's renewal to its home before the read returns, waiting
    /// for its answer; one that does not reach it goes with the next flush.
    /// Says the file's new term when the home renewed it.
    #[cfg(feature = "replication")]
    fn use_at_home(&self, whole: &str, asked: Use) -> Option<i64> {
        let until = asked.until;
        match self.pass_on(&super::forward::Call::Use { path: whole, used: &asked }) {
            Ok(true) => until,
            Ok(false) => None,
            Err(_) => {
                self.blobs.uses.ask(self.set, whole, asked);
                None
            }
        }
    }

    /// Records at the home a use a copy's read passed on; says whether the
    /// row was still the one that read saw.
    #[cfg(feature = "replication")]
    pub(crate) fn use_passed_on(&self, whole: &str, asked: &Use) -> Result<bool> {
        let (set, path, asked, blobs) = (self.set, whole.to_owned(), asked.clone(), Arc::clone(&self.blobs));
        self.blobs.file.write(0, move |tx| write(tx, set, &path, &asked, blobs.now()))
    }

    /// The use a read of `seen` records, and whether it renews before the
    /// read answers; none when no use is due.
    fn use_due(&self, seen: &Seen) -> Option<(Use, bool)> {
        if !self.records_uses(seen) {
            return None;
        }
        let now = self.blobs.now();
        let asked = |until| Use { name: Arc::clone(&self.name), revision: seen.revision, used: now, until };
        match self.idle() {
            Some(term) => {
                let (term, expires) = (crate::clock::millis(term), seen.expires?);
                if expires - now > term - term / REFRESHES {
                    return None;
                }
                Some((asked(Some(now.saturating_add(term))), expires - now < RENEW_AT_ONCE))
            }
            None if now - seen.used.unwrap_or(seen.modified) >= HOUR => Some((asked(None), false)),
            None => None,
        }
    }
}
