//! The blobs engine: `blobs/` in the store's directory, its `blobs.db`, and
//! the files of the contents too large to keep inline; or in a database's
//! file, its tables beside the database's and its bytes in the folder beside
//! the file, so that a file commits with the rows that name it.
//!
//! ```text
//! blobs/
//! ├── blobs.db            sets, files, contents, inline bodies, marks
//! ├── uploads/1a3f08      an upload's bytes until its commit; emptied at open
//! └── objects/1a3/1a3f07  content 0x1a3f07's bytes: renamed here once, never changed
//! sql/app.db              a database's files: the same tables, _tinystore_blobs_…
//! sql/app.blobs/          and the same folders
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension, params};

use super::disk::{Disk, OBJECTS, UPLOADS};
use super::ids::Ids;
use super::scrub::Scrubbing;
use crate::engine::{Claim, Copied, Copying, Engine, Host, Then, to_copy};
use crate::sqlite::{Config, File, Migration, Tx, own_work, sql_error};
use crate::sync::lock;
use crate::{Clock, Durability, Error, Memory, Result, Store, unix_millis};

const SETTLE: &str = "update _tinystore_blobs_meta set value = ?1 where name = 'settled' and value < ?1";
const RESERVE_IDS: &str = "update _tinystore_blobs_meta set value = value + ?1 where name = 'ids' returning value";
const HAS_CONTENT: &str = "select 1 from _tinystore_blobs_contents where id = ?1";
const MIGRATIONS: &[Migration] = &[
    Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql")),
    Migration::of(2, "0002_names.sql", include_str!("migrations/0002_names.sql")),
];
/// A database's files, whose tables are made with blobs.db's names of its
/// step 2: a step after it goes to both histories, the next of each.
#[cfg(feature = "sql")]
const IN_A_DATABASE: &[Migration] =
    &[Migration::of(1, "0001_in_a_database.sql", include_str!("migrations/0001_in_a_database.sql"))];
/// A content id taken in a database's transaction, which a rollback gives
/// back with every row the transaction wrote.
#[cfg(feature = "sql")]
const TAKE_ID: &str = "update _tinystore_blobs_meta set value = value + 1 where name = 'ids' returning value";

/// How often maintenance runs: expiry, marked clears, files no row names any
/// more, what uploads left, and a slice of the scrub.
const MAINTENANCE: Duration = Duration::from_secs(60);

pub(crate) struct Blobs {
    pub(crate) file: Arc<File>,
    /// Where it is kept: blobs.db, or a database's file.
    pub(crate) kept: Kept,
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
/// no process need hold; none when there is no blobs.db, or one whose tables
/// are not made yet.
///
/// A home serves its blobs.db from its first commit, before its migrations',
/// and a copy may hold that commit whole: read as an error, the home's link
/// ended, and the copy's ended again at each turn, never taking the commit
/// that made the tables.
#[cfg(feature = "replication")]
fn object_ids<const N: usize>(store_dir: &std::path::Path, select: &str, params: [i64; N]) -> Result<Vec<i64>> {
    let db = store_dir.join(DIR).join(FILE);
    if !db.exists() {
        return Ok(Vec::new());
    }
    crate::sqlite::read_copy(&db, |connection| {
        if connection.query_row(HAS_CONTENTS, [], |row| row.get::<_, i64>(0))? == 0 {
            return Ok(Vec::new());
        }
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

/// Where a blobs engine is kept, by the paths in the store's directory of its
/// file and of the folder its bytes lie in.
#[derive(Debug)]
pub(crate) struct Kept {
    pub(crate) file: String,
    pub(crate) folder: String,
    /// The database that lends its file, as an error names it: `sql app`.
    pub(crate) lent_by: Option<String>,
}

impl Engine for Blobs {
    /// Closes blobs.db; a database's file is the database's to close.
    fn close(&self) -> Result<()> {
        if self.kept.lent_by.is_some() {
            return Ok(());
        }
        self.file.close()
    }

    /// Names blobs.db, and once it is copied links beside the copy every
    /// file it names: a link copies no byte and keeps the bytes after the
    /// engine lets its own name go, so a film costs the snapshot a directory
    /// entry. No removal begins from the moment on, so that none the copy
    /// names goes before it is linked. A database's file is the database's
    /// to name, and its files are linked once it is copied.
    fn snapshot(&self) -> Result<Copying<'_>> {
        let mut copying = match self.kept.lent_by {
            None => to_copy(&self.file, &self.kept.file, "blobs")?,
            Some(_) => Copying::default(),
        };
        copying.at_the_moment = Some(Box::new(move || {
            let paused = self.disk.pause_removals();
            let then: Then<'_> = Box::new(move |into| {
                let _paused = paused;
                self.link_named(into)
            });
            then
        }));
        Ok(copying)
    }

    /// A set's name and id are its home's, read again on a copy once its
    /// home's commits arrive.
    fn applied(&self, _: &str) {
        lock(&self.sets).clear();
    }
}

impl Blobs {
    /// Links into the snapshot at `into` every file its copy of the file names.
    fn link_named(&self, into: &std::path::Path) -> Result<Vec<Copied>> {
        let named = crate::sqlite::read_copy(&into.join(&self.kept.file), |connection| {
            let mut statement = connection.prepare(SELECT_NAMED)?;
            let ids = statement.query_map([], |row| row.get::<_, i64>(0))?;
            ids.collect::<rusqlite::Result<Vec<i64>>>()
        })
        .map_err(|error| error.within("snapshot blobs: the files the copy names"))?;
        let mut linked = Vec::new();
        for id in named {
            if let Some(name) = self.disk.link_object(id, &into.join(&self.kept.folder))? {
                linked.push(Copied { name: format!("{}/{name}", self.kept.folder), engine: "blobs" });
            }
        }
        Ok(linked)
    }
}

/// What opens a blobs engine on its file: where it is kept, the name of its
/// folder it holds, and its file's history.
struct Opening<'s> {
    store: &'s Store,
    file: Arc<File>,
    kept: Kept,
    claim: Claim,
    migrations: &'static [Migration],
    #[cfg(feature = "async")]
    pool: Arc<crate::pool::Pool>,
}

impl Opening<'_> {
    /// Its folders made, its file migrated, and what a process that died left
    /// removed before anything is written; then its maintenance started.
    fn start(self) -> Result<Arc<Blobs>> {
        let (store, dir) = (self.store, self.claim.path().to_owned());
        // a store beside its holder makes nothing in the holder's directory
        if !store.is_beside() {
            for made in [dir.clone(), dir.join(UPLOADS), dir.join(OBJECTS)] {
                std::fs::create_dir_all(&made)
                    .map_err(|error| Error::io(format!("blobs: create {}", made.display()), error))?;
            }
        }
        self.file.migrate("blobs", self.migrations)?;
        let durable = self.file.durability() == Durability::Full;
        let tasks = self.kept.lent_by.as_ref().map_or_else(|| "blobs".to_owned(), |owner| format!("blobs in {owner}"));
        let blobs = Arc::new(Blobs {
            file: self.file,
            kept: self.kept,
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
            pool: self.pool,
            _claim: self.claim,
        });
        // What a process that died left, and the maintenance after it, are a
        // copy's home's to clear: each turn asks, since a file's home may
        // move while it is open.
        if !blobs.file.is_copy() {
            blobs.recover()?;
        }
        let weak = Arc::downgrade(&blobs);
        store.every(&format!("{tasks}: maintenance"), MAINTENANCE, move || {
            weak.upgrade().filter(|blobs| !blobs.file.is_copy()).map_or(Ok(()), |blobs| {
                // a burst of reads otherwise keeps its readers open for good
                blobs.file.sweep();
                blobs.maintain().map(drop)
            })
        })?;
        Ok(blobs)
    }
}

/// blobs' directory in the store's, and its file in it.
const DIR: &str = "blobs";
const FILE: &str = "blobs.db";
const SELECT_NAMED: &str = "select id from _tinystore_blobs_contents where inline = 0 and names > 0 order by id";
#[cfg(feature = "replication")]
const SELECT_AFTER: &str = "select id from _tinystore_blobs_contents where inline = 0 and id > ?1 order by id";
#[cfg(feature = "replication")]
const HAS_CONTENTS: &str = "select count(*) from sqlite_schema where name = '_tinystore_blobs_contents'";

const SELECT_MARKS: &str = "select (select value from _tinystore_blobs_meta where name = 'ids'), (select value from _tinystore_blobs_meta where name = 'settled')";
const NEXT_REVISION: &str =
    "update _tinystore_blobs_meta set value = value + 1 where name = 'revision' returning value";
const SELECT_SET: &str = "select id from _tinystore_blobs_sets where name = ?1";
const INSERT_SET: &str = "insert into _tinystore_blobs_sets (name) values (?1) on conflict (name) do nothing";

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
            let config = Config::committing(store.durability());
            let file = Arc::new(store.open_file(&format!("{DIR}/{FILE}"), config)?);
            let kept = Kept { file: format!("{DIR}/{FILE}"), folder: DIR.to_owned(), lent_by: None };
            #[cfg(feature = "async")]
            let opening = Opening { store, file, kept, claim, migrations: MIGRATIONS, pool };
            #[cfg(not(feature = "async"))]
            let opening = Opening { store, file, kept, claim, migrations: MIGRATIONS };
            opening.start()
        })
    }

    /// The blobs engine kept in a database's file, which every set of files
    /// opened from the database shares: its bytes lie in the folder beside
    /// the file, `sql/app.blobs/` beside `sql/app.db`.
    #[cfg(feature = "sql")]
    pub(crate) fn in_file(shared: &Arc<crate::engine::SharedFile>) -> Result<Arc<Blobs>> {
        shared.engine(|store| {
            #[cfg(feature = "replication")]
            if store.node().is_some() {
                return Err(Error::invalid(format!(
                    "{}: a database's files on a node, whose copies take no bytes of a database yet",
                    shared.owner()
                )));
            }
            let in_store = shared.file().path().strip_prefix(store.dir()).map_err(|_| {
                Error::internal(format!("{}: a database's file outside its store's directory", shared.owner()))
            })?;
            let file_name = in_store.to_string_lossy().replace('\\', "/");
            let folder = format!("{}.blobs", file_name.strip_suffix(".db").unwrap_or(&file_name));
            let claim = store.claim(&folder)?;
            let kept = Kept { file: file_name, folder, lent_by: Some(shared.owner().to_owned()) };
            let file = Arc::clone(shared.file());
            #[cfg(feature = "async")]
            let opening =
                Opening { store, file, kept, claim, migrations: IN_A_DATABASE, pool: crate::pool::Pool::of(store)? };
            #[cfg(not(feature = "async"))]
            let opening = Opening { store, file, kept, claim, migrations: IN_A_DATABASE };
            opening.start()
        })
    }

    /// The store blobs opened in, while it is open.
    #[cfg(feature = "replication")]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed("the store"))
    }

    /// The file as an error names it: `blobs.db`, or `sql app`.
    #[cfg(feature = "sql")]
    pub(crate) fn place(&self) -> &str {
        self.kept.lent_by.as_deref().unwrap_or(FILE)
    }

    /// A content id taken in a database's transaction: the meta row it moves
    /// is the transaction's, so that a rollback gives it back and no content
    /// that committed has an id past what its file reserved.
    #[cfg(feature = "sql")]
    pub(crate) fn id_in(&self, tx: &Tx<'_>) -> Result<i64> {
        tx.prepare_cached(TAKE_ID)
            .and_then(|mut take| take.query_row([], |row| row.get(0)))
            .map_err(|error| sql_error("blobs: a content id", error))
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
        // A database's transaction takes its ids in its own rows, so that a
        // file it placed and a crash left lies past the mark: any past it is
        // no content's, as no upload runs before the store opened.
        let last = self.disk.last_fan()?.unwrap_or(0).max(Disk::fan(reserved));
        for fan in Disk::fan(settled + 1)..=last {
            self.remove_unnamed_in(fan, settled)?;
        }
        self.settle()
    }

    /// Removes the files of one directory of `objects/` whose ids lie past
    /// `settled` and that no content names. One that will not go now, as one
    /// a scanner holds on Windows, stays in doubt: the settled mark stays
    /// below it, and maintenance removes it later.
    fn remove_unnamed_in(&self, fan: i64, settled: i64) -> Result<()> {
        let ids = self.disk.ids_in(fan)?;
        if ids.is_empty() {
            return Ok(());
        }
        let (low, high) = Disk::span(fan);
        let low = low.max(settled + 1);
        let named =
            ("select id from _tinystore_blobs_contents where id >= ?1 and id <= ?2", "blobs: the contents named");
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

/// The sets of files blobs.db holds, by name. None while the store has no
/// blobs, which a read beside would otherwise try to make.
#[cfg(feature = "client")]
pub(crate) fn sets(store: &Store) -> Result<Vec<String>> {
    const SELECT_SETS: &str = "select name from _tinystore_blobs_sets order by name";
    if !store.dir().join("blobs").join("blobs.db").exists() {
        return Ok(Vec::new());
    }
    Blobs::of(store)?.file.read(|c| {
        let mut select = c.prepare_cached(SELECT_SETS).map_err(|error| sql_error("blobs: its sets", error))?;
        let rows = select.query_map([], |row| row.get(0));
        rows.and_then(Iterator::collect).map_err(|error| sql_error("blobs: its sets", error))
    })
}

#[cfg(all(test, feature = "replication"))]
mod tests {
    use super::*;

    #[test]
    fn a_blobs_db_whose_tables_are_not_made_yet_holds_no_object() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(DIR)).unwrap();
        // a home's first commit of blobs.db, before its migrations': SQLite's file, none of blobs' tables
        let first = rusqlite::Connection::open(dir.path().join(DIR).join(FILE)).unwrap();
        first.execute_batch("create table first (x)").unwrap();
        drop(first);
        assert_eq!(objects_after(dir.path(), 0).unwrap(), Vec::<(i64, String)>::new());
        assert_eq!(objects_missing(dir.path()).unwrap(), Vec::<String>::new());
    }
}
