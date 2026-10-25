//! The records engine: `records.db` in the store's directory, its streams and
//! what each keeps.
//!
//! ```text
//! append ──► heads: a row a stream a write, under zstd, durable
//! list   ◄── the rows of its range, fetched in one read and parsed after it
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::params;

use super::buffer::Held;
use super::head::Codec;
use super::repair::Damaged;
use super::tail::Recent;
use crate::engine::{Claim, Engine, Host};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::store::WeakStore;
use crate::{Clock, Durability, Error, Memory, Result, Store, unix_nanos};

const MIGRATIONS: &[Migration] = &[
    Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql")),
    Migration::of(2, "0002_arrivals.sql", include_str!("migrations/0002_arrivals.sql")),
    Migration::of(3, "0003_ids.sql", include_str!("migrations/0003_ids.sql")),
];

/// records' file in the store's directory.
const FILE: &str = "records.db";

/// How long a stream keeps its events until an open says.
pub(crate) const KEEP: Duration = Duration::from_hours(14 * 24);
/// How far ahead of the store's clock an event's time may be: a later one
/// would hold its row past what its stream keeps.
pub(crate) const AHEAD: Duration = Duration::from_mins(10);
/// How often maintenance runs: what streams no longer keep is removed.
const MAINTENANCE: Duration = Duration::from_mins(1);
/// How far behind its stream's newest event one may come and still be on
/// time: a later one waits in the stream's late head, so that the segments
/// of those on time stay narrow in time.
pub(super) const LATENESS: Duration = Duration::from_secs(10);

pub(crate) struct Records {
    store: WeakStore,
    /// What records tells the store's own metrics.
    pub(super) probe: Arc<crate::probe::Probe>,
    pub(crate) file: File,
    pub(crate) memory: Arc<Memory>,
    clock: Arc<dyn Clock>,
    streams: Mutex<HashMap<String, Stream>>,
    /// The zstd context every append writes its rows with.
    pub(super) codec: Mutex<Codec>,
    /// Held by a pass of maintenance, so that two never pack the same rows.
    pub(super) maintaining: Mutex<()>,
    /// The newest event each stream's head took on time, by the stream's id,
    /// which tells whether the next event is late: kept while
    /// packing leaves rows in the head, forgotten once packing or a drop
    /// empties it, and read from the rows that wait at open and, on a copy,
    /// at each of its home's commits.
    pub(super) waiting: Mutex<HashMap<i64, i64>>,
    /// The events appended lately, held for the tails open.
    pub(super) recent: Recent,
    /// What loggers and writers of lines hold, which the store's background
    /// work writes.
    pub(super) held: Mutex<Held>,
    /// What reads, packing, merges and polls met that no longer reads, kept
    /// while the store runs for a drop to remove.
    pub(super) damaged: Mutex<Damaged>,
    _claim: Claim,
}

/// A stream as the engine knows it.
#[derive(Clone, Debug)]
pub(crate) struct Stream {
    pub(crate) id: i64,
    pub(crate) name: Arc<str>,
    /// What the file says it keeps.
    keep: Option<Duration>,
    /// What an open of this run gave: another is refused till the store
    /// closes.
    said: Option<Duration>,
}

impl Stream {
    pub(crate) fn keeps(&self) -> Duration {
        self.keep.unwrap_or(KEEP)
    }
}

impl Engine for Records {
    /// Writes what loggers and writers of lines hold first: a line given
    /// before the close is kept, and one given after it is dropped and counted.
    fn close(&self) -> Result<()> {
        let flushed = self.flush_held(true);
        self.recent.close();
        self.file.close().and(flushed)
    }

    /// Writes what loggers and writers of lines hold, then copies records.db.
    fn snapshot(&self, into: &std::path::Path) -> Result<Vec<crate::engine::Copied>> {
        self.flush_held(false)?;
        Ok(vec![crate::engine::copy_file(&self.file, FILE, "records", into)?])
    }

    /// The streams a copy knows are those its home had made when it last
    /// read them: read again once its home's commits arrive.
    fn applied(&self, file: &str) {
        if file != FILE {
            return;
        }
        match (read_streams(&self.file), read_waiting(&self.file)) {
            (Ok(streams), Ok(waiting)) => {
                *self.streams() = streams;
                *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = waiting;
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(
                    target: "tinystore",
                    %error,
                    "records: a copy's streams, read again after its home's commit",
                );
            }
        }
    }
}

const SELECT_STREAMS: &str = "select id, name, keep from streams";
const SELECT_WAITING: &str = "select stream, max(last_at) from heads where late = 0 group by stream";
const INSERT_STREAM: &str = "insert into streams (name, keep) values (?1, ?2) returning id";
const SET_KEEP: &str = "update streams set keep = ?2 where id = ?1";

impl Records {
    /// The store's records engine, opened by the first logger or events: its
    /// file claimed and migrated, its streams read.
    pub(crate) fn of(store: &Store) -> Result<Arc<Records>> {
        store.engine(|store| {
            let claim = store.claim(FILE)?;
            // 'os' unless the store says: a log line a commit is not worth a sync
            let durability = store.durability().or(Some(Durability::Os));
            let config = Config { page_size: 1024, incremental_vacuum: true, ..Config::committing(durability) };
            let file = store.open_file(claim.name(), config)?;
            file.migrate("records", MIGRATIONS)?;
            let records = Arc::new(Records {
                store: store.downgrade(),
                probe: Arc::clone(store.probe()),
                streams: Mutex::new(read_streams(&file)?),
                waiting: Mutex::new(read_waiting(&file)?),
                file,
                memory: Arc::clone(store.memory()),
                clock: store.clock(),
                codec: Mutex::default(),
                maintaining: Mutex::default(),
                recent: Recent::default(),
                held: Mutex::default(),
                damaged: Mutex::default(),
                _claim: claim,
            });
            // A copy's packing and expiry are its home's, and reach it as pages.
            if records.file.is_copy() {
                return Ok(records);
            }
            let weak = Arc::downgrade(&records);
            store.every("records: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |records| {
                    // a burst of reads otherwise keeps its readers open for good
                    records.file.sweep();
                    records.maintain().map(drop)
                })
            })?;
            Ok(records)
        })
    }

    /// The store's clock, in nanoseconds since 1970.
    pub(crate) fn now(&self) -> i64 {
        unix_nanos(self.clock.now())
    }

    pub(super) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// The store, while it is open.
    pub(super) fn store(&self) -> Option<Store> {
        self.store.upgrade()
    }

    fn streams(&self) -> MutexGuard<'_, HashMap<String, Stream>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every stream the file has, as it is now.
    pub(crate) fn every_stream(&self) -> Vec<Stream> {
        self.streams().values().cloned().collect()
    }

    pub(crate) fn stream(&self, name: &str) -> Option<Stream> {
        self.streams().get(name).cloned()
    }

    /// Opens the stream `name`, made the first time, and gives it `keep`
    /// when the open says one.
    ///
    /// A stream's keep is its own, in the file: an open that says none
    /// changes nothing, and while the store is open one that says another
    /// than an earlier open said is refused, so that one part of a program
    /// never shortens what another keeps.
    pub(crate) fn open_stream(&self, name: &str, keep: Option<Duration>) -> Result<Stream> {
        check_name(name)?;
        // Kept in whole milliseconds: one under a millisecond would read back as none.
        if keep.is_some_and(|keep| keep.as_millis() == 0) {
            return Err(Error::invalid("a stream keeps its events for more than no time"));
        }
        let mut streams = self.streams();
        let Some(stream) = streams.get_mut(name) else {
            let (named, kept) = (name.to_owned(), keep.map(crate::clock::millis));
            let id = self.file.write(name.len(), move |tx| {
                let made = tx.prepare_cached(INSERT_STREAM);
                let id = made.and_then(|mut insert| insert.query_row(params![named, kept], |row| row.get(0)));
                id.map_err(|error| sql_error("write a stream", error))
            })?;
            let stream = Stream { id, name: Arc::from(name), keep, said: keep };
            streams.insert(name.to_owned(), stream.clone());
            return Ok(stream);
        };
        let Some(keep) = keep else { return Ok(stream.clone()) };
        if let Some(said) = stream.said.filter(|said| *said != keep) {
            let (said, keep) = (crate::text::span(said), crate::text::span(keep));
            return Err(Error::invalid(format!("keep {keep}, where the stream is open with keep {said}")));
        }
        if stream.keep != Some(keep) {
            let id = stream.id;
            self.file.write(8, move |tx| {
                execute(tx, SET_KEEP, params![id, crate::clock::millis(keep)], "write a stream's keep")
            })?;
        }
        (stream.keep, stream.said) = (Some(keep), Some(keep));
        Ok(stream.clone())
    }
}

fn read_streams(file: &File) -> Result<HashMap<String, Stream>> {
    file.read(|connection| {
        let mut select = connection.prepare(SELECT_STREAMS).map_err(|error| sql_error("read the streams", error))?;
        let streams = select.query_map([], |row| {
            let (name, keep): (String, Option<i64>) = (row.get(1)?, row.get(2)?);
            let keep = keep.map(|millis| Duration::from_millis(millis.max(0) as u64));
            Ok((name.clone(), Stream { id: row.get(0)?, name: Arc::from(name), keep, said: None }))
        });
        let streams = streams.and_then(Iterator::collect);
        streams.map_err(|error| sql_error("read the streams", error))
    })
}

/// The newest event waiting on time in each stream's head.
fn read_waiting(file: &File) -> Result<HashMap<i64, i64>> {
    let failed = |error| sql_error("read what the heads hold", error);
    file.read(|connection| {
        let mut select = connection.prepare(SELECT_WAITING).map_err(failed)?;
        let waiting = select.query_map([], |row| Ok((row.get(0)?, row.get(1)?)));
        waiting.and_then(Iterator::collect).map_err(failed)
    })
}

pub(super) use crate::sqlite::execute_cached as execute;

pub(super) fn nanos(span: Duration) -> i64 {
    i64::try_from(span.as_nanos()).unwrap_or(i64::MAX)
}

fn check_name(name: &str) -> Result<()> {
    crate::text::check_name("a stream's", name)
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
