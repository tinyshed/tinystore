//! The search engine: `search.db` in the store's directory, the indexes it
//! holds, and the work that keeps them fast: packing each head into blocks,
//! purging the documents gone, and building again a dictionary of words
//! that fell behind.
//!
//! ```text
//! set ──► head, a grouped commit ── maintenance ──► blocks, a step a commit
//! search ◄── blocks and head, one snapshot
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use super::analyzer;
use super::correct::Corrector;
use super::kept::{Kept, lock};
use super::pack;
use super::purge::{self, Purge};
use super::read::{self, Answer, Asked, Query};
use super::score::Part;
use super::settings::{Settings, Wanted};
use super::write::{self, Prepared};
use crate::engine::{Claim, Engine, Host, Wake};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::{Error, Memory, Result, Store};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// search's file in the store's directory.
const FILE: &str = "search.db";
/// How often maintenance looks for work, unless a set wakes it sooner.
const MAINTENANCE: Duration = Duration::from_secs(10);
/// Documents set since a head was packed that make it due: search-compact
/// packed every thousand writes, which kept `or`'s p99 at 14–16 ms through
/// 15,000 writes where a head never packed reached 37.
const PACK_AT: u64 = 1000;
/// How long a dictionary of an index's words serves after they change
/// before background work builds it again.
const DICTIONARY_AGE: Duration = Duration::from_secs(60);

const READ_INDEX: &str = "select id, settings from _tinystore_search_indexes where name = ?1";
const ADD_INDEX: &str = "insert into _tinystore_search_indexes (name, settings) values (?1, ?2) returning id";
const WRITE_SETTINGS: &str = "update _tinystore_search_indexes set settings = ?2 where id = ?1";
const COUNT_DOCUMENTS: &str = "select documents from _tinystore_search_indexes where id = ?1";
/// The documents set since packing last reached the head, and whether the
/// head holds a posting.
const READ_WAITING: &str = "select
    (select count(*) from _tinystore_search_docs
        where index_id = ?1 and doc >= (select normed from _tinystore_search_indexes where id = ?1)),
    exists (select 1 from _tinystore_search_head where index_id = ?1)";
const READ_WORDS_VERSION: &str = "select words_version from _tinystore_search_indexes where id = ?1";
const READ_VALUE: &str = "select value from _tinystore_search_docs where index_id = ?1 and id = ?2";

pub(crate) struct Search {
    file: File,
    memory: Arc<Memory>,
    indexes: Mutex<HashMap<String, Arc<OpenIndex>>>,
    /// Held by a pass of maintenance, so that two never pack the same head.
    maintaining: Mutex<()>,
    wake: OnceLock<Wake>,
    _claim: Claim,
}

/// An index as the engine holds it while the store is open.
pub(crate) struct OpenIndex {
    pub(crate) id: i64,
    pub(crate) name: String,
    /// Read by a set from its analysis to its commit, so that a change of
    /// fields never lands between them.
    settings: RwLock<Arc<Settings>>,
    kept: Kept,
    /// Sets since its head was last packed whole.
    sets: AtomicU64,
    purge: Mutex<Option<Purge>>,
}

impl std::fmt::Debug for OpenIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenIndex").field("name", &self.name).finish_non_exhaustive()
    }
}

/// What one pass of maintenance did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Maintenance {
    /// Postings packed from heads into blocks.
    pub packed: u64,
    /// Documents gone taken out of the blocks.
    pub purged: u64,
}

impl Engine for Search {
    fn close(&self) -> Result<()> {
        self.file.close()
    }

    fn snapshot(&self, into: &std::path::Path) -> Result<Vec<crate::engine::Copied>> {
        Ok(vec![crate::engine::copy_file(&self.file, FILE, "search", into)?])
    }
}

impl Search {
    /// The store's search engine, opened by its first index: its file
    /// claimed and migrated, its maintenance on the store's schedule.
    pub(crate) fn of(store: &Store) -> Result<Arc<Search>> {
        store.engine(|store| {
            let claim = store.claim(FILE)?;
            let file = File::open(claim.path(), Config::committing(store.durability()))?;
            file.migrate("search", MIGRATIONS)?;
            let search = Arc::new(Search {
                file,
                memory: Arc::clone(store.memory()),
                indexes: Mutex::default(),
                maintaining: Mutex::default(),
                wake: OnceLock::new(),
                _claim: claim,
            });
            let weak = Arc::downgrade(&search);
            let wake = store.every("search: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |search| search.maintain(false).map(drop))
            })?;
            let _ = search.wake.set(wake);
            Ok(search)
        })
    }

    /// Opens the index `name`, made the first time, with the settings
    /// `wanted`; an index that holds others is changed to them.
    pub(crate) fn open_index(&self, name: &str, wanted: &Wanted) -> Result<Arc<OpenIndex>> {
        check_name(name)?;
        wanted.check()?;
        let mut indexes = lock(&self.indexes);
        if let Some(open) = indexes.get(name) {
            self.change(open, wanted)?;
            return Ok(Arc::clone(open));
        }
        let open = match self.file.read(|connection| read_index(connection, name))? {
            Some((id, settings)) => {
                let open = Arc::new(OpenIndex::new(id, name, settings));
                self.change(&open, wanted)?;
                open
            }
            None => {
                let settings = Settings::new(wanted);
                let (named, json) = (name.to_owned(), settings.to_json());
                let id = self.file.write(name.len() + json.len(), move |tx| {
                    let added = tx.prepare_cached(ADD_INDEX);
                    let id = added.and_then(|mut insert| insert.query_row((named, json), |row| row.get(0)));
                    id.map_err(|error| sql_error("write an index", error))
                })?;
                Arc::new(OpenIndex::new(id, name, settings))
            }
        };
        indexes.insert(name.to_owned(), Arc::clone(&open));
        Ok(open)
    }

    /// Changes an index's settings to `wanted`, and says each change in the log.
    fn change(&self, open: &OpenIndex, wanted: &Wanted) -> Result<()> {
        let mut settings = open.settings_to_change();
        if settings.analysis != analyzer::VERSION {
            return Err(Error::invalid("its words were read by another release: reading them again is not built yet"));
        }
        let id = open.id;
        let documents = self.file.read(|connection| count(connection, COUNT_DOCUMENTS, id))?;
        let (changed, said) = settings.change(wanted, documents)?;
        if changed == **settings {
            return Ok(());
        }
        let json = changed.to_json();
        self.file.write(json.len(), move |tx| write::execute(tx, WRITE_SETTINGS, (id, json), "write settings"))?;
        for line in said {
            tracing::info!(target: "tinystore", "search {}: {line}", open.name);
        }
        *settings = Arc::new(changed);
        Ok(())
    }

    /// Sets the documents `documents` makes of the index's settings, which
    /// hold still until they commit, all of them in one commit or none.
    pub(crate) fn set(
        &self,
        open: &OpenIndex,
        documents: impl FnOnce(&Settings) -> Result<Vec<Prepared>>,
    ) -> Result<()> {
        let settings = open.settings_read();
        let prepared = documents(&settings)?;
        let (bytes, count) = (prepared.iter().map(Prepared::bytes).sum(), prepared.len() as u64);
        if count == 0 {
            return Ok(());
        }
        self.file.write(bytes, move |tx| prepared.iter().try_for_each(|document| write::set(tx, document)))?;
        drop(settings);
        let before = open.sets.fetch_add(count, Ordering::Relaxed);
        if before < PACK_AT && before + count >= PACK_AT {
            self.wake();
        }
        Ok(())
    }

    pub(crate) fn delete(&self, open: &OpenIndex, id: String) -> Result<bool> {
        let index = open.id;
        self.file.write(id.len(), move |tx| write::delete(tx, index, &id))
    }

    /// The value of the document `id`, as JSON.
    pub(crate) fn get(&self, open: &OpenIndex, id: &str) -> Result<Option<Vec<u8>>> {
        self.file.read(|connection| {
            let failed = |error| sql_error("read a document", error);
            let mut select = connection.prepare_cached(READ_VALUE).map_err(failed)?;
            select.query_row((open.id, id), |row| row.get(0)).optional().map_err(failed)
        })
    }

    pub(crate) fn search(&self, open: &OpenIndex, query: Query) -> Result<Answer> {
        let settings = Arc::clone(&open.settings_read());
        let asked = Asked { index: open.id, settings: &settings, kept: &open.kept, memory: &self.memory, query };
        let answer = self.file.read(|connection| read::search(connection, asked))?;
        if answer.behind {
            self.wake();
        }
        Ok(answer)
    }

    fn wake(&self) {
        if let Some(wake) = self.wake.get() {
            wake.wake();
        }
    }

    /// Packs each index's head, purges its documents gone and builds again
    /// its dictionary of words when they are due; `everything` takes each
    /// as due once it has any work, as a store without background work asks.
    pub(crate) fn maintain(&self, everything: bool) -> Result<Maintenance> {
        let _one = lock(&self.maintaining);
        let mut done = Maintenance::default();
        let indexes: Vec<Arc<OpenIndex>> = lock(&self.indexes).values().cloned().collect();
        for open in indexes {
            let within = |error: Error| error.within(format!("index {}", open.name));
            done.packed += self.pack(&open, everything).map_err(within)?;
            done.purged += self.purge(&open, everything).map_err(within)?;
            self.build_dictionary(&open, everything).map_err(within)?;
        }
        Ok(done)
    }

    /// Packs an index's head whole, a step a commit, once it is due: a
    /// thousand documents set since, or a pass that a failure stopped and
    /// no set followed.
    fn pack(&self, open: &OpenIndex, everything: bool) -> Result<u64> {
        let id = open.id;
        let (waiting, held) = self.file.read(|connection| {
            let failed = |error| sql_error("read the head", error);
            let mut select = connection.prepare_cached(READ_WAITING).map_err(failed)?;
            select.query_row([id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))).map_err(failed)
        })?;
        if !held || !everything && waiting > 0 && waiting < PACK_AT as i64 {
            return Ok(0);
        }
        open.sets.store(0, Ordering::Relaxed);
        let mut packed = 0;
        loop {
            let step = self.file.transaction(|tx| pack::step(tx, id, pack::STEP_ROWS))?;
            packed += step.postings;
            if step.done {
                return Ok(packed);
            }
        }
    }

    /// Takes an index's documents gone out of its blocks, a step a commit,
    /// once they are due. A purge stopped by a failure goes on from where it
    /// stood at the next pass; one stopped by a restart begins again.
    fn purge(&self, open: &OpenIndex, everything: bool) -> Result<u64> {
        let id = open.id;
        let mut under_way = lock(&open.purge);
        if under_way.is_none() {
            if !self.file.read(|connection| purge::due(connection, id, everything))? {
                return Ok(0);
            }
            *under_way = Some(self.file.read(|connection| purge::begin(connection, id))?);
        }
        let Some(purge) = under_way.as_mut() else { return Ok(0) };
        loop {
            let at = self.file.transaction(|tx| purge.step(tx, id, purge::STEP_KEYS))?;
            if purge.advance(at) {
                break;
            }
        }
        let purged = purge.documents() as u64;
        *under_way = None;
        Ok(purged)
    }

    /// Builds again the dictionary of an index's words a search built, once
    /// its words changed and it is old enough to.
    fn build_dictionary(&self, open: &OpenIndex, everything: bool) -> Result<()> {
        let Some(kept) = lock(&open.kept.corrector).clone() else { return Ok(()) };
        if !everything && kept.built.elapsed() < DICTIONARY_AGE {
            return Ok(());
        }
        let settings = Arc::clone(&open.settings_read());
        let parts: Vec<Part> = settings.fields.iter().map(|field| Part::written(field.number)).collect();
        let built = self.file.read(|connection| {
            let version = count(connection, READ_WORDS_VERSION, open.id)? as i64;
            if version == kept.version {
                return Ok(None);
            }
            Corrector::build(connection, open.id, version, &parts).map(Some)
        })?;
        if let Some(built) = built {
            *lock(&open.kept.corrector) = Some(Arc::new(built));
        }
        Ok(())
    }
}

#[cfg(test)]
impl Search {
    /// One step of packing an index's head, as maintenance takes them.
    pub(crate) fn pack_step(&self, open: &OpenIndex, most_rows: usize) -> Result<pack::Packed> {
        self.file.transaction(|tx| pack::step(tx, open.id, most_rows))
    }

    /// Purges what an index has gone now, `most_keys` keys a step, calling
    /// `after` once each step commits.
    pub(crate) fn purge_in_steps(&self, open: &OpenIndex, most_keys: usize, mut after: impl FnMut()) -> Result<()> {
        let mut purge = self.file.read(|connection| purge::begin(connection, open.id))?;
        loop {
            let at = self.file.transaction(|tx| purge.step(tx, open.id, most_keys))?;
            after();
            if purge.advance(at) {
                return Ok(());
            }
        }
    }
}

impl OpenIndex {
    fn new(id: i64, name: &str, settings: Settings) -> OpenIndex {
        OpenIndex {
            id,
            name: name.to_owned(),
            settings: RwLock::new(Arc::new(settings)),
            kept: Kept::default(),
            sets: AtomicU64::new(0),
            purge: Mutex::default(),
        }
    }

    pub(crate) fn settings_read(&self) -> RwLockReadGuard<'_, Arc<Settings>> {
        self.settings.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn settings_to_change(&self) -> RwLockWriteGuard<'_, Arc<Settings>> {
        self.settings.write().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn read_index(connection: &Connection, name: &str) -> Result<Option<(i64, Settings)>> {
    let failed = |error| sql_error("read an index", error);
    let mut select = connection.prepare_cached(READ_INDEX).map_err(failed)?;
    let row: Option<(i64, String)> =
        select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)?;
    row.map(|(id, json)| Ok((id, Settings::from_json(&json)?))).transpose()
}

/// A count an index's row or rows give.
fn count(connection: &Connection, sql: &str, index: i64) -> Result<u64> {
    let failed = |error| sql_error("read an index", error);
    let mut select = connection.prepare_cached(sql).map_err(failed)?;
    let count: i64 = select.query_row([index], |row| row.get(0)).map_err(failed)?;
    Ok(count.max(0) as u64)
}

fn check_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first && rest && name.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid(format!("an index's name is [a-z0-9][a-z0-9_-]{{0,63}}, not {name:?}")))
}
