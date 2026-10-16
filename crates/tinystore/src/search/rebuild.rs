//! An index read again beside itself: its rows, made by settings that need
//! every document read again, while the serving rows answer searches and
//! take writes; once the new rows have read every document, they switch
//! in, in one commit, and the old ones are dropped a step a commit.
//!
//! - **Copying** reads the serving rows' documents in the order of their
//!   numbers, 256 a step, worked out in a snapshot, and writes those still
//!   there; a document set meanwhile has a later number, which the copy
//!   reaches in turn, and a delete reaches both rows in its own commit. The
//!   last few are copied in the commit that switches, no write running
//!   meanwhile, so that the new rows hold every document the old ones do.
//! - **Numbers** start again from 1 in the new rows, in the order the old
//!   ones gave, so that of equal scores the document set first still comes
//!   first, and a rebuild of the same settings is how an index is numbered
//!   again, once the numbers it gave pass twice the documents it holds.
//! - **A restart** finds the new rows by `beside_of` and goes on from
//!   `copied_through`.
//! - **A rebuild that fails**, a document the new settings cannot read, says
//!   why in `failed`; the serving rows answer as before, and the next change
//!   or `reindex` begins again.

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::engine::{Search, check_name};
use super::open::{Beside, Current, Data, OpenIndex, Status};
use super::settings::{Settings, Wanted};
use super::table;
use super::vectors;
use super::write::{self, Incoming, Prepared, execute};
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

const READ_SERVING: &str = "select id, settings from _tinystore_search_indexes
    where name = ?1 and dropping = 0 and beside_of is null";
const READ_BESIDE: &str = "select id, settings, failed from _tinystore_search_indexes
    where beside_of = ?1 and dropping = 0";
const ADD_INDEX: &str = "insert into _tinystore_search_indexes (name, settings) values (?1, ?2) returning id";
const ADD_BESIDE: &str = "insert into _tinystore_search_indexes (name, settings, beside_of, why)
    values ('~' || ?1, ?2, ?1, ?3) returning id";
const MARK_DROPPING: &str =
    "update _tinystore_search_indexes set dropping = 1, beside_of = null, name = '~drop:' || id where id = ?1";
const MARK_NAMED_DROPPING: &str = "update _tinystore_search_indexes set dropping = 1, beside_of = null,
    name = '~drop:' || id where name = ?1 or beside_of = (select id from _tinystore_search_indexes where name = ?1)";
const READ_PROGRESS: &str = "select (select next_doc from _tinystore_search_indexes where id = ?2),
    copied_through, failed, why from _tinystore_search_indexes where id = ?1";
const WRITE_COPIED: &str = "update _tinystore_search_indexes set copied_through = ?2 where id = ?1";
const WRITE_FAILED: &str = "update _tinystore_search_indexes set failed = ?2 where id = ?1";
const SWITCH: &str = "update _tinystore_search_indexes set name = ?2, beside_of = null, why = null,
    filling = (select filling from _tinystore_search_indexes where id = ?3),
    filled_through = (select filled_through from _tinystore_search_indexes where id = ?3) where id = ?1";
const READ_DOCS: &str = "select doc, id, value from _tinystore_search_docs
    where index_id = ?1 and doc > ?2 order by doc limit ?3";
const STILL_THERE: &str = "select exists (select 1 from _tinystore_search_docs where index_id = ?1 and doc = ?2)";
const BESIDE_LIVE: &str = "select exists (select 1 from _tinystore_search_indexes
    where id = ?1 and beside_of = ?2 and dropping = 0 and failed is null)";
const READ_NUMBERS: &str = "select documents, next_doc from _tinystore_search_indexes where id = ?1";
const READ_DROPPING: &str = "select id from _tinystore_search_indexes where dropping = 1";
const DROP_ROW: &str = "delete from _tinystore_search_indexes where id = ?1";
const WRITE_OPENED: &str = "update _tinystore_search_indexes set opened_at = ?2 where id = ?1";
const LIST: &str = "select id, name, documents, vectors, opened_at from _tinystore_search_indexes
    where dropping = 0 and beside_of is null order by name";

/// What a step of dropping deletes, each a few rows of one table: a key's
/// blocks go before its list and the key, so that no block outlives the key
/// that names it.
const DROP_STEPS: &[&str] = &[
    "delete from _tinystore_search_blocks where key in
        (select id from _tinystore_search_keys where index_id = ?1 order by id limit 64)",
    "delete from _tinystore_search_lists where key in
        (select id from _tinystore_search_keys where index_id = ?1 order by id limit 64)",
    "delete from _tinystore_search_keys where id in
        (select id from _tinystore_search_keys where index_id = ?1 order by id limit 64)",
    "delete from _tinystore_search_head where (index_id, key, doc) in
        (select index_id, key, doc from _tinystore_search_head where index_id = ?1 limit 4096)",
    "delete from _tinystore_search_docs where (index_id, doc) in
        (select index_id, doc from _tinystore_search_docs where index_id = ?1 limit 512)",
    "delete from _tinystore_search_norms where (index_id, part, chunk) in
        (select index_id, part, chunk from _tinystore_search_norms where index_id = ?1 limit 64)",
    "delete from _tinystore_search_gone where (index_id, doc) in
        (select index_id, doc from _tinystore_search_gone where index_id = ?1 limit 4096)",
    "delete from _tinystore_search_parts where index_id = ?1",
    "delete from _tinystore_search_vectors where key in
        (select key from _tinystore_search_vectors where key >= ?2 and key < ?3 limit 512)",
    "delete from _tinystore_search_codes where key in
        (select key from _tinystore_search_codes where key >= ?2 and key < ?3 limit 2048)",
    "delete from _tinystore_search_members where (index_id, cluster, doc) in
        (select index_id, cluster, doc from _tinystore_search_members where index_id = ?1 limit 4096)",
    "delete from _tinystore_search_clusters where index_id = ?1",
];

/// Documents a step of copying reads.
const COPY_STEP: i64 = 256;
/// Numbers an index gives before it may be numbered again, and the number
/// past which it is in any case, long before 2³² ends them.
const RENUMBER_FROM: i64 = if cfg!(test) { 64 } else { 4096 };
const RENUMBER_ANYWAY: i64 = 1 << 31;

/// How far a rebuild has read, of the numbers the serving rows gave.
#[derive(Debug)]
pub(crate) struct Progress {
    next_doc: i64,
    copied_through: i64,
    pub(crate) failed: Option<String>,
    why: Option<String>,
}

impl Progress {
    pub(crate) fn share(&self) -> f64 {
        (self.copied_through as f64 / (self.next_doc - 1).max(1) as f64).clamp(0.0, 1.0)
    }
}

/// An index open in no process yet, as search.db holds it: its serving
/// rows, and the rows a rebuild makes beside them.
pub(crate) fn read_current(connection: &Connection, name: &str) -> Result<Option<Current>> {
    let failed = |error| sql_error("read an index", error);
    let mut select = connection.prepare_cached(READ_SERVING).map_err(failed)?;
    let row: Option<(i64, String)> =
        select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)?;
    let Some((id, json)) = row else { return Ok(None) };
    let mut select = connection.prepare_cached(READ_BESIDE).map_err(failed)?;
    let beside: Option<(i64, String, Option<String>)> =
        select.query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(failed)?;
    let beside = beside
        .map(|(id, json, failed)| {
            Ok(Beside { data: Data::new(id, Settings::from_json(&json)?), failed: failed.is_some() })
        })
        .transpose()?;
    Ok(Some(Current { serving: Data::new(id, Settings::from_json(&json)?), beside }))
}

pub(crate) fn progress(connection: &Connection, (serving, beside): (i64, i64)) -> Result<Progress> {
    let failed = |error| sql_error("read a rebuild", error);
    let mut select = connection.prepare_cached(READ_PROGRESS).map_err(failed)?;
    let read = select.query_row([beside, serving], |row| {
        Ok(Progress { next_doc: row.get(0)?, copied_through: row.get(1)?, failed: row.get(2)?, why: row.get(3)? })
    });
    read.map_err(failed)
}

/// A step of copying, worked out in a snapshot: the next documents of the
/// serving rows, each as the new settings read it, how far it reaches, and
/// whether it read as many as a step reads.
#[derive(Debug)]
pub(crate) struct Copy {
    documents: Vec<(u32, Prepared)>,
    through: i64,
    full: bool,
}

/// The next documents a rebuild reads, each prepared for the rows beside
/// the serving ones; a document they cannot read fails the rebuild, named.
fn copied(connection: &Connection, serving: i64, beside: &Data, progress: &Progress) -> Result<Copy> {
    let failed = |error| sql_error("read documents to copy", error);
    let mut select = connection.prepare_cached(READ_DOCS).map_err(failed)?;
    let range = (serving, progress.copied_through, COPY_STEP);
    let rows = select.query_map(range, |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?, row.get(2)?)));
    let rows: Vec<(u32, String, Vec<u8>)> = rows.and_then(Iterator::collect).map_err(failed)?;
    let mut documents = Vec::with_capacity(rows.len());
    for (doc, id, bytes) in &rows {
        if beside.settings.table.is_some() {
            let prepared = table::prepared_again(connection, beside.id, &beside.settings, (id, bytes))
                .map_err(|error| error.within(format!("id {id:?}")))?;
            documents.extend(prepared.map(|prepared| (*doc, prepared)));
            continue;
        }
        let tree: Value = serde_json::from_slice(bytes)
            .map_err(|error| Error::corrupt(format!("document {doc} that does not read as JSON")).with_source(error))?;
        let vector = match beside.settings.vectors {
            Some(_) => Some(vectors::values(connection, serving, *doc)?),
            None => None,
        };
        let incoming =
            Incoming { id: id.clone(), tree, bytes: bytes.clone(), vector: vector.as_ref().map(Option::as_deref) };
        let prepared =
            Prepared::new(beside.id, incoming, &beside.settings).map_err(|error| error.within(format!("id {id:?}")))?;
        documents.push((*doc, prepared));
    }
    let through = rows.last().map_or(progress.copied_through, |(doc, _, _)| i64::from(*doc));
    Ok(Copy { documents, through, full: rows.len() as i64 == COPY_STEP })
}

/// Writes a step of copying: each document still there as it was read, so
/// that no write since is undone. Says whether the rows beside still take it.
fn copy(tx: &Tx<'_>, (serving, beside): (i64, i64), step: &Copy) -> Result<bool> {
    let failed = |error| sql_error("copy documents", error);
    let live = tx
        .prepare_cached(BESIDE_LIVE)
        .and_then(|mut select| select.query_row((beside, serving), |row| row.get::<_, bool>(0)));
    if !live.map_err(failed)? {
        return Ok(false);
    }
    for (doc, prepared) in &step.documents {
        let there = tx
            .prepare_cached(STILL_THERE)
            .and_then(|mut select| select.query_row((serving, doc), |row| row.get::<_, bool>(0)));
        if there.map_err(failed)? {
            write::set(tx, prepared)?;
        }
    }
    execute(tx, WRITE_COPIED, (beside, step.through), "write how far a rebuild read")?;
    Ok(true)
}

impl Search {
    /// Begins reading an index again beside its serving rows, by `target`,
    /// dropping rows a rebuild before left, in one commit.
    pub(crate) fn begin_beside(
        &self,
        open: &OpenIndex,
        current: &Current,
        (target, why): (Settings, String),
    ) -> Result<Beside> {
        let (serving, json, old) = (current.serving.id, target.to_json(), current.beside.as_ref().map(|b| b.data.id));
        let told = why.clone();
        let id = self.file.transaction(|tx| {
            if let Some(old) = old {
                execute(tx, MARK_DROPPING, [old], "drop an index's rows")?;
            }
            let added = tx
                .prepare_cached(ADD_BESIDE)
                .and_then(|mut insert| insert.query_row((serving, &json, &told), |row| row.get(0)));
            added.map_err(|error| sql_error("begin a rebuild", error))
        })?;
        let documents = self.file.read(|connection| numbers(connection, serving))?.0;
        tracing::info!(target: "tinystore", "search {}: {why}: reading {documents} documents beside the index, \
            which answers as before until then", open.name);
        *open.began.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
        self.wake();
        Ok(Beside { data: Data::new(id, target), failed: false })
    }

    /// Goes on with an index's rebuild, a step a commit, and switches its rows
    /// in once a step finds fewer documents than a step reads; without one,
    /// begins numbering the index again once it is due.
    pub(crate) fn rebuild(&self, open: &OpenIndex) -> Result<()> {
        let current = open.current();
        let Some(beside) = current.beside.clone().filter(|beside| !beside.failed) else {
            return self.renumber(open, &current);
        };
        let rows = (current.serving.id, beside.data.id);
        loop {
            let progress = self.file.read(|connection| progress(connection, rows))?;
            if progress.failed.is_some() {
                return Ok(());
            }
            let step = match self.file.read(|connection| copied(connection, rows.0, &beside.data, &progress)) {
                Err(error) if matches!(error.kind(), ErrorKind::Invalid | ErrorKind::Limit) => {
                    return self.fail(open, beside.data.id, &error);
                }
                step => step?,
            };
            if !step.full {
                return self.switch(open, beside.data.id);
            }
            if !self.file.transaction(|tx| copy(tx, rows, &step))? {
                return Ok(());
            }
        }
    }

    /// Switches a rebuild's rows in, no write running meanwhile: the last
    /// documents copied and the rows renamed in one commit, and the old ones
    /// dropped in the background.
    fn switch(&self, open: &OpenIndex, beside: i64) -> Result<()> {
        let mut current = open.change();
        let Some(rebuilt) = current.beside.clone().filter(|b| b.data.id == beside && !b.failed) else { return Ok(()) };
        let (serving, name) = (current.serving.id, open.name.clone());
        let switched: Result<Option<String>> = self.file.transaction(|tx| {
            let why = progress(tx, (serving, beside))?.why;
            loop {
                let step = copied(tx, serving, &rebuilt.data, &progress(tx, (serving, beside))?)?;
                if !copy(tx, (serving, beside), &step)? || !step.full {
                    break;
                }
            }
            execute(tx, MARK_DROPPING, [serving], "drop an index's old rows")?;
            execute(tx, SWITCH, (beside, &name, serving), "switch an index's rows")?;
            Ok(why)
        });
        let why = match switched {
            Err(error) if matches!(error.kind(), ErrorKind::Invalid | ErrorKind::Limit) => {
                drop(current);
                return self.fail(open, beside, &error);
            }
            switched => switched?,
        };
        *current = std::sync::Arc::new(Current { serving: rebuilt.data, beside: None });
        drop(current);
        let began = open.began.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let took = began.map(|began| format!(" in {:.0} s", began.elapsed().as_secs_f64())).unwrap_or_default();
        tracing::info!(target: "tinystore", "search {}: {} ready{took}; switched", open.name, why.unwrap_or_default());
        open.settle();
        Ok(())
    }

    /// Stops a rebuild that cannot read a document, saying why; the serving
    /// rows answer as before.
    fn fail(&self, open: &OpenIndex, beside: i64, error: &Error) -> Result<()> {
        let said = error.to_string();
        let mut current = open.change();
        self.file.transaction(|tx| execute(tx, WRITE_FAILED, (beside, &said), "write why a rebuild failed"))?;
        if let Some(failed) = current.beside.clone().filter(|b| b.data.id == beside) {
            *current = std::sync::Arc::new(Current {
                beside: Some(Beside { failed: true, ..failed }),
                ..Current::clone(&current)
            });
        }
        drop(current);
        let name = &open.name;
        tracing::warn!(target: "tinystore", "search {name}: its rebuild failed: {said}; the index answers as before");
        open.settle();
        Ok(())
    }

    /// Begins numbering an index again once the numbers it gave pass twice
    /// the documents it holds, or in any case past 2³¹; an index rebuilt by
    /// hand waits for `reindex` until then.
    fn renumber(&self, open: &OpenIndex, current: &Current) -> Result<()> {
        if current.beside.is_some() {
            return Ok(());
        }
        let (documents, next) = self.file.read(|connection| numbers(connection, current.serving.id))?;
        let given = next - 1;
        let manual = open.manual.load(std::sync::atomic::Ordering::Relaxed);
        if !(next >= RENUMBER_ANYWAY || !manual && given >= RENUMBER_FROM && given > 2 * documents) {
            return Ok(());
        }
        let mut held = open.change();
        if held.beside.is_some() || held.serving.id != current.serving.id {
            return Ok(());
        }
        let target = Settings::new(&wanted_of(&held.serving.settings));
        let why = format!("numbered again: {given} numbers given for {documents} documents");
        let beside = self.begin_beside(open, &held, (target, why))?;
        *held = std::sync::Arc::new(Current { beside: Some(beside), ..Current::clone(&held) });
        Ok(())
    }

    pub(crate) fn status(&self, open: &OpenIndex) -> Result<Status> {
        open.refuse_dropped()?;
        let current = open.current();
        let Some(beside) = current.beside.clone() else {
            let serving = &current.serving;
            let filling = self.file.read(|connection| table::filling(connection, serving.id, &serving.settings))?;
            return Ok(filling.map_or(Status::Ready, |done| Status::Rebuilding { done }));
        };
        let progress = self.file.read(|connection| progress(connection, (open.id(), beside.data.id)))?;
        Ok(match progress.failed {
            Some(error) => Status::Failed { error },
            None => Status::Rebuilding { done: progress.share() },
        })
    }

    /// Waits until the index serves the settings it was opened with: its
    /// rebuild switched in, or failed, which is the error.
    pub(crate) fn ready(&self, open: &OpenIndex) -> Result<()> {
        loop {
            match self.status(open)? {
                Status::Ready => return Ok(()),
                Status::Failed { error } => return Err(Error::invalid(format!("its rebuild failed: {error}"))),
                Status::Rebuilding { .. } => open.wait_settled(std::time::Duration::from_millis(100)),
            }
        }
    }

    /// Drops the index `name`, its rows and a rebuild's beside them, a step a
    /// commit in the background; says whether there was one. A handle of it
    /// is refused from then on, and opening the name again makes a new one.
    pub(crate) fn drop_index(&self, name: &str) -> Result<bool> {
        check_name(name)?;
        let open = super::kept::lock(&self.indexes).remove(name);
        let _held = open.as_ref().map(|open| {
            let held = open.change();
            open.mark_dropped();
            held
        });
        let named = name.to_owned();
        let dropped = self.file.transaction(|tx| {
            table::unfollow(tx, &named)?;
            let marked = tx.prepare_cached(MARK_NAMED_DROPPING).and_then(|mut update| update.execute([&named]));
            marked.map_err(|error| sql_error("drop an index", error))
        })?;
        if dropped > 0 {
            tracing::info!(target: "tinystore", "search {name}: dropped; its rows go in the background");
            self.wake();
        }
        Ok(dropped > 0)
    }

    /// Drops the rows of every index dropped or switched out, a step a commit.
    pub(crate) fn drop_rows(&self) -> Result<()> {
        let dropping = self.file.read(|connection| {
            let failed = |error| sql_error("read the indexes dropped", error);
            let mut select = connection.prepare_cached(READ_DROPPING).map_err(failed)?;
            let ids = select.query_map([], |row| row.get::<_, i64>(0));
            ids.and_then(Iterator::collect::<rusqlite::Result<Vec<i64>>>).map_err(failed)
        })?;
        for id in dropping {
            while self.file.transaction(|tx| drop_step(tx, id))? {}
        }
        Ok(())
    }

    /// Records that a program opened an index now; a copy's programs open
    /// its home's indexes, and record nothing.
    pub(crate) fn opened(&self, id: i64) -> Result<()> {
        if self.file.is_copy() {
            return Ok(());
        }
        let at = self.clock.now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as i64);
        self.file.write(16, move |tx| execute(tx, WRITE_OPENED, (id, at), "write when an index was opened"))
    }

    /// Every index search.db holds, by name.
    pub(crate) fn list(&self) -> Result<Vec<Listed>> {
        let rows = self.file.read(|connection| {
            let failed = |error| sql_error("list the indexes", error);
            let mut select = connection.prepare_cached(LIST).map_err(failed)?;
            let rows = select.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            });
            rows.and_then(Iterator::collect::<rusqlite::Result<Vec<(i64, String, i64, i64, i64)>>>).map_err(failed)
        })?;
        let mut listed = Vec::with_capacity(rows.len());
        for (id, name, documents, vectors, opened_at) in rows {
            let status = self.file.read(|connection| listed_status(connection, id))?;
            let opened =
                (opened_at > 0).then(|| std::time::UNIX_EPOCH + std::time::Duration::from_millis(opened_at as u64));
            listed.push(Listed {
                name,
                documents: documents.max(0) as u64,
                vectors: vectors.max(0) as u64,
                opened,
                status,
            });
        }
        Ok(listed)
    }
}

#[cfg(test)]
impl Search {
    /// One step of copying an index's rebuild, as maintenance takes them;
    /// says whether there may be more.
    pub(crate) fn copy_step(&self, open: &OpenIndex) -> Result<bool> {
        let current = open.current();
        let Some(beside) = current.beside.clone() else { return Ok(false) };
        let rows = (current.serving.id, beside.data.id);
        let progress = self.file.read(|connection| progress(connection, rows))?;
        let step = self.file.read(|connection| copied(connection, rows.0, &beside.data, &progress))?;
        self.file.transaction(|tx| copy(tx, rows, &step))?;
        Ok(step.full)
    }
}

/// An index as `search::list` says it.
#[derive(Clone, Debug, PartialEq)]
pub struct Listed {
    pub name: String,
    pub documents: u64,
    pub vectors: u64,
    /// When a program last opened it, by the store's clock.
    pub opened: Option<std::time::SystemTime>,
    pub status: Status,
}

fn listed_status(connection: &Connection, serving: i64) -> Result<Status> {
    let failed = |error| sql_error("read a rebuild", error);
    let mut select = connection.prepare_cached(READ_BESIDE).map_err(failed)?;
    let beside: Option<i64> = select.query_row([serving], |row| row.get(0)).optional().map_err(failed)?;
    let Some(beside) = beside else { return Ok(Status::Ready) };
    let progress = progress(connection, (serving, beside))?;
    Ok(match progress.failed {
        Some(error) => Status::Failed { error },
        None => Status::Rebuilding { done: progress.share() },
    })
}

fn numbers(connection: &Connection, index: i64) -> Result<(i64, i64)> {
    let failed = |error| sql_error("read an index's numbers", error);
    let mut select = connection.prepare_cached(READ_NUMBERS).map_err(failed)?;
    select.query_row([index], |row| Ok((row.get(0)?, row.get(1)?))).map_err(failed)
}

/// A step of dropping an index's rows; says whether there may be more.
fn drop_step(tx: &Tx<'_>, index: i64) -> Result<bool> {
    let mut dropped = 0;
    for sql in DROP_STEPS {
        let params: Vec<i64> =
            if sql.contains("?3") { vec![index, index << 32, (index + 1) << 32] } else { vec![index] };
        let ran = tx.prepare_cached(sql).and_then(|mut delete| delete.execute(rusqlite::params_from_iter(params)));
        dropped += ran.map_err(|error| sql_error("drop an index's rows", error))?;
    }
    if dropped == 0 {
        execute(tx, DROP_ROW, [index], "drop an index's row")?;
    }
    Ok(dropped > 0)
}

/// Marks an index's rows to be dropped a step a commit.
pub(crate) fn mark_dropping(tx: &Tx<'_>, index: i64) -> Result<()> {
    execute(tx, MARK_DROPPING, [index], "drop an index's rows")
}

/// Adds the rows of a new index; says its id.
pub(crate) fn add_index(tx: &Tx<'_>, name: &str, settings: &Settings) -> Result<i64> {
    let added = tx
        .prepare_cached(ADD_INDEX)
        .and_then(|mut insert| insert.query_row((name, settings.to_json()), |row| row.get(0)));
    added.map_err(|error| sql_error("write an index", error))
}

/// What an open would ask for to make `settings` again, its fields and
/// filters numbered afresh.
pub(crate) fn wanted_of(settings: &Settings) -> Wanted {
    Wanted {
        fields: settings.fields.iter().map(|field| (field.path.clone(), field.weight)).collect(),
        filters: settings.filters.iter().map(|filter| filter.path.clone()).collect(),
        typo_tolerance: settings.typo_tolerance,
        dimensions: settings.vectors.map(|vectors| vectors.dimensions),
        recall: settings.vectors.and_then(|vectors| vectors.recall),
        vector_weight: settings.vectors.map(|_| settings.hybrid.vector_weight),
        rerank: settings.hybrid.rerank,
        manual: false,
        table: settings.table.clone(),
    }
}
