//! The renewals reads of idle keys ask for, written together once a second: a
//! read does not wait for a commit, so a flood of reads costs a write a second.
//! A copy's reads ask its home for theirs, passed on together once a second.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::cells;
use crate::Result;
use crate::sqlite::File;
use crate::sync::lock;

/// Renewals that may wait at once; past them a key's next read asks again.
const MOST_WAITING: usize = 100_000;

/// Renewals one commit writes.
const BATCH: usize = 10_000;

/// Renewals a copy passes to its home at once.
#[cfg(feature = "replication")]
const PASSED: usize = 1024;

#[derive(Debug, Default)]
pub(crate) struct Renewals {
    waiting: Mutex<HashMap<(i64, Vec<u8>), Renewal>>,
    /// A copy's renewals, each with the request its home runs.
    passing: Mutex<HashMap<Key, (Renewal, Vec<u8>)>>,
}

/// A key by its bucket's id and its path.
type Key = (i64, Vec<u8>);

/// A renewal is bound to the cell its read saw, so that it never extends a key
/// written again, renewed or deleted since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Renewal {
    pub(crate) version: i64,
    pub(crate) seen: i64,
    pub(crate) until: i64,
}

type Asked = ((i64, Vec<u8>), Renewal);

impl Renewals {
    /// Keeps a renewal for the next flush, the latest one a key.
    pub(crate) fn ask(&self, bucket: i64, path: Vec<u8>, renewal: Renewal) {
        let mut waiting = self.lock();
        let key = (bucket, path);
        if waiting.len() >= MOST_WAITING && !waiting.contains_key(&key) {
            return;
        }
        waiting.insert(key, renewal);
    }

    /// Writes the renewals asked for and says how many keys they renewed. A
    /// failed commit keeps its renewals for the next flush, unless a read has
    /// asked again since. Those a copy kept for its home are written too, once
    /// it is the home.
    pub(crate) fn flush(&self, file: &File) -> Result<usize> {
        let mut asked: Vec<Asked> = self.lock().drain().collect();
        if !file.is_copy() {
            asked.extend(lock(&self.passing).drain().map(|(key, (renewal, _))| (key, renewal)));
        }
        let mut renewed = 0;
        for (index, batch) in asked.chunks(BATCH).enumerate() {
            let batch = batch.to_vec();
            match file.write(0, move |tx| write(tx, &batch)) {
                Ok(written) => renewed += written,
                Err(error) => {
                    self.give_back(&asked[index * BATCH..]);
                    return Err(error);
                }
            }
        }
        Ok(renewed)
    }

    fn give_back(&self, asked: &[Asked]) {
        let mut waiting = self.lock();
        for (key, renewal) in asked {
            waiting.entry(key.clone()).or_insert(*renewal);
        }
    }

    /// Keeps a copy's renewal for the next pass to its home, the latest one
    /// a key, with `request`, the call its home runs.
    #[cfg(feature = "replication")]
    pub(crate) fn pass(&self, bucket: i64, path: Vec<u8>, renewal: Renewal, request: Vec<u8>) {
        let mut passing = lock(&self.passing);
        let key = (bucket, path);
        if passing.len() >= MOST_WAITING && !passing.contains_key(&key) {
            return;
        }
        passing.insert(key, (renewal, request));
    }

    /// Passes a copy's renewals to its home, which renews them as its own
    /// reads do, and says how many it passed; the first failure is the error.
    ///
    /// A home out of reach keeps them for the next pass, unless a read has
    /// asked again since; one its home ran already renews nothing a second
    /// time, bound to what it saw. Those the home refused are let go, since
    /// one it always refuses would hold every other back, and the key's next
    /// read asks again.
    #[cfg(feature = "replication")]
    pub(crate) fn pass_home(&self, store: &crate::Store) -> Result<usize> {
        let asked: Vec<_> = lock(&self.passing).drain().collect();
        let (mut passed, mut first_failure) = (0, None);
        for (index, batch) in asked.chunks(PASSED).enumerate() {
            let requests: Vec<Vec<u8>> = batch.iter().map(|(_, (_, request))| request.clone()).collect();
            match super::forward::pass_on_all(store, &requests) {
                Ok(()) => passed += batch.len(),
                Err(error) if error.kind() == crate::ErrorKind::Unavailable => {
                    let mut passing = lock(&self.passing);
                    for (key, kept) in &asked[index * PASSED..] {
                        passing.entry(key.clone()).or_insert_with(|| kept.clone());
                    }
                    return Err(error);
                }
                Err(error) => {
                    first_failure.get_or_insert(error);
                }
            }
        }
        first_failure.map_or(Ok(passed), Err)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(i64, Vec<u8>), Renewal>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn write(tx: &crate::sqlite::Tx<'_>, batch: &[Asked]) -> Result<usize> {
    let mut renewed = 0;
    for ((bucket, path), renewal) in batch {
        if cells::renew(tx, *bucket, path, (renewal.version, renewal.seen), renewal.until)? {
            renewed += 1;
        }
    }
    Ok(renewed)
}
