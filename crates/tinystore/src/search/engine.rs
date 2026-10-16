//! The search engine: `search.db` in the store's directory, the indexes it
//! holds, and the work that keeps them fast and their settings as asked:
//! packing each head into blocks, purging the documents gone, building
//! again a dictionary of words that fell behind, keeping the clusters of
//! vectors, reading an index again beside itself and dropping what is
//! dropped. A table's indexes keep the same tables in a database's file,
//! beside the program's own, in an engine of their own.
//!
//! ```text
//! set ──► head, a grouped commit ── maintenance ──► blocks, a step a commit
//! search ◄── blocks and head, one snapshot
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};

use super::kept::lock;
use super::open::{Beside, Current, Data, OpenIndex};
use super::read::{self, Answer, Asked, Query};
use super::rebuild;
use super::settings::{Changed, Settings, Table, Wanted};
use super::table;
use super::upkeep::PACK_AT;
use super::write::{self, Prepared};
use crate::clock::Clock;
use crate::engine::{Claim, Engine, Host, Wake};
use crate::sqlite::{Config, File, Migration, sql_error};
use crate::{Error, Memory, Result, Store};

const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// search's file in the store's directory.
pub(super) const FILE: &str = "search.db";
/// How often maintenance looks for work, unless a set wakes it sooner.
const MAINTENANCE: Duration = Duration::from_secs(10);

const WRITE_SETTINGS: &str = "update _tinystore_search_indexes set settings = ?2 where id = ?1";
const COUNT_DOCUMENTS: &str = "select documents from _tinystore_search_indexes where id = ?1";
const READ_VALUE: &str = "select value from _tinystore_search_docs where index_id = ?1 and id = ?2";

pub(crate) struct Search {
    pub(crate) file: Arc<File>,
    /// What the log calls a file another engine lends: `sql app`.
    pub(super) lent_by: Option<String>,
    memory: Arc<Memory>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) indexes: Mutex<HashMap<String, Arc<OpenIndex>>>,
    /// Held by a pass of maintenance, so that two never pack the same head.
    maintaining: Mutex<()>,
    wake: OnceLock<Wake>,
    /// Rows a settle set since maintenance last packed, and the indexes it
    /// set them in, which maintenance keeps whether or not this process
    /// opened them. A settle runs on the writer, so these are all it holds.
    table_sets: AtomicU64,
    reached: Mutex<HashSet<String>>,
    _claim: Option<Claim>,
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
    /// Closes search.db; a database's file is the database's to close.
    fn close(&self) -> Result<()> {
        if self.lent_by.is_some() {
            return Ok(());
        }
        self.file.close()
    }

    /// Copies search.db; a table's indexes are copied with their database.
    fn snapshot(&self, into: &std::path::Path) -> Result<Vec<crate::engine::Copied>> {
        if self.lent_by.is_some() {
            return Ok(Vec::new());
        }
        Ok(vec![crate::engine::copy_file(&self.file, FILE, "search", into)?])
    }
}

/// The search engines kept in the store's databases' files, which
/// [`maintain`](super::maintain) reaches as it does search.db.
#[derive(Default)]
pub(crate) struct InFiles(Mutex<Vec<Weak<Search>>>);

impl Engine for InFiles {
    fn close(&self) -> Result<()> {
        Ok(())
    }
}

impl InFiles {
    pub(crate) fn of(store: &Store) -> Result<Arc<InFiles>> {
        store.engine(|_| Ok(Arc::new(InFiles::default())))
    }

    #[cfg(feature = "sql")]
    pub(crate) fn keep(&self, search: &Arc<Search>) {
        lock(&self.0).push(Arc::downgrade(search));
    }

    pub(crate) fn engines(&self) -> Vec<Arc<Search>> {
        lock(&self.0).iter().filter_map(Weak::upgrade).collect()
    }
}

impl Search {
    /// The store's search engine, opened by its first index: its file
    /// claimed and migrated, its maintenance on the store's schedule.
    pub(crate) fn of(store: &Store) -> Result<Arc<Search>> {
        store.engine(|store| {
            let claim = store.claim(FILE)?;
            let file = Arc::new(store.open_file(claim.name(), Config::committing(store.durability()))?);
            Search::start(store, file, None, Some(claim))
        })
    }

    /// Makes search's tables in `file` when it has none, and starts its
    /// maintenance on the store's schedule.
    pub(super) fn start(
        store: &Store,
        file: Arc<File>,
        lent_by: Option<String>,
        claim: Option<Claim>,
    ) -> Result<Arc<Search>> {
        file.migrate("search", MIGRATIONS)?;
        let tasks = lent_by.as_ref().map_or_else(|| "search".to_owned(), |owner| format!("search in {owner}"));
        let search = Arc::new(Search {
            file,
            lent_by,
            memory: Arc::clone(store.memory()),
            clock: store.clock(),
            indexes: Mutex::default(),
            maintaining: Mutex::default(),
            wake: OnceLock::new(),
            table_sets: AtomicU64::new(0),
            reached: Mutex::default(),
            _claim: claim,
        });
        let weak = Arc::downgrade(&search);
        let wake = store.every(&format!("{tasks}: maintenance"), MAINTENANCE, move || {
            weak.upgrade().map_or(Ok(()), |search| search.maintain(false).map(drop))
        })?;
        let _ = search.wake.set(wake);
        Ok(search)
    }

    /// Counts a row a settle set in the index `name`, which maintenance
    /// then keeps, and wakes it once enough wait to be packed.
    pub(crate) fn table_set(&self, name: &str) {
        let mut reached = lock(&self.reached);
        if !reached.contains(name) {
            reached.insert(name.to_owned());
        }
        drop(reached);
        if self.table_sets.fetch_add(1, Ordering::Relaxed) + 1 == PACK_AT {
            self.wake();
        }
    }

    /// Opens the index `name`, made the first time, with the settings
    /// `wanted`; an index that holds others is changed to them, and read
    /// again beside itself where they need it, or where `reindex` asks.
    pub(crate) fn open_index(&self, name: &str, wanted: &Wanted, reindex: bool) -> Result<Arc<OpenIndex>> {
        check_name(name)?;
        wanted.check()?;
        let mut indexes = lock(&self.indexes);
        let mut made = false;
        let open = match indexes.get(name) {
            Some(open) => Arc::clone(open),
            None => {
                let current = match self.file.read(|connection| rebuild::read_current(connection, name))? {
                    Some(current) => current,
                    None => {
                        made = true;
                        let settings = Settings::new(wanted);
                        let id = self.file.transaction(|tx| {
                            let id = rebuild::add_index(tx, name, &settings)?;
                            if settings.table.is_some() {
                                table::begin_fill(tx, id)?;
                            }
                            Ok::<i64, Error>(id)
                        })?;
                        self.wake();
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
        if let Some(followed) = &wanted.table {
            self.follow(&open, followed, made)?;
        }
        self.opened(open.id())?;
        Ok(open)
    }

    /// Puts a table's index's triggers on its table; when they were missing
    /// from an index it did not make now, as after a migration rebuilt the
    /// table, it reads the table again, said in the log.
    fn follow(&self, open: &OpenIndex, followed: &Table, made: bool) -> Result<()> {
        let id = open.id();
        let again = self.file.transaction(|tx| {
            let missing = table::follow(tx, &open.name, followed)?;
            if missing && !made {
                table::begin_again(tx, id)?;
            }
            Ok::<bool, Error>(missing && !made)
        })?;
        if again {
            tracing::warn!(target: "tinystore", "search {}: its triggers were missing from table {}, as a migration \
                that rebuilds the table leaves it; reading the table again", open.name, followed.name);
            self.wake();
        }
        Ok(())
    }

    /// The index `name` as it is, made by no one: a read connection's.
    #[cfg(feature = "wire")]
    pub(crate) fn open_existing(&self, name: &str) -> Result<Arc<OpenIndex>> {
        check_name(name)?;
        let mut indexes = lock(&self.indexes);
        if let Some(open) = indexes.get(name) {
            return Ok(Arc::clone(open));
        }
        let Some(current) = self.file.read(|connection| rebuild::read_current(connection, name))? else {
            return Err(Error::invalid("no such index, and a connection that only reads makes none"));
        };
        let open = Arc::new(OpenIndex::new(name, current));
        indexes.insert(name.to_owned(), Arc::clone(&open));
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
        self.search_then(open, query, |_, _| Ok(())).map(|(answer, ())| answer)
    }

    /// Searches, then runs `then` on its answer in the same snapshot, as a
    /// table's index reads its hits' rows.
    pub(crate) fn search_then<R>(
        &self,
        open: &OpenIndex,
        query: Query,
        then: impl FnOnce(&Connection, &Answer) -> Result<R>,
    ) -> Result<(Answer, R)> {
        self.file.read(|connection| self.search_on(connection, open, query, then))
    }

    /// Searches on `connection`, a reader's snapshot or the writer of a
    /// transaction that holds the file, whose writes the search then sees.
    pub(crate) fn search_on<R>(
        &self,
        connection: &Connection,
        open: &OpenIndex,
        query: Query,
        then: impl FnOnce(&Connection, &Answer) -> Result<R>,
    ) -> Result<(Answer, R)> {
        open.refuse_dropped()?;
        let serving = open.current().serving.clone();
        let asked =
            Asked { index: serving.id, settings: &serving.settings, kept: &serving.kept, memory: &self.memory, query };
        let answer = read::search(connection, asked)?;
        let then = then(connection, &answer)?;
        if answer.behind {
            self.wake();
        }
        Ok((answer, then))
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
        self.keep_reached()?;
        self.table_sets.store(0, Ordering::Relaxed);
        let indexes: Vec<Arc<OpenIndex>> = lock(&self.indexes).values().cloned().collect();
        for open in indexes {
            let within = |error: Error| error.within(format!("index {}", open.name));
            self.fill(&open).map_err(within)?;
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

    /// Opens, for maintenance, each table's index a settle set rows in that
    /// this process has not opened.
    fn keep_reached(&self) -> Result<()> {
        let names = std::mem::take(&mut *lock(&self.reached));
        for name in names {
            if lock(&self.indexes).contains_key(&name) {
                continue;
            }
            if let Some(current) = self.file.read(|connection| rebuild::read_current(connection, &name))? {
                let open = Arc::new(OpenIndex::new(&name, current));
                lock(&self.indexes).entry(name).or_insert(open);
            }
        }
        Ok(())
    }

    /// Reads a table's index's rows the first time, a step a commit, and
    /// wakes whoever waits for it once they are read.
    fn fill(&self, open: &OpenIndex) -> Result<()> {
        if open.current().serving.settings.table.is_none() {
            return Ok(());
        }
        let mut stepped = false;
        while !self.file.transaction(|tx| self.fill_step(tx, &open.name))? {
            stepped = true;
        }
        if stepped {
            open.settle();
        }
        Ok(())
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
