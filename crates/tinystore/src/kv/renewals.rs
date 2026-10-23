//! The renewals reads of idle keys ask for, written together once a second: a
//! read does not wait for a commit, so a flood of reads costs a write a second.
//! A copy's reads ask its home for theirs, passed on together once a second.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::cells;
#[cfg(feature = "replication")]
use super::forward;
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

/// How long a read on a copy in its key's last minute waits for the home to
/// answer its renewal; past it the read returns, and the renewal goes on.
#[cfg(feature = "replication")]
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Default)]
pub(crate) struct Renewals {
    waiting: Mutex<HashMap<(i64, Vec<u8>), Renewal>>,
    /// A copy's renewals, which the answers of those passed on put back when
    /// they found no way home.
    passing: Arc<Passing>,
}

/// A key by its bucket's id and its path.
type Key = (i64, Vec<u8>);

/// A copy's renewals by their key, each with the request its home runs.
type Passing = Mutex<HashMap<Key, (Renewal, Vec<u8>)>>;

/// Renewals taken out of `Passing` to pass home.
#[cfg(feature = "replication")]
type Passed = Vec<(Key, (Renewal, Vec<u8>))>;

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

    /// Passes to its home the renewals a copy kept, `PASSED` at most, and says
    /// how many. It runs on the store's scheduler beside every engine's
    /// periodic work, so it waits for nothing: the renewals leave over the
    /// link up now, and the home's answer comes back on the link's thread.
    ///
    /// Renewals that found no way home, with no link up or one that broke
    /// before the answer, are kept for the next pass, unless a read asked
    /// again since. A home that runs one twice renews nothing the second time,
    /// bound to what its read saw. The home answers for all of them, renewing
    /// what it can, so none holds back the others; a home that refuses them
    /// whole, as one of a release that knows no renewal does, has them let go,
    /// and each key's next read asks again.
    #[cfg(feature = "replication")]
    pub(crate) fn pass_home(&self, store: &crate::Store) -> Result<usize> {
        let passed = self.take_passing();
        if passed.is_empty() {
            return Ok(0);
        }
        let count = passed.len();
        let requests: Vec<Vec<u8>> = passed.iter().map(|(_, (_, request))| request.clone()).collect();
        let (passing, kept) = (Arc::clone(&self.passing), passed.clone());
        let sent = forward::pass_on_all_then(store, &requests, move |answer| {
            if found_no_way_home(answer.as_ref()) {
                keep_again(&passing, kept);
            }
        });
        match sent {
            Ok(()) => Ok(count),
            Err(error) => {
                if forward::no_way_home(&error) {
                    keep_again(&self.passing, passed);
                }
                Err(error)
            }
        }
    }

    /// Passes to its home at once the renewal of a read in its key's last
    /// minute, and says whether the home renewed it; none when no answer came
    /// within `ANSWER_WAIT`, the renewal on its way. One that finds no way home
    /// is kept for the next pass, with no wait for a link.
    #[cfg(feature = "replication")]
    pub(crate) fn pass_now(&self, store: &crate::Store, key: Key, renewal: Renewal, request: Vec<u8>) -> Option<bool> {
        let (told, answer) = std::sync::mpsc::channel();
        let (passing, kept) = (Arc::clone(&self.passing), vec![(key.clone(), (renewal, request.clone()))]);
        let sent = forward::pass_on_then(store, request.clone(), move |answer| {
            if found_no_way_home(answer.as_ref()) {
                keep_again(&passing, kept);
            }
            let _ = told.send(answer.and_then(Result::ok).and_then(|bytes| forward::found(&bytes).ok()));
        });
        if let Err(error) = sent {
            if forward::no_way_home(&error) {
                keep_again(&self.passing, vec![(key, (renewal, request))]);
            }
            return None;
        }
        answer.recv_timeout(ANSWER_WAIT).ok().flatten()
    }

    #[cfg(feature = "replication")]
    fn take_passing(&self) -> Passed {
        let mut passing = lock(&self.passing);
        let keys: Vec<Key> = passing.keys().take(PASSED).cloned().collect();
        keys.into_iter().filter_map(|key| passing.remove_entry(&key)).collect()
    }

    /// How many renewals wait for the next flush, and for the next pass home.
    #[cfg(all(test, feature = "replication", feature = "sql"))]
    pub(crate) fn kept(&self) -> (usize, usize) {
        (self.lock().len(), lock(&self.passing).len())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(i64, Vec<u8>), Renewal>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a renewal passed on found no way home, which a later pass may:
/// the link broke before the answer, or a node on the way reached no home.
#[cfg(feature = "replication")]
fn found_no_way_home<T>(answer: Option<&Result<T>>) -> bool {
    match answer {
        None => true,
        Some(Err(error)) => forward::no_way_home(error),
        Some(Ok(_)) => false,
    }
}

/// Keeps renewals passed on for the next pass again, unless a read asked
/// again since.
#[cfg(feature = "replication")]
fn keep_again(passing: &Passing, passed: Passed) {
    let mut passing = lock(passing);
    for (key, kept) in passed {
        if passing.len() >= MOST_WAITING {
            return;
        }
        passing.entry(key).or_insert(kept);
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
