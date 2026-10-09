//! The rows of kv.db: one statement a step, each a named constant, and no
//! vocabulary above a row's bucket, path, version, expiry and value.

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params};

use super::value::Raw;
use crate::Result;
use crate::sqlite::sql_error;

/// A value over this many bytes lives in a row of its own, so that a page of
/// cells keeps many keys and a scan of keys does not read every big value.
pub(crate) const SPILL: usize = 512;

/// The largest value a key holds.
pub(crate) const MAX_VALUE: usize = 1 << 20;

/// Whether a cell is hidden by a clear marked on a branch above it, by the
/// branch's prefix followed by an owner's or a key's mark.
macro_rules! hidden {
    () => {
        "exists (select 1 from _tinystore_kv_branches as m
            where m.bucket = c.bucket and m.cleared >= c.version
            and substr(c.path, 1, length(m.prefix)) = m.prefix
            and substr(c.path, length(m.prefix) + 1, 1) in (x'01', x'02'))"
    };
}

const SELECT_LIVE: &str = concat!(
    "select c.version, c.expires, c.value, s.value from _tinystore_kv_cells as c
        left join _tinystore_kv_spilled as s on s.id = c.spill
        where c.bucket = ?1 and c.path = ?2 and (c.expires is null or c.expires > ?3) and not ",
    hidden!()
);
/// Whether a cell met by an upsert is gone: expired, or hidden by a clear.
macro_rules! gone {
    () => {
        concat!("(c.expires <= ?6 or ", hidden!(), ")")
    };
}

const SELECT_HAS: &str = concat!(
    "select 1 from _tinystore_kv_cells as c
        where c.bucket = ?1 and c.path = ?2 and (c.expires is null or c.expires > ?3) and not ",
    hidden!()
);
const SELECT_CURRENT: &str = concat!(
    "select c.version, c.expires, c.spill, (c.expires is null or c.expires > ?3) and not ",
    hidden!(),
    " from _tinystore_kv_cells as c where c.bucket = ?1 and c.path = ?2"
);
const SELECT_PAGE: &str = concat!(
    "select c.path, c.version, c.expires, c.value, s.value from _tinystore_kv_cells as c
        left join _tinystore_kv_spilled as s on s.id = c.spill
        where c.bucket = ?1 and c.path > ?2 and c.path < ?3 and (c.expires is null or c.expires > ?4) and not ",
    hidden!(),
    " order by c.path limit cast(?5 as integer)"
);
const UPSERT: &str = "insert into _tinystore_kv_cells as c (bucket, path, version, expires, value, spill)
    values (?1, ?2, ?3, ?4, ?5, ?6)
    on conflict (bucket, path) do update set
        version = excluded.version, expires = excluded.expires, value = excluded.value, spill = excluded.spill";
/// A counter's add in one statement: a counter gone starts again from the
/// number added, with a new expiry, and a live one keeps its own. A sum past
/// the int64 range updates nothing, since SQLite would make it a REAL.
const ADD: &str = concat!(
    "insert into _tinystore_kv_cells as c (bucket, path, version, expires, value) values (?1, ?2, ?3, ?4, ?5)
    on conflict (bucket, path) do update set
        value = iif(",
    gone!(),
    ", excluded.value, c.value + excluded.value),
        expires = iif(",
    gone!(),
    ", excluded.expires, c.expires),
        version = excluded.version
    where ",
    gone!(),
    " or typeof(c.value + excluded.value) = 'integer'
    returning value"
);
const INSERT_SPILLED: &str = "insert into _tinystore_kv_spilled (value) values (?1)";
const DELETE_SPILLED: &str = "delete from _tinystore_kv_spilled where id = ?1";
const DELETE_CELL: &str = "delete from _tinystore_kv_cells where bucket = ?1 and path = ?2";
const UPDATE_EXPIRES: &str = "update _tinystore_kv_cells set expires = ?3 where bucket = ?1 and path = ?2";
const RENEW: &str =
    "update _tinystore_kv_cells set expires = ?5 where bucket = ?1 and path = ?2 and version = ?3 and expires = ?4";
const COUNT_UNDER: &str = "select count(*) from (
    select 1 from _tinystore_kv_cells where bucket = ?1 and path >= ?2 and path < ?3 limit cast(?4 as integer))";
const DELETE_UNDER: &str =
    "delete from _tinystore_kv_cells where bucket = ?1 and path >= ?2 and path < ?3 returning spill";
const MARK_CLEARED: &str = "insert into _tinystore_kv_branches (bucket, prefix, cleared) values (?1, ?2, ?3)
    on conflict (bucket, prefix) do update set cleared = excluded.cleared";
const EXPIRE: &str = "delete from _tinystore_kv_cells where (bucket, path) in (
    select bucket, path from _tinystore_kv_cells where expires <= ?1 order by expires limit cast(?2 as integer)
) returning spill";
const SELECT_MARKS: &str = "select bucket, prefix, cleared from _tinystore_kv_branches";
const DROP_HIDDEN: &str = "delete from _tinystore_kv_cells where (bucket, path) in (
    select bucket, path from _tinystore_kv_cells
    where bucket = ?1 and path >= ?2 and path < ?3 and version <= ?4 limit cast(?5 as integer)
) returning spill";
const UNMARK: &str = "delete from _tinystore_kv_branches where bucket = ?1 and prefix = ?2 and cleared = ?3";
const SELECT_REVISION: &str = "select value from _tinystore_kv_meta where name = 'revision'";
const KEEP_REVISION: &str = "update _tinystore_kv_meta set value = ?1 where name = 'revision' and value < ?1";
const SELECT_BUCKET: &str = "select id, role from _tinystore_kv_buckets where name = ?1";
const INSERT_BUCKET: &str = "insert into _tinystore_kv_buckets (name, role) values (?1, ?2) returning id";

/// A live cell as a read finds it.
#[derive(Debug)]
pub(crate) struct Cell {
    pub(crate) version: i64,
    pub(crate) expires: Option<i64>,
    pub(crate) raw: Raw,
}

/// A cell as a write finds it, live or not.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Current {
    pub(crate) version: i64,
    pub(crate) expires: Option<i64>,
    pub(crate) spill: Option<i64>,
    pub(crate) live: bool,
}

/// A clear marked on a branch too large to delete at once.
#[derive(Clone, Debug)]
pub(crate) struct Mark {
    pub(crate) bucket: i64,
    pub(crate) prefix: Vec<u8>,
    pub(crate) cleared: i64,
}

pub(crate) fn live(connection: &Connection, bucket: i64, path: &[u8], now: i64) -> Result<Option<Cell>> {
    connection
        .prepare_cached(SELECT_LIVE)
        .and_then(|mut select| {
            select
                .query_row(params![bucket, path, now], |row| {
                    Ok(Cell {
                        version: row.get(0)?,
                        expires: row.get(1)?,
                        raw: raw_of(row.get_ref(2)?, row.get_ref(3)?)?,
                    })
                })
                .optional()
        })
        .map_err(|error| sql_error("a read", error))
}

pub(crate) fn has(connection: &Connection, bucket: i64, path: &[u8], now: i64) -> Result<bool> {
    connection
        .prepare_cached(SELECT_HAS)
        .and_then(|mut select| select.exists(params![bucket, path, now]))
        .map_err(|error| sql_error("a read", error))
}

pub(crate) fn current(connection: &Connection, bucket: i64, path: &[u8], now: i64) -> Result<Option<Current>> {
    connection
        .prepare_cached(SELECT_CURRENT)
        .and_then(|mut select| {
            select
                .query_row(params![bucket, path, now], |row| {
                    Ok(Current { version: row.get(0)?, expires: row.get(1)?, spill: row.get(2)?, live: row.get(3)? })
                })
                .optional()
        })
        .map_err(|error| sql_error("a write: the key's cell", error))
}

/// Writes a cell, moving a value over [`SPILL`] bytes to a row of its own and
/// deleting the row the cell's last value spilled to.
pub(crate) fn put(
    connection: &Connection,
    bucket: i64,
    path: &[u8],
    stamp: (i64, Option<i64>),
    raw: &Raw,
    spilled: Option<i64>,
) -> Result<()> {
    let (version, expires) = stamp;
    if let Some(spill) = spilled {
        execute(connection, DELETE_SPILLED, params![spill], "a write: the value it replaces")?;
    }
    let (value, spill) = match raw {
        Raw::Bytes(bytes) if bytes.len() > SPILL => {
            execute(connection, INSERT_SPILLED, params![bytes], "a write: its large value")?;
            (rusqlite::types::Value::Null, Some(connection.last_insert_rowid()))
        }
        Raw::Bytes(bytes) => (rusqlite::types::Value::Blob(bytes.clone()), None),
        Raw::Int(int) => (rusqlite::types::Value::Integer(*int), None),
        Raw::None => (rusqlite::types::Value::Null, None),
    };
    execute(connection, UPSERT, params![bucket, path, version, expires, value, spill], "a write")
}

/// Deletes a cell and the row its value spilled to.
pub(crate) fn remove(connection: &Connection, bucket: i64, path: &[u8], spilled: Option<i64>) -> Result<()> {
    execute(connection, DELETE_CELL, params![bucket, path], "a delete")?;
    if let Some(spill) = spilled {
        execute(connection, DELETE_SPILLED, params![spill], "a delete: its large value")?;
    }
    Ok(())
}

pub(crate) fn set_expires(connection: &Connection, bucket: i64, path: &[u8], expires: Option<i64>) -> Result<()> {
    execute(connection, UPDATE_EXPIRES, params![bucket, path, expires], "an expiry")
}

/// Moves a key's expiry on, unless it was written or touched since it was
/// read, and says whether it did.
pub(crate) fn renew(connection: &Connection, bucket: i64, path: &[u8], seen: (i64, i64), expires: i64) -> Result<bool> {
    let (version, seen_expires) = seen;
    let changed = connection
        .prepare_cached(RENEW)
        .and_then(|mut renew| renew.execute(params![bucket, path, version, seen_expires, expires]))
        .map_err(|error| sql_error("a renewal", error))?;
    Ok(changed > 0)
}

/// Adds `n` to the counter at `path` and returns its sum, or `None` when the
/// sum passes the int64 range and nothing changed.
pub(crate) fn add(
    connection: &Connection,
    bucket: i64,
    path: &[u8],
    stamp: (i64, Option<i64>),
    n: i64,
    now: i64,
) -> Result<Option<i64>> {
    let (version, expires) = stamp;
    connection
        .prepare_cached(ADD)
        .and_then(|mut add| add.query_row(params![bucket, path, version, expires, n, now], |row| row.get(0)).optional())
        .map_err(|error| sql_error("an add", error))
}

/// A page of a branch's own live cells, by path.
pub(crate) fn page(
    connection: &Connection,
    bucket: i64,
    range: (&[u8], &[u8]),
    now: i64,
    limit: usize,
) -> Result<Vec<(Vec<u8>, Cell)>> {
    let (after, past) = range;
    let read = || -> rusqlite::Result<Vec<(Vec<u8>, Cell)>> {
        let mut select = connection.prepare_cached(SELECT_PAGE)?;
        let rows = select.query_map(params![bucket, after, past, now, limit as i64], |row| {
            let cell =
                Cell { version: row.get(1)?, expires: row.get(2)?, raw: raw_of(row.get_ref(3)?, row.get_ref(4)?)? };
            Ok((row.get(0)?, cell))
        })?;
        rows.collect()
    };
    read().map_err(|error| sql_error("a page", error))
}

/// Deletes every cell in `[from, past)` when there are at most `bound` of
/// them, and says whether it did.
pub(crate) fn delete_under(connection: &Connection, bucket: i64, range: (&[u8], &[u8]), bound: usize) -> Result<bool> {
    let (from, past) = range;
    let count: i64 = connection
        .prepare_cached(COUNT_UNDER)
        .and_then(|mut count| count.query_row(params![bucket, from, past, bound as i64 + 1], |row| row.get(0)))
        .map_err(|error| sql_error("a clear: its size", error))?;
    if count as usize > bound {
        return Ok(false);
    }
    let spills = returned_spills(connection, DELETE_UNDER, params![bucket, from, past], "a clear")?;
    delete_spills(connection, &spills)?;
    Ok(true)
}

pub(crate) fn mark_cleared(connection: &Connection, bucket: i64, prefix: &[u8], cleared: i64) -> Result<()> {
    execute(connection, MARK_CLEARED, params![bucket, prefix, cleared], "a clear: its mark")
}

/// Deletes up to `limit` expired cells, the oldest first, with their spilled
/// values; says how many.
pub(crate) fn expire(connection: &Connection, now: i64, limit: usize) -> Result<usize> {
    let spills = returned_spills(connection, EXPIRE, params![now, limit as i64], "expiry")?;
    delete_spills(connection, &spills)?;
    Ok(spills.len())
}

pub(crate) fn marks(connection: &Connection) -> Result<Vec<Mark>> {
    let read = || -> rusqlite::Result<Vec<Mark>> {
        let mut select = connection.prepare_cached(SELECT_MARKS)?;
        let rows =
            select.query_map([], |row| Ok(Mark { bucket: row.get(0)?, prefix: row.get(1)?, cleared: row.get(2)? }))?;
        rows.collect()
    };
    read().map_err(|error| sql_error("the marked clears", error))
}

/// Deletes up to `limit` cells a mark hides, and the mark once none is left;
/// says how many it deleted.
pub(crate) fn drop_hidden(connection: &Connection, mark: &Mark, past: &[u8], limit: usize) -> Result<usize> {
    let spills = returned_spills(
        connection,
        DROP_HIDDEN,
        params![mark.bucket, mark.prefix, past, mark.cleared, limit as i64],
        "a marked clear",
    )?;
    delete_spills(connection, &spills)?;
    if spills.len() < limit {
        execute(connection, UNMARK, params![mark.bucket, mark.prefix, mark.cleared], "a marked clear: its mark")?;
    }
    Ok(spills.len())
}

pub(crate) fn revision(connection: &Connection) -> Result<i64> {
    connection.query_row(SELECT_REVISION, [], |row| row.get(0)).map_err(|error| sql_error("the revision", error))
}

pub(crate) fn keep_revision(connection: &Connection, revision: i64) -> Result<()> {
    execute(connection, KEEP_REVISION, params![revision], "the revision")
}

/// The row of `name`, made with `role` the first time: its id and the role it
/// holds.
pub(crate) fn bucket(connection: &Connection, name: &str, role: &str) -> Result<(i64, String)> {
    let found: Option<(i64, String)> = connection
        .prepare_cached(SELECT_BUCKET)
        .and_then(|mut select| select.query_row(params![name], |row| Ok((row.get(0)?, row.get(1)?))).optional())
        .map_err(|error| sql_error("its record", error))?;
    if let Some(found) = found {
        return Ok(found);
    }
    let id = connection
        .prepare_cached(INSERT_BUCKET)
        .and_then(|mut insert| insert.query_row(params![name, role], |row| row.get(0)))
        .map_err(|error| sql_error("its record", error))?;
    Ok((id, role.to_owned()))
}

/// A row's value: a spilled value when the cell names one, else what the cell
/// holds.
fn raw_of(value: ValueRef<'_>, spilled: ValueRef<'_>) -> rusqlite::Result<Raw> {
    match (value, spilled) {
        (ValueRef::Null, ValueRef::Blob(bytes)) => Ok(Raw::Bytes(bytes.to_vec())),
        (ValueRef::Null, _) => Ok(Raw::None),
        (ValueRef::Integer(int), _) => Ok(Raw::Int(int)),
        (ValueRef::Blob(bytes), _) => Ok(Raw::Bytes(bytes.to_vec())),
        (other, _) => Err(rusqlite::Error::InvalidColumnType(2, "value".to_owned(), other.data_type())),
    }
}

fn execute(connection: &Connection, sql: &str, params: impl rusqlite::Params, what: &str) -> Result<()> {
    connection
        .prepare_cached(sql)
        .and_then(|mut statement| statement.execute(params))
        .map(|_| ())
        .map_err(|error| sql_error(what, error))
}

fn returned_spills(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    what: &str,
) -> Result<Vec<Option<i64>>> {
    let run = || -> rusqlite::Result<Vec<Option<i64>>> {
        let mut statement = connection.prepare_cached(sql)?;
        let rows = statement.query_map(params, |row| row.get(0))?;
        rows.collect()
    };
    run().map_err(|error| sql_error(what, error))
}

fn delete_spills(connection: &Connection, spills: &[Option<i64>]) -> Result<()> {
    for spill in spills.iter().flatten() {
        execute(connection, DELETE_SPILLED, params![spill], "a large value")?;
    }
    Ok(())
}
