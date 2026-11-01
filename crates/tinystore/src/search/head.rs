//! The head: the postings sets wrote since packing last reached their key,
//! a row a key a set, which searches read beside the key's list.
//!
//! ```text
//! fresh (index_id, key, first) → postings: (doc − doc before, tf, len) a posting    varints
//! ```
//!
//! A row a key a set rather than one a posting: a set of 1,000 passages of
//! 50 words wrote 50,000 rows, which packing read and deleted again, and
//! their pages through the WAL each time took half of packing's time.
//!
//! A delete, or a set that replaces a document, takes its postings out of
//! the rows its set wrote alone, a row a key, as the head of a row a posting
//! did; a row a set of many wrote it leaves, since writing it again costs
//! the rest of the set's postings. The document is marked gone, which
//! searches leave out, packing leaves out of the blocks and a purge takes
//! out of the head. The head a release before 0002 kept, a row a posting,
//! is read and drained by `legacy.rs`.

use std::collections::HashSet;
use std::sync::LazyLock;

use rusqlite::Connection;

use super::legacy;
use super::postings::{Posting, pack_inline, unpack_inline};
use super::write::execute;
use crate::Result;
use crate::sqlite::{Tx, sql_error};

const READ_KEY: &str =
    "select first, postings from _tinystore_search_fresh where index_id = ?1 and key = ?2 order by first";
const READ_FIRST: &str =
    "select key, first, postings from _tinystore_search_fresh where index_id = ?1 order by key, first";
const ANY: &str = "select exists (select 1 from _tinystore_search_fresh where index_id = ?1)";
const DROP_KEYS: &str = "delete from _tinystore_search_fresh where index_id = ?1 and key >= ?2 and key <= ?3";
const DROP_ROW: &str = "delete from _tinystore_search_fresh where index_id = ?1 and key = ?2 and first = ?3";
const DROP_ALONE: &str =
    "delete from _tinystore_search_fresh where index_id = ?1 and key = ?2 and first = ?3 and count = 1";
const ADD_ROW: &str =
    "insert into _tinystore_search_fresh (index_id, key, first, count, postings) values (?1, ?2, ?3, ?4, ?5)";
/// Rows a statement writes, five values each.
const ROWS: usize = 64;
static ADD_ROWS: LazyLock<String> = LazyLock::new(|| {
    let rows = vec!["(?, ?, ?, ?, ?)"; ROWS].join(", ");
    format!("insert into _tinystore_search_fresh (index_id, key, first, count, postings) values {rows}")
});

/// Writes a set's postings, `(key, doc, tf, len)`, a row a key, many rows a
/// statement in the order of the head's keys; empties `postings`.
pub(super) fn write(tx: &Tx<'_>, index: i64, postings: &mut Vec<(i64, u32, u32, u32)>) -> Result<()> {
    postings.sort_unstable();
    let rows: Vec<(i64, u32, i64, Vec<u8>)> = postings
        .chunk_by(|a, b| a.0 == b.0)
        .map(|run| {
            let held: Vec<Posting> = run.iter().map(|&(_, doc, tf, len)| Posting { doc, tf, len }).collect();
            (run[0].0, run[0].1, run.len() as i64, pack_inline(&held))
        })
        .collect();
    let failed = |error| sql_error("write postings", error);
    let mut many = tx.prepare_cached(&ADD_ROWS).map_err(failed)?;
    let (chunks, rest) = rows.as_chunks::<ROWS>();
    for chunk in chunks {
        let values: Vec<&dyn rusqlite::ToSql> = chunk
            .iter()
            .flat_map(|(key, first, count, bytes)| [&index as &dyn rusqlite::ToSql, key, first, count, bytes])
            .collect();
        many.execute(values.as_slice()).map_err(failed)?;
    }
    for (key, first, count, bytes) in rest {
        execute(tx, ADD_ROW, (index, key, first, count, bytes), "write postings")?;
    }
    postings.clear();
    Ok(())
}

/// A key's postings in the head, in their documents' order.
pub(super) fn read(connection: &Connection, index: i64, key: i64) -> Result<Vec<Posting>> {
    let mut postings = legacy::head(connection, index, key)?;
    let failed = |error| sql_error(format!("read key {key}'s head"), error);
    let mut select = connection.prepare_cached(READ_KEY).map_err(failed)?;
    let mut rows = select.query((index, key)).map_err(failed)?;
    while let Some(row) = rows.next().map_err(failed)? {
        let bytes = row.get_ref(1).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
        postings.extend(unpack_inline(bytes).map_err(|error| error.within(format!("key {key}'s head")))?);
    }
    Ok(postings)
}

/// Whether the index's head holds a posting.
pub(super) fn any(connection: &Connection, index: i64) -> Result<bool> {
    let failed = |error| sql_error("read the head", error);
    let fresh: bool = connection
        .prepare_cached(ANY)
        .and_then(|mut select| select.query_row([index], |row| row.get(0)))
        .map_err(failed)?;
    Ok(fresh || legacy::any_head(connection, index)?)
}

/// Keys with their postings, in the keys' order.
pub(super) type Keyed = Vec<(i64, Vec<Posting>)>;

/// The head's first keys with their postings, whole keys, till they hold
/// `most` postings or the head ends; and whether it ended.
pub(super) fn first_keys(connection: &Connection, index: i64, most: usize) -> Result<(Keyed, bool)> {
    let failed = |error| sql_error("read the head", error);
    let mut select = connection.prepare_cached(READ_FIRST).map_err(failed)?;
    let mut rows = select.query([index]).map_err(failed)?;
    let mut keys: Keyed = Vec::new();
    let mut held = 0;
    while let Some(row) = rows.next().map_err(failed)? {
        let key: i64 = row.get(0).map_err(failed)?;
        if keys.last().is_none_or(|(last, _)| *last != key) {
            if held >= most {
                return Ok((keys, false));
            }
            keys.push((key, Vec::new()));
        }
        let bytes = row.get_ref(2).and_then(|value| Ok(value.as_blob()?)).map_err(failed)?;
        let postings = unpack_inline(bytes).map_err(|error| error.within(format!("key {key}'s head")))?;
        held += postings.len();
        if let Some((_, all)) = keys.last_mut() {
            all.extend(postings);
        }
    }
    Ok((keys, true))
}

/// Takes a document out of a key's row it wrote alone, if it wrote one.
pub(super) fn forget(tx: &Tx<'_>, index: i64, key: i64, doc: u32) -> Result<()> {
    execute(tx, DROP_ALONE, (index, key, doc), "drop a posting").map(drop)
}

/// Drops the head's rows of the keys from `first` to `last`, packed.
pub(super) fn drop_keys(tx: &Tx<'_>, index: i64, (first, last): (i64, i64)) -> Result<()> {
    execute(tx, DROP_KEYS, (index, first, last), "drop packed postings").map(drop)
}

/// Takes the documents `gone` out of a key's rows of the head.
pub(super) fn take_out(connection: &Connection, index: i64, key: i64, gone: &HashSet<u32>) -> Result<()> {
    legacy::take_out_of_head(connection, index, key, gone)?;
    let failed = |error| sql_error(format!("read key {key}'s head"), error);
    let mut select = connection.prepare_cached(READ_KEY).map_err(failed)?;
    let rows = select.query_map((index, key), |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?)));
    let rows: Vec<(u32, Vec<u8>)> = rows.and_then(Iterator::collect).map_err(failed)?;
    for (first, bytes) in rows {
        let held = unpack_inline(&bytes).map_err(|error| error.within(format!("key {key}'s head")))?;
        if !held.iter().any(|posting| gone.contains(&posting.doc)) {
            continue;
        }
        execute(connection, DROP_ROW, (index, key, first), "take postings out of the head")?;
        let left: Vec<Posting> = held.into_iter().filter(|posting| !gone.contains(&posting.doc)).collect();
        if let Some(head) = left.first() {
            execute(
                connection,
                ADD_ROW,
                (index, key, head.doc, left.len() as i64, pack_inline(&left)),
                "write postings",
            )?;
        }
    }
    Ok(())
}
