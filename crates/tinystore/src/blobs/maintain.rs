//! What the store runs every minute: expired files and the files marked
//! clears hid, removed a batch at a time; the files of contents no row names
//! any more; what uploads left; the settled mark; and a slice of the scrub.

use std::sync::Arc;

use rusqlite::params;

use super::change::Change;
use super::engine::Blobs;
use super::list::PAST_EVERY;
use super::paths::past;
use crate::sqlite::sql_error;
use crate::{Result, Store};

/// Rows one maintenance transaction removes.
const BATCH: i64 = 10_000;

/// Transactions of each kind one maintenance runs at most.
const BATCHES: usize = 10;

const EXPIRE: &str = "delete from files where (set_id, path) in (
    select set_id, path from files where expires <= ?1 order by expires limit cast(?2 as integer)
) returning content";
const SELECT_CLEARED: &str = "select set_id, prefix, revision from cleared";
const DROP_HIDDEN: &str = "delete from files where (set_id, path) in (
    select set_id, path from files where set_id = ?1 and path >= ?2 and path < ?3 and revision <= ?4
    limit cast(?5 as integer)
) returning content";
const UNMARK: &str = "delete from cleared where set_id = ?1 and prefix = ?2 and revision = ?3";
const SELECT_UNNAMED: &str = "select id from contents where names = 0 order by id limit cast(?1 as integer)";
const FORGET: &str = "delete from contents where id = ?1 and names = 0";

/// What one maintenance did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// Files past their term, removed.
    pub expired: usize,
    /// Files a marked clear hid, removed.
    pub cleared: usize,
    /// Files on disk of contents no file names, removed.
    pub removed: usize,
    /// Bytes the scrub read.
    pub scrubbed: u64,
    /// Contents the scrub found changed or missing.
    pub damaged: usize,
}

/// Removes expired files, the files large clears hid, and bytes no file names
/// any more, and scrubs a slice of the stored bytes. The store runs it every
/// minute; a store opened without background work leaves it to the caller.
pub fn maintain(store: &Store) -> Result<Maintenance> {
    Blobs::of(store)?.maintain()
}

impl Blobs {
    /// Every part runs, whatever another's failure; the first failure is the
    /// answer.
    pub(crate) fn maintain(self: &Arc<Self>) -> Result<Maintenance> {
        let mut done = Maintenance::default();
        let mut first = None;
        let mut keep = |failed: Result<()>| {
            if let Err(error) = failed {
                first.get_or_insert(error);
            }
        };
        keep(self.expire().map(|expired| done.expired = expired));
        keep(self.drop_cleared().map(|cleared| done.cleared = cleared));
        keep(self.remove_unnamed().map(|removed| done.removed = removed));
        keep(self.disk.sweep_uploads(&self.ids).map(drop));
        for id in self.ids.doubtful() {
            self.resolve(id);
        }
        keep(self.settle());
        keep(self.scrub().map(|(scrubbed, damaged)| (done.scrubbed, done.damaged) = (scrubbed, damaged)));
        first.map_or(Ok(done), Err)
    }

    fn expire(self: &Arc<Self>) -> Result<usize> {
        let now = self.now();
        self.batches(move |change| change.deleted(EXPIRE, params![now, BATCH]))
    }

    /// Deletes the files marked clears hid, a batch a transaction, and each
    /// mark with the last of its files.
    fn drop_cleared(self: &Arc<Self>) -> Result<usize> {
        let marks: Vec<(i64, String, i64)> = self.file.read(|connection| {
            let mut statement = connection
                .prepare_cached(SELECT_CLEARED)
                .map_err(|error| sql_error("blobs: the marked clears", error))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|error| sql_error("blobs: the marked clears", error))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|error| sql_error("blobs: the marked clears", error))
        })?;
        let mut total = 0;
        for (set, prefix, revision) in marks {
            let to = past(&prefix).unwrap_or_else(|| PAST_EVERY.clone());
            total += self.batches(move |change| {
                let contents = change.deleted(DROP_HIDDEN, params![set, prefix, to, revision, BATCH])?;
                if contents.len() < usize::try_from(BATCH).unwrap_or(usize::MAX) {
                    change.execute(UNMARK, params![set, prefix, revision])?;
                }
                Ok(contents)
            })?;
        }
        Ok(total)
    }

    /// Runs `batch` in a transaction of its own until one deletes fewer than
    /// a full batch, ten at most. Each takes a name from the contents of the
    /// files it deleted, and removes the files it leaves without names once
    /// it has committed.
    fn batches<F>(self: &Arc<Self>, batch: F) -> Result<usize>
    where
        F: Fn(&mut Change<'_, '_>) -> Result<Vec<i64>> + Clone + Send + 'static,
    {
        let mut total = 0;
        for _ in 0..BATCHES {
            let (blobs, batch) = (Arc::clone(self), batch.clone());
            let (deleted, freed) = self.file.write(0, move |tx| {
                let mut change = Change::new(tx, &blobs);
                let contents = batch(&mut change)?;
                let deleted = contents.len();
                change.release_all(contents)?;
                Ok((deleted, change.freed))
            })?;
            total += deleted;
            self.let_files_go(&freed);
            if deleted < usize::try_from(BATCH).unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(total)
    }

    /// Removes the files of contents no file names, which a commit left
    /// because its own removal failed, and then their rows. A file that will
    /// not go stays listed, and is tried again next time.
    fn remove_unnamed(&self) -> Result<usize> {
        let mut removed = 0;
        for _ in 0..BATCHES {
            let unnamed: Vec<i64> = self.file.read(|connection| {
                let mut statement = connection
                    .prepare_cached(SELECT_UNNAMED)
                    .map_err(|error| sql_error("blobs: the contents no file names", error))?;
                let rows = statement
                    .query_map([BATCH], |row| row.get(0))
                    .map_err(|error| sql_error("blobs: the contents no file names", error))?;
                rows.collect::<rusqlite::Result<Vec<i64>>>()
                    .map_err(|error| sql_error("blobs: the contents no file names", error))
            })?;
            let gone: Vec<i64> = unnamed.iter().copied().filter(|&id| self.disk.remove_object(id).is_ok()).collect();
            if !gone.is_empty() {
                let forgotten = gone.clone();
                self.file.write(0, move |tx| {
                    for id in &forgotten {
                        tx.execute(FORGET, [id]).map_err(|error| sql_error("blobs: forget a content", error))?;
                    }
                    Ok(())
                })?;
            }
            removed += gone.len();
            if unnamed.len() < usize::try_from(BATCH).unwrap_or(usize::MAX) || gone.len() < unnamed.len() {
                break;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
#[path = "maintain_tests.rs"]
mod tests;
