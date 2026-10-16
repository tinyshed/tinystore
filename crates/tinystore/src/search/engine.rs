//! The search engine: `search.db` in the store's directory, the indexes it
//! holds, and the work that keeps them fast and their settings as asked:
//! packing each head into blocks, purging the documents gone, building
//! again a dictionary of words that fell behind, keeping the clusters of
//! vectors, reading an index again beside itself and dropping what is
//! dropped.
//!
//! ```text
//! set ──► head, a grouped commit ── maintenance ──► blocks, a step a commit
//! search ◄── blocks and head, one snapshot
//! ```

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use super::kept::lock;
use super::open::{Beside, Current, Data, OpenIndex};
use super::read::{self, Answer, Asked, Query};
use super::rebuild;
use super::settings::{Changed, Settings, Wanted};
use super::upkeep::PACK_AT;
use super::write::{self, Prepared};
use crate::clock::Clock;
use crate::engine::{Claim, Engine, Host, Wake};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::{Error, Memory, Result, Store};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// search's file in the store's directory.
const FILE: &str = "search.db";
/// How often maintenance looks for work, unless a set wakes it sooner.
const MAINTENANCE: Duration = Duration::from_secs(10);

const WRITE_SETTINGS: &str = "update _tinystore_search_indexes set settings = ?2 where id = ?1";
const COUNT_DOCUMENTS: &str = "select documents from _tinystore_search_indexes where id = ?1";
const READ_VALUE: &str = "select value from _tinystore_search_docs where index_id = ?1 and id = ?2";

pub(crate) struct Search {
    pub(crate) file: File,
    memory: Arc<Memory>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) indexes: Mutex<HashMap<String, Arc<OpenIndex>>>,
    /// Held by a pass of maintenance, so that two never pack the same head.
    maintaining: Mutex<()>,
    wake: OnceLock<Wake>,
    _claim: Claim,
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
                clock: store.clock(),
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
    /// `wanted`; an index that holds others is changed to them, and read
    /// again beside itself where they need it, or where `reindex` asks.
    pub(crate) fn open_index(&self, name: &str, wanted: &Wanted, reindex: bool) -> Result<Arc<OpenIndex>> {
        check_name(name)?;
        wanted.check()?;
        let mut indexes = lock(&self.indexes);
        let open = match indexes.get(name) {
            Some(open) => Arc::clone(open),
            None => {
                let current = match self.file.read(|connection| rebuild::read_current(connection, name))? {
                    Some(current) => current,
                    None => {
                        let settings = Settings::new(wanted);
                        let id = self.file.transaction(|tx| rebuild::add_index(tx, name, &settings))?;
                        Current { serving: Data::new(id, settings), beside: None }
                    }
                };
                let open = Arc::new(OpenIndex::new(name, current));
                indexes.insert(name.to_owned(), Arc::clone(&open));
                open
            }
        };
        drop(indexes);
        open.manual.store(wanted.manual, Ordering::Relaxed);
        self.change(&open, wanted, reindex)?;
        self.opened(open.id())?;
        Ok(open)
    }

    /// Changes an index to `wanted`: what applies at once to its serving
    /// rows and to a rebuild's beside them, and a rebuild begun where the
    /// change needs one that none under way gives; each said in the log.
    fn change(&self, open: &OpenIndex, wanted: &Wanted, reindex: bool) -> Result<()> {
        let mut current = open.change();
        let documents = self.file.read(|connection| count(connection, COUNT_DOCUMENTS, current.serving.id))?;
        let changed = current.serving.settings.change(wanted, documents)?;
        let why = rebuild_asked(&changed, wanted, reindex)?;
        let serving = self.changed_data(&current.serving, &changed)?;
        let target = Settings::new(wanted);
        let reads_as_asked = current.beside.as_ref().filter(|b| !b.failed && b.data.settings.reads_as(&target));
        let beside = match (reads_as_asked, why) {
            (Some(beside), _) => Some(self.change_beside(beside, wanted)?),
            (None, Some(why)) => Some(self.begin_beside(open, &current, (target, why))?),
            (None, None) => self.keep_beside(current.beside.as_ref(), wanted)?,
        };
        for line in &changed.said {
            tracing::info!(target: "tinystore", "search {}: {line}", open.name);
        }
        *current = Arc::new(Current { serving, beside });
        Ok(())
    }

    /// Rows read by changed settings, written to their row when they differ.
    fn changed_data(&self, data: &Data, changed: &Changed) -> Result<Data> {
        if changed.settings == *data.settings {
            return Ok(data.clone());
        }
        let (id, json) = (data.id, changed.settings.to_json());
        self.file.write(json.len(), move |tx| write::execute(tx, WRITE_SETTINGS, (id, json), "write settings"))?;
        Ok(data.with(changed.settings.clone()))
    }

    /// A rebuild under way that no change asks again: a failed one kept to
    /// say why, one that can take `wanted` at once changed, and one that
    /// cannot dropped.
    fn keep_beside(&self, beside: Option<&Beside>, wanted: &Wanted) -> Result<Option<Beside>> {
        let Some(beside) = beside else { return Ok(None) };
        if beside.failed {
            return Ok(Some(beside.clone()));
        }
        match self.change_beside(beside, wanted) {
            Ok(changed) => Ok(Some(changed)),
            Err(_) => {
                self.file.transaction(|tx| rebuild::mark_dropping(tx, beside.data.id))?;
                Ok(None)
            }
        }
    }

    /// A rebuild's rows with what of `wanted` applies at once.
    fn change_beside(&self, beside: &Beside, wanted: &Wanted) -> Result<Beside> {
        let documents = self.file.read(|connection| count(connection, COUNT_DOCUMENTS, beside.data.id))?;
        let changed = beside.data.settings.change(wanted, documents)?;
        Ok(Beside { data: self.changed_data(&beside.data, &changed)?, failed: beside.failed })
    }

    /// Sets the documents `documents` makes of the serving rows' settings,
    /// which hold still until they commit, all of them in one commit or
    /// none; a rebuild under way copies them in turn.
    pub(crate) fn set(&self, open: &OpenIndex, documents: impl FnOnce(&Data) -> Result<Vec<Prepared>>) -> Result<()> {
        let current = open.hold();
        open.refuse_dropped()?;
        let prepared = documents(&current.serving)?;
        let count = prepared.len() as u64;
        if count == 0 {
            return Ok(());
        }
        let bytes = prepared.iter().map(Prepared::bytes).sum();
        self.file.write(bytes, move |tx| prepared.iter().try_for_each(|document| write::set(tx, document)))?;
        drop(current);
        let before = open.sets.fetch_add(count, Ordering::Relaxed);
        if before < PACK_AT && before + count >= PACK_AT {
            self.wake();
        }
        Ok(())
    }

    /// Deletes the document `id` from the serving rows and a rebuild's beside
    /// them, in one commit; says whether the serving rows held it.
    pub(crate) fn delete(&self, open: &OpenIndex, id: String) -> Result<bool> {
        let current = open.hold();
        open.refuse_dropped()?;
        let indexes: Vec<i64> = current.written().map(|data| data.id).collect();
        let deleted = self.file.write(id.len(), move |tx| {
            let mut deleted = Vec::with_capacity(indexes.len());
            for index in indexes {
                deleted.push(write::delete(tx, index, &id)?);
            }
            Ok(deleted.first().copied().unwrap_or(false))
        });
        drop(current);
        deleted
    }

    /// The value of the document `id`, as JSON.
    pub(crate) fn get(&self, open: &OpenIndex, id: &str) -> Result<Option<Vec<u8>>> {
        open.refuse_dropped()?;
        let index = open.id();
        self.file.read(|connection| {
            let failed = |error| sql_error("read a document", error);
            let mut select = connection.prepare_cached(READ_VALUE).map_err(failed)?;
            select.query_row((index, id), |row| row.get(0)).optional().map_err(failed)
        })
    }

    pub(crate) fn search(&self, open: &OpenIndex, query: Query) -> Result<Answer> {
        open.refuse_dropped()?;
        let serving = open.current().serving.clone();
        let asked =
            Asked { index: serving.id, settings: &serving.settings, kept: &serving.kept, memory: &self.memory, query };
        let answer = self.file.read(|connection| read::search(connection, asked))?;
        if answer.behind {
            self.wake();
        }
        Ok(answer)
    }

    pub(crate) fn wake(&self) {
        if let Some(wake) = self.wake.get() {
            wake.wake();
        }
    }

    /// Packs each index's heads, purges their documents gone, builds again
    /// their dictionaries of words and keeps their clusters when they are
    /// due, for its serving rows and a rebuild's beside them; goes on with
    /// each rebuild, and drops what is dropped. `everything` takes each as
    /// due once it has any work, as a store without background work asks.
    pub(crate) fn maintain(&self, everything: bool) -> Result<Maintenance> {
        let _one = lock(&self.maintaining);
        let mut done = Maintenance::default();
        let indexes: Vec<Arc<OpenIndex>> = lock(&self.indexes).values().cloned().collect();
        for open in indexes {
            let within = |error: Error| error.within(format!("index {}", open.name));
            let current = open.current();
            for data in current.written() {
                done.packed += self.pack(data, everything).map_err(within)?;
                if data.id == current.serving.id {
                    open.sets.store(0, Ordering::Relaxed);
                }
                done.purged += self.purge(data, everything).map_err(within)?;
                self.build_dictionary(data, everything).map_err(within)?;
                self.cluster(&open, data, everything).map_err(within)?;
            }
            self.rebuild(&open).map_err(within)?;
        }
        self.drop_rows()?;
        Ok(done)
    }
}

/// Why a change needs an index read again, if it does: a change that needs
/// it to serve what was asked, a field or a filter removed whose keys it
/// drops, or `reindex`'s asking. An index rebuilt by hand refuses the first
/// until `reindex`, and leaves the second for its next rebuild.
fn rebuild_asked(changed: &Changed, wanted: &Wanted, reindex: bool) -> Result<Option<String>> {
    match &changed.rebuild {
        Some(asked) if wanted.manual && !reindex && asked.needed => Err(Error::invalid(format!(
            "{}: the index needs reading again, which an index rebuilt by hand waits for reindex to ask",
            asked.why
        ))),
        Some(_) if wanted.manual && !reindex => Ok(None),
        Some(asked) => Ok(Some(asked.why.clone())),
        None if reindex => Ok(Some("read again as reindex asked".to_owned())),
        None => Ok(None),
    }
}

#[cfg(test)]
impl Search {
    /// Reads search.db in a snapshot, as a search does.
    pub(crate) fn file_read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.file.read(read)
    }

    /// Writes search.db in a transaction of its own.
    pub(crate) fn file_write<T>(&self, write: impl FnOnce(&crate::sqlite::Tx<'_>) -> Result<T>) -> Result<T> {
        self.file.transaction(write)
    }

    /// One step of packing an index's head, as maintenance takes them.
    pub(crate) fn pack_step(&self, open: &OpenIndex, most_rows: usize) -> Result<super::pack::Packed> {
        let id = open.id();
        self.file.transaction(|tx| super::pack::step(tx, id, most_rows))
    }

    /// Purges what an index has gone now, `most_keys` keys a step, calling
    /// `after` once each step commits.
    pub(crate) fn purge_in_steps(&self, open: &OpenIndex, most_keys: usize, mut after: impl FnMut()) -> Result<()> {
        let id = open.id();
        let mut purge = self.file.read(|connection| super::purge::begin(connection, id))?;
        loop {
            let at = self.file.transaction(|tx| purge.step(tx, id, most_keys))?;
            after();
            if purge.advance(at) {
                return Ok(());
            }
        }
    }
}

/// A count an index's row or rows give.
pub(crate) fn count(connection: &Connection, sql: &str, index: i64) -> Result<u64> {
    let failed = |error| sql_error("read an index", error);
    let mut select = connection.prepare_cached(sql).map_err(failed)?;
    let count: i64 = select.query_row([index], |row| row.get(0)).map_err(failed)?;
    Ok(count.max(0) as u64)
}

pub(crate) fn check_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first && rest && name.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid(format!("an index's name is [a-z0-9][a-z0-9_-]{{0,63}}, not {name:?}")))
}
