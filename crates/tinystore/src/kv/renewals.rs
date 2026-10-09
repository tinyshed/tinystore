//! The renewals reads of idle keys ask for, written together once a second: a
//! read does not wait for a commit, so a flood of reads costs a write a second.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::cells;
use crate::Result;
use crate::sqlite::File;

/// Renewals that may wait at once; past them a key's next read asks again.
const MOST_WAITING: usize = 100_000;

/// Renewals one commit writes.
const BATCH: usize = 10_000;

#[derive(Debug, Default)]
pub(crate) struct Renewals {
    waiting: Mutex<HashMap<(i64, Vec<u8>), Renewal>>,
}

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
    /// asked again since.
    pub(crate) fn flush(&self, file: &File) -> Result<usize> {
        let asked: Vec<Asked> = self.lock().drain().collect();
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
