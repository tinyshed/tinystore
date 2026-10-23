//! A table's index: an index kept in a database's file whose documents are
//! a table's rows, by a key column of unique values.
//!
//! ```text
//! a write to the table ─ the index's triggers ─► pending keys ─ settle, before the commit ─► the index
//! a process beside the store ─► pending keys ─ settle, at the next write or within a second ─► the index
//! ```
//!
//! The triggers name only the key, so that a migration may drop any other
//! column, and every change of a row lists its key; a settle reads each row
//! as it is now and sets it again only when what the index reads of it
//! changed, which its digest says, so that an update of a column the index
//! does not read costs a read. The first time, the index reads the table's
//! rows a step at a time in the order of their keys; the triggers keep the
//! rows behind the step, and a step reads each row as it is.

use std::collections::HashSet;

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, ToSql};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::engine::Search;
use super::open::Current;
use super::rebuild::read_current;
use super::settings::{Settings, Table};
use super::write::{self, Incoming, Prepared, execute};
use crate::sqlite::{Left, Settle, Tx, given_sql_error, sql_error};
use crate::{Error, ErrorKind, Result};

const ANY_PENDING: &str = "select exists (select 1 from _tinystore_search_pending)";
const READ_PENDING: &str = "select rowid, name, key from _tinystore_search_pending order by rowid limit 256";
const DROP_PENDING: &str = "delete from _tinystore_search_pending where rowid <= ?1";
const DROP_NAMED_PENDING: &str = "delete from _tinystore_search_pending where name = ?1";
const READ_KEPT: &str = "select value from _tinystore_search_docs where index_id = ?1 and id = ?2";
const READ_TRIGGER: &str = "select sql from sqlite_schema where type = 'trigger' and name = ?1";
const READ_FILL: &str = "select filling, filled_through, documents from _tinystore_search_indexes where id = ?1";
const WRITE_FILL: &str = "update _tinystore_search_indexes set filling = ?2, filled_through = ?3 where id = ?1";
const BEGIN_FILL: &str = "update _tinystore_search_indexes set filling = ?2, filled_through = null where id = ?1";
const READ_DOCS: &str = "select doc, value from _tinystore_search_docs
    where index_id = ?1 and doc > ?2 order by doc limit cast(?3 as integer)";
const READ_ID: &str = "select id from _tinystore_search_docs where index_id = ?1 and doc = ?2";

/// What a table's index reads while it fills, as `filling` keeps it.
const FILLED: i64 = 0;
const READING: i64 = 1;
const READING_AGAIN: i64 = 2;
const SWEEPING: i64 = 3;

/// Keys a settle or a step of filling reads at once.
const STEP: usize = 256;
/// The bytes of a row's digest that a document keeps before its key.
const DIGEST: usize = 16;
/// The largest a row is as the index reads it, as a document's JSON.
const MOST_ROW: usize = 1 << 20;

/// What the triggers a table's index puts on its table are named, each
/// beside the statement that makes it as SQLite keeps it: `CREATE TRIGGER`
/// in capitals, so that one already there compares equal.
fn triggers(name: &str, table: &Table) -> [(String, String); 3] {
    let (on, key) = (quoted(&table.name), quoted(&table.key));
    let pending = "insert into _tinystore_search_pending (name, key)";
    let changed = format!("select '{name}', new.{key} where new.{key} is not old.{key};");
    let made = |event: &str, body: String| {
        let trigger = trigger_of(name, event);
        let sql = format!("CREATE TRIGGER {} after {event} on {on} begin {body} end", quoted(&trigger));
        (trigger, sql)
    };
    [
        made("insert", format!("{pending} values ('{name}', new.{key});")),
        made("update", format!("{pending} values ('{name}', old.{key}); {pending} {changed}")),
        made("delete", format!("{pending} values ('{name}', old.{key});")),
    ]
}

use crate::sqlite::quote_ident as quoted;

/// Puts the triggers of the index `name` on its table, made again where
/// they read otherwise; says whether one was missing, as when a migration
/// rebuilt the table by SQLite's twelve steps and they went with the old.
pub(crate) fn follow(tx: &Tx<'_>, name: &str, table: &Table) -> Result<bool> {
    let failed = |error| sql_error(format!("follow table {}", table.name), error);
    let mut missing = false;
    for (trigger, sql) in triggers(name, table) {
        let kept: Option<String> = tx
            .prepare_cached(READ_TRIGGER)
            .and_then(|mut select| select.query_row([&trigger], |row| row.get(0)).optional())
            .map_err(failed)?;
        if kept.as_deref() == Some(sql.as_str()) {
            continue;
        }
        missing |= kept.is_none();
        let drop = format!("drop trigger if exists {}", quoted(&trigger));
        tx.execute_batch(&drop).and_then(|()| tx.execute_batch(&sql)).map_err(failed)?;
    }
    Ok(missing)
}

/// The trigger of the index `name` that lists a row on `event`.
fn trigger_of(name: &str, event: &str) -> String {
    format!("_tinystore_search_{name}_{event}")
}

/// Takes the triggers of the index `name` off its table, and its pending
/// keys with them.
pub(crate) fn unfollow(tx: &Tx<'_>, name: &str) -> Result<()> {
    let failed = |error| sql_error("stop following a table", error);
    for event in ["insert", "update", "delete"] {
        let drop = format!("drop trigger if exists {}", quoted(&trigger_of(name, event)));
        tx.execute_batch(&drop).map_err(failed)?;
    }
    execute(tx, DROP_NAMED_PENDING, [name], "drop an index's pending keys").map(drop)
}

/// Has a new table's index read its table's rows the first time.
pub(crate) fn begin_fill(tx: &Tx<'_>, index: i64) -> Result<()> {
    execute(tx, BEGIN_FILL, [index, READING], "begin reading a table").map(drop)
}

/// Has a table's index whose triggers were missing read its table's rows
/// again, then delete the documents whose rows are gone: what a migration
/// changed meanwhile left no key behind.
pub(crate) fn begin_again(tx: &Tx<'_>, index: i64) -> Result<()> {
    execute(tx, BEGIN_FILL, [index, READING_AGAIN], "begin reading a table again").map(drop)
}

/// Whether any key waits for its index.
pub(crate) fn any_pending(connection: &Connection) -> Result<bool> {
    let failed = |error| sql_error("read the pending keys", error);
    let mut select = connection.prepare_cached(ANY_PENDING).map_err(failed)?;
    select.query_row([], |row| row.get(0)).map_err(failed)
}

/// The share of its table's rows a table's index holds while it fills, and
/// none once it is filled.
pub(crate) fn filling(connection: &Connection, index: i64, settings: &Settings) -> Result<Option<f64>> {
    let Some(table) = &settings.table else { return Ok(None) };
    let failed = |error| sql_error(format!("read table {}", table.name), error);
    let mut select = connection.prepare_cached(READ_FILL).map_err(failed)?;
    let (filling, documents): (bool, i64) =
        select.query_row([index], |row| Ok((row.get(0)?, row.get(2)?))).map_err(failed)?;
    if !filling {
        return Ok(None);
    }
    let count = format!("select count(*) from {}", quoted(&table.name));
    let rows: i64 = connection.query_row(&count, [], |row| row.get(0)).map_err(failed)?;
    Ok(Some((documents as f64 / rows.max(1) as f64).clamp(0.0, 1.0)))
}

impl Settle for Search {
    fn settle(&self, tx: &Tx<'_>, left: Left) -> Result<()> {
        self.settle_pending(tx, left)
    }
}

impl Search {
    /// Sets or deletes in its index each row whose key waits, then takes the
    /// keys out, a step at a time. A row the index cannot read fails the
    /// write that left it, and one a process beside the store wrote is said
    /// in the log and left out of the index.
    fn settle_pending(&self, tx: &Tx<'_>, left: Left) -> Result<()> {
        if !any_pending(tx)? {
            return Ok(());
        }
        let mut targets = Targets::default();
        loop {
            let pending = read_pending(tx)?;
            let Some(&(last, _, _)) = pending.last() else { return Ok(()) };
            let mut seen = HashSet::new();
            for (_, name, key) in &pending {
                if !seen.insert((name, sql_key(key))) {
                    continue;
                }
                if let Some(current) = targets.of(tx, name)? {
                    let target = Target { name, current };
                    self.read_row(tx, target, key, left).map_err(|error| error.within(format!("index {name}")))?;
                }
            }
            execute(tx, DROP_PENDING, [last], "take out the pending keys")?;
            if pending.len() < STEP {
                return Ok(());
            }
        }
    }

    /// Sets the row of `key` in its index as the table holds it, or deletes
    /// it when the table holds none. A row of another process's that the
    /// index cannot read is said and left out.
    fn read_row(&self, tx: &Tx<'_>, Target { name, current }: Target<'_>, key: &SqlValue, left: Left) -> Result<()> {
        let id = match key_id(key) {
            Ok(id) => id,
            Err(error) if left == Left::Before => return said_left_out(&format!("{key:?}"), &error),
            Err(error) => return Err(error),
        };
        let serving = &current.serving;
        let set = prepared_row(tx, serving.id, &serving.settings, key, &id).and_then(|prepared| match prepared {
            Some(prepared)
                if kept_value(tx, serving.id, &id)?.as_deref() == Some(prepared.value.as_slice())
                    && !read_otherwise_beside(tx, current, key, (&id, &prepared.value))? =>
            {
                Ok(())
            }
            Some(prepared) => {
                write::set(tx, &prepared)?;
                self.table_set(name);
                Ok(())
            }
            None => current.written().try_for_each(|data| write::delete(tx, data.id, &id).map(drop)),
        });
        match set {
            Err(error) if left == Left::Before && matches!(error.kind(), ErrorKind::Invalid | ErrorKind::Limit) => {
                current.written().try_for_each(|data| write::delete(tx, data.id, &id).map(drop))?;
                said_left_out(&id, &error)
            }
            set => set.map_err(|error| error.within(format!("row {id}"))),
        }
    }

    /// A step of filling a table's index: its table's rows read in the order
    /// of their keys, then, when it reads them again, its documents in the
    /// order of their numbers; says whether it is filled.
    pub(crate) fn fill_step(&self, tx: &Tx<'_>, name: &str) -> Result<bool> {
        let Some(current) = read_current(tx, name)? else { return Ok(true) };
        let index = current.serving.id;
        if current.serving.settings.table.is_none() {
            return Ok(true);
        }
        let (filling, through): (i64, SqlValue) = tx
            .prepare_cached(READ_FILL)
            .and_then(|mut select| select.query_row([index], |row| Ok((row.get(0)?, row.get(1)?))))
            .map_err(|error| sql_error("read how far a table was read", error))?;
        let (filling, through) = match filling {
            FILLED => return Ok(true),
            SWEEPING => match self.sweep_step(tx, &current, &through)? {
                Some(doc) => (SWEEPING, doc),
                None => (FILLED, SqlValue::Null),
            },
            filling => self.read_step(tx, Target { name, current: &current }, (filling, through))?,
        };
        execute(tx, WRITE_FILL, (index, filling, through), "write how far a table was read")?;
        Ok(filling == FILLED)
    }

    /// Reads the table's next rows into the index; says what it reads next
    /// and from where.
    fn read_step(
        &self,
        tx: &Tx<'_>,
        target: Target<'_>,
        (filling, through): (i64, SqlValue),
    ) -> Result<(i64, SqlValue)> {
        let Some(table) = &target.current.serving.settings.table else { return Ok((FILLED, through)) };
        let keys = next_keys(tx, table, &through)?;
        for key in &keys {
            self.read_row(tx, target, key, Left::Before)
                .map_err(|error| error.within(format!("index {}", target.name)))?;
        }
        Ok(match (keys.len() < STEP, filling) {
            (false, _) => (filling, keys.last().cloned().unwrap_or(through)),
            (true, READING_AGAIN) => (SWEEPING, SqlValue::Null),
            (true, _) => (FILLED, SqlValue::Null),
        })
    }

    /// Deletes the next documents whose rows the table holds no longer; says
    /// the last number it read, or none once it read every one.
    fn sweep_step(&self, tx: &Tx<'_>, current: &Current, through: &SqlValue) -> Result<Option<SqlValue>> {
        let serving = &current.serving;
        let after = match through {
            SqlValue::Integer(doc) => *doc,
            _ => 0,
        };
        let failed = |error| sql_error("read an index's documents", error);
        let docs: Vec<(i64, Vec<u8>)> = tx
            .prepare_cached(READ_DOCS)
            .and_then(|mut select| {
                let rows = select.query_map((serving.id, after, STEP as i64), |row| Ok((row.get(0)?, row.get(1)?)));
                rows.and_then(Iterator::collect)
            })
            .map_err(failed)?;
        for (doc, value) in &docs {
            let id: String = tx
                .prepare_cached(READ_ID)
                .and_then(|mut select| select.query_row((serving.id, doc), |row| row.get(0)))
                .map_err(failed)?;
            if prepared_again(tx, serving.id, &serving.settings, (&id, value))?.is_none() {
                current.written().try_for_each(|data| write::delete(tx, data.id, &id).map(drop))?;
            }
        }
        Ok(docs.last().filter(|_| docs.len() == STEP).map(|(doc, _)| SqlValue::Integer(*doc)))
    }
}

/// An index a row is read into: its name and its rows.
#[derive(Clone, Copy)]
struct Target<'a> {
    name: &'a str,
    current: &'a Current,
}

/// The current rows of each index a settle meets, read once a settle; none
/// for an index dropped.
#[derive(Default)]
struct Targets(std::collections::HashMap<String, Option<Current>>);

impl Targets {
    fn of(&mut self, tx: &Tx<'_>, name: &str) -> Result<Option<&Current>> {
        if !self.0.contains_key(name) {
            let current = read_current(tx, name)?;
            self.0.insert(name.to_owned(), current);
        }
        Ok(self.0.get(name).and_then(Option::as_ref))
    }
}

fn read_pending(tx: &Tx<'_>) -> Result<Vec<(i64, String, SqlValue)>> {
    let failed = |error| sql_error("read the pending keys", error);
    let mut select = tx.prepare_cached(READ_PENDING).map_err(failed)?;
    let rows = select.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)));
    rows.and_then(Iterator::collect).map_err(failed)
}

/// The keys after `through` in the table's order, a step of them.
fn next_keys(tx: &Tx<'_>, table: &Table, through: &SqlValue) -> Result<Vec<SqlValue>> {
    let failed = |error| sql_error(format!("read table {}", table.name), error);
    let (on, key) = (quoted(&table.name), quoted(&table.key));
    // A table's statements are written from its names, and kept by their text.
    let first = matches!(through, SqlValue::Null);
    let after = if first { String::new() } else { format!("where {key} > ?1") };
    let sql = format!("select {key} from {on} {after} order by {key} limit {STEP}");
    let mut select = tx.prepare_cached(&sql).map_err(failed)?;
    let params: &[&dyn ToSql] = if first { &[] } else { &[through] };
    let rows = select.query_map(params, |row| row.get(0));
    rows.and_then(Iterator::collect).map_err(failed)
}

/// The id a document of the row keyed `key` has: its key as text.
fn key_id(key: &SqlValue) -> Result<String> {
    match key {
        SqlValue::Integer(number) => Ok(number.to_string()),
        SqlValue::Text(text) if !text.is_empty() => Ok(text.clone()),
        SqlValue::Null => Err(Error::invalid("a row without a key, which a table's index finds rows by")),
        other => Err(Error::invalid(format!("a key of {other:?}: a table's index keys rows by integers or text"))),
    }
}

/// A key as a set tells two apart.
fn sql_key(key: &SqlValue) -> (u8, String) {
    match key {
        SqlValue::Integer(number) => (0, number.to_string()),
        SqlValue::Text(text) => (1, text.clone()),
        other => (2, format!("{other:?}")),
    }
}

fn said_left_out(row: &str, error: &Error) -> Result<()> {
    tracing::warn!(target: "tinystore", "search: row {row}, written beside the store, left out of its index: {error}");
    Ok(())
}

/// The value a document of the index keeps, as `kept_value` reads it.
fn kept_value(tx: &Tx<'_>, index: i64, id: &str) -> Result<Option<Vec<u8>>> {
    let failed = |error| sql_error("read a document", error);
    let mut select = tx.prepare_cached(READ_KEPT).map_err(failed)?;
    select.query_row((index, id), |row| row.get(0)).optional().map_err(failed)
}

/// Whether a rebuild under way reads the row keyed `key` otherwise than its
/// rows hold it, where the serving rows hold it as `serving` reads: a change
/// of a column only the new settings read. The serving rows then set it
/// again, so that its new number brings the rebuild's copy back to it.
fn read_otherwise_beside(tx: &Tx<'_>, current: &Current, key: &SqlValue, (id, serving): (&str, &[u8])) -> Result<bool> {
    let Some(beside) = current.beside.as_ref().filter(|beside| !beside.failed) else { return Ok(false) };
    let read = match read(tx, &beside.data.settings, key) {
        Ok(Some(row)) => [row.digest.as_slice(), &key_json(key)].concat(),
        Ok(None) => return Ok(false),
        // What the new settings cannot read fails the rebuild at its copy, not this write.
        Err(error) if matches!(error.kind(), ErrorKind::Invalid | ErrorKind::Limit) => return Ok(true),
        Err(error) => return Err(error),
    };
    // Settings that read the same columns read the row as the serving rows do.
    if read == serving {
        return Ok(false);
    }
    Ok(kept_value(tx, beside.data.id, id)?.as_deref() != Some(read.as_slice()))
}

/// The row keyed `key` as the index `index` sets it, or none when the table
/// holds no such row.
fn prepared_row(
    connection: &Connection,
    index: i64,
    settings: &Settings,
    key: &SqlValue,
    id: &str,
) -> Result<Option<Prepared>> {
    let Some(row) = read(connection, settings, key)? else { return Ok(None) };
    let value = [row.digest.as_slice(), &key_json(key)].concat();
    let incoming = Incoming { id: id.to_owned(), tree: row.tree, bytes: value, vector: Some(row.vector.as_deref()) };
    Prepared::new(index, incoming, settings).map(Some)
}

/// The row a document of a table's index stands for, prepared again by
/// `settings`, as a rebuild copies it: none once the table holds it no
/// longer, a delete then reaching every row of the index.
pub(crate) fn prepared_again(
    connection: &Connection,
    index: i64,
    settings: &Settings,
    (id, value): (&str, &[u8]),
) -> Result<Option<Prepared>> {
    let key =
        row_key(value).ok_or_else(|| Error::corrupt(format!("document {id:?}: a row's key that does not read")))?;
    prepared_row(connection, index, settings, &key, id)
}

/// The statement that reads a hit's row whole, and the key it binds, from
/// the value its document keeps.
#[cfg(any(feature = "sql", test))]
pub(crate) fn hit_row(table: &Table, value: &[u8]) -> Result<(String, SqlValue)> {
    let key = row_key(value).ok_or_else(|| Error::corrupt("a hit's row key that does not read"))?;
    Ok((format!("select * from {} where {} = ?", quoted(&table.name), quoted(&table.key)), key))
}

/// The row's key the value of a table's document keeps after its digest.
fn row_key(value: &[u8]) -> Option<SqlValue> {
    let json = serde_json::from_slice::<Value>(value.get(DIGEST..)?).ok()?;
    sql_value(&json)
}

fn key_json(key: &SqlValue) -> Vec<u8> {
    match key {
        SqlValue::Integer(number) => number.to_string().into_bytes(),
        SqlValue::Text(text) => Value::String(text.clone()).to_string().into_bytes(),
        _ => Vec::new(),
    }
}

fn sql_value(json: &Value) -> Option<SqlValue> {
    match json {
        Value::Number(number) => number.as_i64().map(SqlValue::Integer),
        Value::String(text) => Some(SqlValue::Text(text.clone())),
        _ => None,
    }
}

/// A row as the index reads it: its columns read as a document, its
/// vector, and the digest of both.
struct Row {
    tree: Value,
    vector: Option<Vec<f32>>,
    digest: [u8; DIGEST],
}

/// The columns of the row keyed `key` the index reads, or none. The row is
/// read whole and its columns taken by name, so that a column a migration
/// dropped reads as empty until the index is opened without it.
fn read(connection: &Connection, settings: &Settings, key: &SqlValue) -> Result<Option<Row>> {
    let Some(table) = &settings.table else { return Err(Error::internal("a table's row read for no table")) };
    let mut columns: Vec<&str> = settings.fields.iter().map(|field| field.path.as_str()).collect();
    columns.extend(settings.filters.iter().map(|filter| filter.path.as_str()));
    columns.extend(table.vector.as_deref());
    let mut seen = HashSet::new();
    columns.retain(|column| seen.insert(*column));
    let sql = format!("select * from {} where {} = ?1", quoted(&table.name), quoted(&table.key));
    // A table or a key column gone is the program's schema, not the store's
    // failure: `invalid`, which leaves out a row a process beside the store
    // left, rather than failing every later write of the file.
    let failed = |error| given_sql_error(format!("read table {}", table.name), error);
    let mut select = connection.prepare_cached(&sql).map_err(failed)?;
    // A kept statement compiled before a migration changed the table is
    // compiled again at its step, so its columns are named only after it.
    let row = select.query_row([key], |row| {
        let stepped = row.as_ref();
        let names = (0..stepped.column_count()).map(|at| stepped.column_name(at).map(str::to_owned));
        let names: Vec<String> = names.collect::<rusqlite::Result<_>>()?;
        let values: Vec<SqlValue> = (0..names.len()).map(|at| row.get(at)).collect::<rusqlite::Result<_>>()?;
        Ok((names, values))
    });
    let Some((names, mut row)) = row.optional().map_err(failed)? else { return Ok(None) };
    let values = columns.iter().map(|column| match names.iter().position(|name| name == column) {
        Some(at) => std::mem::replace(&mut row[at], SqlValue::Null),
        None => SqlValue::Null,
    });
    row_of(&columns, values.collect(), table).map(Some)
}

fn row_of(columns: &[&str], values: Vec<SqlValue>, table: &Table) -> Result<Row> {
    let mut tree = Map::new();
    let mut vector = None;
    for (column, value) in columns.iter().zip(values) {
        if table.vector.as_deref() == Some(*column) {
            vector = floats(column, value)?;
            continue;
        }
        tree.insert((*column).to_owned(), json_of(value));
    }
    let tree = Value::Object(tree);
    let bytes = serde_json::to_vec(&tree).map_err(|error| Error::invalid(format!("a row: {error}")))?;
    if bytes.len() > MOST_ROW {
        return Err(Error::limit(format!("a row of {} bytes as the index reads it, past 1 MiB", bytes.len())));
    }
    let mut digest = Sha256::new();
    digest.update(&bytes);
    if let Some(vector) = &vector {
        digest.update([1]);
        vector.iter().for_each(|value| digest.update(value.to_le_bytes()));
    }
    let digest: [u8; DIGEST] = digest.finalize()[..DIGEST].try_into().expect("a digest is longer than its part kept");
    Ok(Row { tree, vector, digest })
}

/// A column's value as a document holds it; a blob holds no words, and is
/// null.
fn json_of(value: SqlValue) -> Value {
    match value {
        SqlValue::Integer(number) => Value::from(number),
        SqlValue::Real(number) => Value::from(number),
        SqlValue::Text(text) => Value::String(text),
        SqlValue::Null | SqlValue::Blob(_) => Value::Null,
    }
}

/// A vector column's floats, little-endian, or none where it is null.
fn floats(column: &str, value: SqlValue) -> Result<Option<Vec<f32>>> {
    match value {
        SqlValue::Null => Ok(None),
        SqlValue::Blob(bytes) if bytes.len() % 4 == 0 => {
            Ok(Some(bytes.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect()))
        }
        SqlValue::Blob(bytes) => {
            Err(Error::invalid(format!("column {column} holds {} bytes, not floats of four bytes each", bytes.len())))
        }
        _ => Err(Error::invalid(format!("column {column} holds no blob: a vector is a blob of little-endian floats"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_triggers_statement_starts_as_sqlite_keeps_it_and_quotes_every_name() {
        let table = Table { name: "my \"table\"".to_owned(), key: "id".to_owned(), vector: None };
        let [(insert, sql), _, (delete, _)] = triggers("articles_search", &table);
        assert_eq!(insert, "_tinystore_search_articles_search_insert");
        assert_eq!(delete, "_tinystore_search_articles_search_delete");
        assert!(sql.starts_with(
            "CREATE TRIGGER \"_tinystore_search_articles_search_insert\" after insert on \"my \"\"table\"\"\""
        ));
    }

    #[test]
    fn a_key_reads_back_from_the_value_a_document_keeps() {
        let table = Table { name: "t".to_owned(), key: "id".to_owned(), vector: None };
        for key in [SqlValue::Integer(-42), SqlValue::Integer(i64::MAX), SqlValue::Text("a \"b\"".to_owned())] {
            let value = [[7; DIGEST].as_slice(), &key_json(&key)].concat();
            assert_eq!(hit_row(&table, &value).unwrap().1, key);
        }
        assert!(key_id(&SqlValue::Real(1.5)).is_err());
        assert!(key_id(&SqlValue::Null).is_err());
    }
}
