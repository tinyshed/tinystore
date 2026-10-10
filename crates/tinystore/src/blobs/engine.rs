//! The blobs engine: `blobs/` in the store's directory, its `blobs.db`, and
//! the files of the contents too large to keep inline.
//!
//! ```text
//! blobs/
//! ├── blobs.db            sets, files, contents, inline bodies, marks
//! ├── uploads/1a3f08      an upload's bytes until its commit; emptied at open
//! └── objects/1a3/1a3f07  content 0x1a3f07's bytes: renamed here once, never changed
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension, params};

use super::disk::{Disk, OBJECTS, UPLOADS};
use super::ids::Ids;
use super::scrub::Scrubbing;
use crate::engine::{Claim, Engine, Host};
use crate::sqlite::{Config, File, Migration, Tx, sql_error};
use crate::{Clock, Durability, Error, Memory, Result, Store, unix_millis};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often maintenance runs: expiry, marked clears, files no row names any
/// more, what uploads left, and a slice of the scrub.
const MAINTENANCE: Duration = Duration::from_secs(60);

pub(crate) struct Blobs {
    pub(crate) file: File,
    pub(crate) disk: Disk,
    pub(crate) ids: Ids,
    pub(crate) memory: Arc<Memory>,
    pub(crate) scrubbing: Mutex<Option<Scrubbing>>,
    clock: Arc<dyn Clock>,
    revision: AtomicI64,
    sets: Mutex<HashMap<String, i64>>,
    _claim: Claim,
}

impl Engine for Blobs {
    fn close(&self) -> Result<()> {
        self.file.close()
    }
}

const SELECT_MARKS: &str = "select (select value from meta where name = 'revision'),
    (select value from meta where name = 'ids'), (select value from meta where name = 'settled')";
const SET_REVISION: &str = "update meta set value = ?1 where name = 'revision'";
const SELECT_SET: &str = "select id from sets where name = ?1";
const INSERT_SET: &str = "insert into sets (name) values (?1) on conflict (name) do nothing";

impl Blobs {
    /// The store's blobs engine, opened by the first files: its directory
    /// claimed and made, its file migrated, and what a process that died left
    /// removed before anything is written.
    pub(crate) fn of(store: &Store) -> Result<Arc<Blobs>> {
        store.engine(|store| {
            let claim = store.claim("blobs")?;
            let dir = claim.path().to_owned();
            for made in [dir.clone(), dir.join(UPLOADS), dir.join(OBJECTS)] {
                std::fs::create_dir_all(&made)
                    .map_err(|error| Error::io(format!("blobs: create {}", made.display()), error))?;
            }
            let config = Config::committing(store.durability());
            let durable = config.durability == Durability::Full;
            let file = File::open(dir.join("blobs.db"), config)?;
            file.migrate("blobs", MIGRATIONS)?;
            let blobs = Arc::new(Blobs {
                file,
                disk: Disk::new(dir, durable),
                ids: Ids::default(),
                memory: Arc::clone(store.memory()),
                scrubbing: Mutex::new(None),
                clock: store.clock(),
                revision: AtomicI64::new(0),
                sets: Mutex::new(HashMap::new()),
                _claim: claim,
            });
            blobs.recover()?;
            let weak = Arc::downgrade(&blobs);
            store.every("blobs: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |blobs| blobs.maintain().map(drop))
            })?;
            Ok(blobs)
        })
    }

    /// Takes up the file's marks and removes what a process that died left:
    /// every file of `uploads/`, and in the directories of `objects/` that
    /// hold the ids past the settled mark, each file no content names. Only an
    /// upload makes such a file, between its rename and its commit, and every
    /// id at or below the mark is committed or has none, so nothing else is
    /// walked.
    fn recover(&self) -> Result<()> {
        let (revision, reserved, settled): (i64, i64, i64) = self.file.read(|connection| {
            connection
                .query_row(SELECT_MARKS, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|error| sql_error("blobs: read the marks", error))
        })?;
        self.revision.store(revision, Ordering::SeqCst);
        self.ids.start(reserved);
        self.disk.sweep_uploads(&self.ids)?;
        if settled < reserved {
            for fan in Disk::fan(settled + 1)..=Disk::fan(reserved) {
                self.remove_unnamed_in(fan, settled, reserved)?;
            }
        }
        self.settle()
    }

    /// Removes the files of one directory of `objects/` whose ids lie past
    /// `settled`, up to `reserved`, and that no content names.
    fn remove_unnamed_in(&self, fan: i64, settled: i64, reserved: i64) -> Result<()> {
        let ids = self.disk.ids_in(fan)?;
        if ids.is_empty() {
            return Ok(());
        }
        let (low, high) = Disk::span(fan);
        let (low, high) = (low.max(settled + 1), high.min(reserved));
        let named: Vec<i64> = self.file.read(|connection| {
            let mut statement = connection
                .prepare_cached("select id from contents where id >= ?1 and id <= ?2")
                .map_err(|error| sql_error("blobs: the contents named", error))?;
            let rows = statement
                .query_map(params![low, high], |row| row.get(0))
                .map_err(|error| sql_error("blobs: the contents named", error))?;
            rows.collect::<rusqlite::Result<Vec<i64>>>().map_err(|error| sql_error("blobs: the contents named", error))
        })?;
        for id in ids.into_iter().filter(|id| (low..=high).contains(id) && !named.contains(id)) {
            self.disk.remove_object(id)?;
        }
        Ok(())
    }

    /// Moves the settled mark up to what no upload holds, so that the next
    /// open walks only the directories of the ids reserved since.
    pub(crate) fn settle(&self) -> Result<()> {
        let mark = self.ids.settled_mark();
        self.file.write(0, move |tx| {
            tx.execute("update meta set value = ?1 where name = 'settled' and value < ?1", [mark])
                .map(drop)
                .map_err(|error| sql_error("blobs: settle the content ids", error))
        })
    }

    /// The next content id, held until its upload ends; a block is reserved
    /// in a write of its own when the last one is spent.
    pub(crate) fn take_id(&self) -> Result<i64> {
        self.ids.take(|block| {
            self.file.write(0, move |tx| {
                tx.query_row("update meta set value = value + ?1 where name = 'ids' returning value", [block], |row| {
                    row.get(0)
                })
                .map_err(|error| sql_error("blobs: reserve content ids", error))
            })
        })
    }

    /// Learns what became of a file content whose commit's outcome is
    /// unknown: a content row says it committed, and none that it did not, so
    /// its file goes. A file that cannot answer leaves the id in doubt.
    pub(crate) fn resolve(&self, id: i64) {
        let found = self.file.read(|connection| {
            connection
                .query_row("select 1 from contents where id = ?1", [id], |_| Ok(()))
                .optional()
                .map_err(|error| sql_error("blobs: a content in doubt", error))
        });
        match found {
            Ok(Some(())) => self.ids.let_go(id),
            Ok(None) if self.disk.remove_object(id).is_ok() => self.ids.let_go(id),
            _ => self.ids.doubt(id),
        }
    }

    /// The revision of one write, kept as the file's high-water mark in the
    /// write's own transaction; a write rolled back leaves a gap, never a
    /// repeat.
    pub(crate) fn next_revision(&self, tx: &Tx<'_>) -> Result<i64> {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        tx.execute(SET_REVISION, [revision]).map_err(|error| sql_error("blobs: the revision", error))?;
        Ok(revision)
    }

    /// The id of the set of files `name`, made the first time it is asked.
    pub(crate) fn set_id(&self, name: &str) -> Result<i64> {
        if let Some(&id) = lock(&self.sets).get(name) {
            return Ok(id);
        }
        let owned = name.to_owned();
        let id = self.file.write(0, move |tx| {
            tx.execute(INSERT_SET, [&owned]).map_err(|error| sql_error("blobs: a set of files", error))?;
            tx.query_row(SELECT_SET, [&owned], |row| row.get(0))
                .map_err(|error| sql_error("blobs: a set of files", error))
        })?;
        lock(&self.sets).insert(name.to_owned(), id);
        Ok(id)
    }

    /// The store's clock, as the rows keep it: unix milliseconds.
    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    pub(crate) fn time(millis: i64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).unwrap_or(0))
    }

    /// Removes the files of the contents a commit left without names. One that
    /// will not go stays listed, and maintenance tries it again.
    pub(crate) fn let_files_go(&self, ids: &[i64]) {
        for &id in ids {
            if let Err(error) = self.disk.remove_object(id) {
                tracing::debug!(target: "tinystore", %error, "blobs: a file maintenance removes later");
            }
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
