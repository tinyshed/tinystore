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
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension, params};

use super::disk::{Disk, OBJECTS, UPLOADS};
use super::ids::Ids;
use super::scrub::Scrubbing;
use crate::engine::{Claim, Copied, Engine, Host, copy_file};
use crate::sqlite::{Config, File, Migration, Tx, own_work, sql_error};
use crate::sync::lock;
use crate::{Clock, Durability, Error, Memory, Result, Store, unix_millis};

const SETTLE: &str = "update meta set value = ?1 where name = 'settled' and value < ?1";
const RESERVE_IDS: &str = "update meta set value = value + ?1 where name = 'ids' returning value";
const HAS_CONTENT: &str = "select 1 from contents where id = ?1";
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often maintenance runs: expiry, marked clears, files no row names any
/// more, what uploads left, and a slice of the scrub.
const MAINTENANCE: Duration = Duration::from_secs(60);

pub(crate) struct Blobs {
    pub(crate) file: File,
    /// What blobs tells the store's own metrics.
    pub(crate) probe: Arc<crate::probe::Probe>,
    pub(crate) disk: Disk,
    pub(crate) ids: Ids,
    pub(crate) memory: Arc<Memory>,
    pub(crate) scrubbing: Mutex<Option<Scrubbing>>,
    clock: Arc<dyn Clock>,
    /// The disk the store keeps free, which no file may take.
    keep_free: u64,
    sets: Mutex<HashMap<String, i64>>,
    /// The store, for the writes a copy passes to its home.
    #[cfg(feature = "replication")]
    store: crate::store::WeakStore,
    /// The store's pool, which an awaited call on files made from blocking
    /// ones runs on: the pool holds no store, so this keeps none open.
    #[cfg(feature = "async")]
    pub(crate) pool: Arc<crate::pool::Pool>,
    _claim: Claim,
}

/// The files of `objects/` that the copy of blobs.db in `store_dir` names
/// and the store lacks, by their paths in its directory: what a node fetches
/// from the node it copies blobs.db from.
#[cfg(feature = "replication")]
pub(crate) fn objects_missing(store_dir: &std::path::Path) -> Result<Vec<String>> {
    let paths = object_ids(store_dir, SELECT_NAMED, [])?.into_iter().map(object_in_store);
    Ok(paths.filter(|path| !store_dir.join(path).exists()).collect())
}

/// The files of `objects/` the blobs.db in `store_dir` holds past the id
/// `after`, oldest first, each with its id and its path in the store's
/// directory: what a node sends the nodes taking blobs.db from it before the
/// commits that name them. One its disk does not hold yet is left out.
#[cfg(feature = "replication")]
pub(crate) fn objects_after(store_dir: &std::path::Path, after: i64) -> Result<Vec<(i64, String)>> {
    let paths = object_ids(store_dir, SELECT_AFTER, [after])?.into_iter().map(|id| (id, object_in_store(id)));
    Ok(paths.filter(|(_, path)| store_dir.join(path).exists()).collect())
}

/// The content ids `select` reads from the blobs.db in `store_dir`, which
/// no process need hold; none when there is no blobs.db.
#[cfg(feature = "replication")]
fn object_ids<const N: usize>(store_dir: &std::path::Path, select: &str, params: [i64; N]) -> Result<Vec<i64>> {
    let db = store_dir.join(DIR).join(FILE);
    if !db.exists() {
        return Ok(Vec::new());
    }
    crate::sqlite::read_copy(&db, |connection| {
        let mut statement = connection.prepare(select)?;
        let ids = statement.query_map(rusqlite::params_from_iter(params), |row| row.get::<_, i64>(0))?;
        ids.collect::<rusqlite::Result<Vec<i64>>>()
    })
}

/// Content `id`'s file by its path in the store's directory.
#[cfg(feature = "replication")]
fn object_in_store(id: i64) -> String {
    format!("{DIR}/{}", Disk::object_name(id))
}

/// Whether `path` names a file of `objects/`, as [`objects_missing`] writes
/// one: what a node serves of blobs beside blobs.db.
#[cfg(feature = "replication")]
pub(crate) fn is_object(path: &str) -> bool {
    let hex = |part: &str| !part.is_empty() && part.len() <= 16 && part.bytes().all(|b| b.is_ascii_hexdigit());
    let mut parts = path.split('/');
    parts.next() == Some(DIR)
        && parts.next() == Some(OBJECTS)
        && parts.next().is_some_and(hex)
        && parts.next().is_some_and(hex)
        && parts.next().is_none()
}

/// blobs.db, as a node names it.
#[cfg(feature = "replication")]
pub(crate) const FILE_IN_STORE: &str = "blobs/blobs.db";

impl Engine for Blobs {
    fn close(&self) -> Result<()> {
        self.file.close()
    }

    /// Copies blobs.db and links beside the copy every file it names: a link
    /// copies no byte and keeps the bytes after the engine lets its own name
    /// go, so a film costs the snapshot a directory entry. No file is removed
    /// meanwhile, so that none the copy names goes before it is linked.
    fn snapshot(&self, into: &std::path::Path) -> Result<Vec<Copied>> {
        let _removals = self.disk.hold_removals();
        let mut copied = vec![copy_file(&self.file, &format!("{DIR}/{FILE}"), "blobs", into)?];
        let named = crate::sqlite::read_copy(&into.join(DIR).join(FILE), |connection| {
            let mut statement = connection.prepare(SELECT_NAMED)?;
            let ids = statement.query_map([], |row| row.get::<_, i64>(0))?;
            ids.collect::<rusqlite::Result<Vec<i64>>>()
        })
        .map_err(|error| error.within("snapshot blobs: the files the copy names"))?;
        for id in named {
            if let Some(name) = self.disk.link_object(id, &into.join(DIR))? {
                copied.push(Copied { name: format!("{DIR}/{name}"), engine: "blobs" });
            }
        }
        Ok(copied)
    }

    /// A set's name and id are its home's, read again on a copy once its
    /// home's commits arrive.
    fn applied(&self, _: &str) {
        lock(&self.sets).clear();
    }
}

/// blobs' directory in the store's, and its file in it.
const DIR: &str = "blobs";
const FILE: &str = "blobs.db";
const SELECT_NAMED: &str = "select id from contents where inline = 0 and names > 0 order by id";
#[cfg(feature = "replication")]
const SELECT_AFTER: &str = "select id from contents where inline = 0 and id > ?1 order by id";

const SELECT_MARKS: &str =
    "select (select value from meta where name = 'ids'), (select value from meta where name = 'settled')";
const NEXT_REVISION: &str = "update meta set value = value + 1 where name = 'revision' returning value";
const SELECT_SET: &str = "select id from sets where name = ?1";
const INSERT_SET: &str = "insert into sets (name) values (?1) on conflict (name) do nothing";

impl Blobs {
    /// The store's blobs engine, opened by the first files: its directory
    /// claimed and made, its file migrated, and what a process that died left
    /// removed before anything is written.
    pub(crate) fn of(store: &Store) -> Result<Arc<Blobs>> {
        // found first: an engine's open holds the lock a second open waits for
        #[cfg(feature = "async")]
        let pool = crate::pool::Pool::of(store)?;
        store.engine(|store| {
            let claim = store.claim(DIR)?;
            let dir = claim.path().to_owned();
            // a store beside its holder makes nothing in the holder's directory:
            // without blobs.db there, its open is `NotFound`
            if !store.is_beside() {
                for made in [dir.clone(), dir.join(UPLOADS), dir.join(OBJECTS)] {
                    std::fs::create_dir_all(&made)
                        .map_err(|error| Error::io(format!("blobs: create {}", made.display()), error))?;
                }
            }
            let config = Config::committing(store.durability());
            let durable = config.durability == Durability::Full;
            let file = store.open_file(&format!("{DIR}/{FILE}"), config)?;
            file.migrate("blobs", MIGRATIONS)?;
            let blobs = Arc::new(Blobs {
                file,
                probe: Arc::clone(store.probe()),
                disk: Disk::new(dir, durable),
                ids: Ids::default(),
                memory: Arc::clone(store.memory()),
                scrubbing: Mutex::new(None),
                clock: store.clock(),
                keep_free: store.keep_free(),
                sets: Mutex::new(HashMap::new()),
                #[cfg(feature = "replication")]
                store: store.downgrade(),
                #[cfg(feature = "async")]
                pool,
                _claim: claim,
            });
            // What a process that died left, and the maintenance after it,
            // are a copy's home's to clear: each turn asks, since a file's
            // home may move while it is open.
            if !blobs.file.is_copy() {
                blobs.recover()?;
            }
            let weak = Arc::downgrade(&blobs);
            store.every("blobs: maintenance", MAINTENANCE, move || {
                weak.upgrade().filter(|blobs| !blobs.file.is_copy()).map_or(Ok(()), |blobs| {
                    // a burst of reads otherwise keeps its readers open for good
                    blobs.file.sweep();
                    blobs.maintain().map(drop)
                })
            })?;
            Ok(blobs)
        })
    }

    /// The store blobs opened in, while it is open.
    #[cfg(feature = "replication")]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed("the store"))
    }

    /// Refuses `coming` bytes that would leave the disk less free than the
    /// store keeps, asking the system how much is free now.
    pub(crate) fn room(&self, coming: u64) -> Result<()> {
        if self.keep_free == 0 {
            return Ok(());
        }
        let free = crate::space::available(self.disk.dir())
            .map_err(|error| Error::io("blobs: the disk's free space", error))?;
        if free >= self.keep_free.saturating_add(coming) {
            return Ok(());
        }
        Err(Error::limit(format!(
            "{coming} bytes more would leave the disk {} bytes free, under the {} the store keeps",
            free.saturating_sub(coming),
            self.keep_free
        )))
    }

    /// Takes up the file's marks and removes what a process that died left:
    /// every file of `uploads/`, and in the directories of `objects/` that
    /// hold the ids past the settled mark, each file no content names. Only an
    /// upload makes such a file, between its rename and its commit, and every
    /// id at or below the mark is committed or has none, so nothing else is
    /// walked.
    fn recover(&self) -> Result<()> {
        let (reserved, settled): (i64, i64) = self.file.read(|connection| {
            connection
                .query_row(SELECT_MARKS, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|error| sql_error("blobs: read the marks", error))
        })?;
        self.ids.start(reserved);
        // An upload left that will not go now, as one Windows still shares, is
        // swept later and keeps no store from opening.
        if let Err(error) = self.disk.sweep_uploads(&self.ids) {
            tracing::warn!(target: "tinystore", %error, "blobs: an upload left by a process that died stays for now");
        }
        if settled < reserved {
            for fan in Disk::fan(settled + 1)..=Disk::fan(reserved) {
                self.remove_unnamed_in(fan, settled, reserved)?;
            }
        }
        self.settle()
    }

    /// Removes the files of one directory of `objects/` whose ids lie past
    /// `settled`, up to `reserved`, and that no content names. One that will
    /// not go now, as one a scanner holds on Windows, stays in doubt: the
    /// settled mark stays below it, and maintenance removes it later.
    fn remove_unnamed_in(&self, fan: i64, settled: i64, reserved: i64) -> Result<()> {
        let ids = self.disk.ids_in(fan)?;
        if ids.is_empty() {
            return Ok(());
        }
        let (low, high) = Disk::span(fan);
        let (low, high) = (low.max(settled + 1), high.min(reserved));
        let named = ("select id from contents where id >= ?1 and id <= ?2", "blobs: the contents named");
        let named: Vec<i64> =
            self.file.read(|connection| rows_of(connection, named, params![low, high], |row| row.get(0)))?;
        for id in ids.into_iter().filter(|id| (low..=high).contains(id) && !named.contains(id)) {
            if let Err(error) = self.disk.remove_object(id) {
                tracing::warn!(target: "tinystore", %error, "blobs: a file left by a process that died stays for now");
                self.ids.doubt(id);
            }
        }
        Ok(())
    }

    /// Moves the settled mark up to what no upload holds, so that the next
    /// open walks only the directories of the ids reserved since.
    pub(crate) fn settle(&self) -> Result<()> {
        let mark = self.ids.settled_mark();
        own_work(|| {
            self.file.write(0, move |tx| {
                tx.prepare_cached(SETTLE)
                    .and_then(|mut update| update.execute([mark]))
                    .map(drop)
                    .map_err(|error| sql_error("blobs: settle the content ids", error))
            })
        })
    }

    /// The next content id, held until its upload ends; a block is reserved
    /// in a write of its own when the last one is spent.
    pub(crate) fn take_id(&self) -> Result<i64> {
        self.ids.take(|block| {
            own_work(|| {
                self.file.write(0, move |tx| {
                    tx.prepare_cached(RESERVE_IDS)
                        .and_then(|mut reserve| reserve.query_row([block], |row| row.get(0)))
                        .map_err(|error| sql_error("blobs: reserve content ids", error))
                })
            })
        })
    }

    /// Learns what became of a file content whose commit's outcome is
    /// unknown: a content row says it committed, and none that it did not, so
    /// its file goes. A file that cannot answer leaves the id in doubt.
    pub(crate) fn resolve(&self, id: i64) {
        let found = self.file.read(|connection| {
            connection
                .prepare_cached(HAS_CONTENT)
                .and_then(|mut select| select.query_row([id], |_| Ok(())))
                .optional()
                .map_err(|error| sql_error("blobs: a content in doubt", error))
        });
        match found {
            Ok(Some(())) => self.ids.let_go(id),
            Ok(None) if self.disk.remove_object(id).is_ok() => self.ids.let_go(id),
            _ => self.ids.doubt(id),
        }
    }

    /// The revision of one write, taken from the file in the write's own
    /// transaction rather than counted in memory: a copy's file moves under
    /// its engine as its home's commits arrive, and a copy that takes over
    /// writes past every revision its home wrote.
    pub(crate) fn next_revision(&self, tx: &Tx<'_>) -> Result<i64> {
        tx.prepare_cached(NEXT_REVISION)
            .and_then(|mut next| next.query_row([], |row| row.get(0)))
            .map_err(|error| sql_error("blobs: the revision", error))
    }

    /// The id of the set of files `name`, made the first time it is asked.
    pub(crate) fn set_id(&self, name: &str) -> Result<i64> {
        if let Some(&id) = lock(&self.sets).get(name) {
            return Ok(id);
        }
        let owned = name.to_owned();
        // Found before it is made, so that a copy, which writes nothing,
        // opens a set its home made.
        let made = own_work(|| {
            self.file.write(0, move |tx| {
                let failed = |error| sql_error("blobs: a set of files", error);
                let found = tx.query_row(SELECT_SET, [&owned], |row| row.get(0)).optional().map_err(failed)?;
                if let Some(id) = found {
                    return Ok(id);
                }
                tx.execute(INSERT_SET, [&owned]).map_err(failed)?;
                tx.query_row(SELECT_SET, [&owned], |row| row.get(0)).map_err(failed)
            })
        });
        let id = made?;
        lock(&self.sets).insert(name.to_owned(), id);
        Ok(id)
    }

    /// The store's clock, as the rows keep it: unix milliseconds.
    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    /// A row's time; a term too far for Windows' clock, such as a ttl of
    /// `Duration::MAX`, reads as the last second of 9999.
    pub(crate) fn time(millis: i64) -> SystemTime {
        crate::clock::from_unix_millis(millis)
    }

    /// Removes the files of the contents a commit left without names. One that
    /// will not go stays listed, and maintenance tries it again.
    pub(crate) fn remove_freed(&self, ids: &[i64]) {
        for &id in ids {
            if let Err(error) = self.disk.remove_object(id) {
                tracing::debug!(target: "tinystore", %error, "blobs: a file maintenance removes later");
            }
        }
    }
}

pub(super) use crate::sqlite::query_cached as rows_of;
