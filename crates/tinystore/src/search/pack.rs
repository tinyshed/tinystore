//! Packing: the head's postings appended to their keys' lists, a step of
//! keys a commit, so that the writer is held for a step and never for the
//! whole head. search-wchurn measured steps of 2,048 rows holding it 92 ms
//! at most, where one commit held it a second.
//!
//! A step first gives every document set since the last step its lengths
//! in `norms`, and only then appends: a posting in a block finds its length
//! there, and a document's lengths are written once and never change. A key
//! is packed whole in one step, so a key's head always follows its blocks.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};

use super::kept::{NORMS, bytes_of, lengths_of};
use super::postings::{
    BLOCK, Entry, INLINE, Posting, decode_directory, decode_lengths, encode_directory, entry_of, pack, pack_inline,
    unpack, unpack_inline,
};
use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_NUMBERS: &str = "select next_doc, normed from _tinystore_search_indexes where id = ?1";
const READ_LENGTHS: &str = "select doc, lengths from _tinystore_search_docs where index_id = ?1 and doc >= ?2";
const WRITE_NORMS: &str =
    "insert or replace into _tinystore_search_norms (index_id, part, chunk, lengths) values (?1, ?2, ?3, ?4)";
const SET_NORMED: &str = "update _tinystore_search_indexes set normed = ?2 where id = ?1";
const HEAD_FIRST: &str = "select key, doc, tf, len from _tinystore_search_head where index_id = ?1 order by key, doc
    limit cast(?2 as integer)";
const HEAD_OF: &str =
    "select key, doc, tf, len from _tinystore_search_head where index_id = ?1 and key = ?2 order by doc";
const DROP_HEAD: &str = "delete from _tinystore_search_head where index_id = ?1 and key >= ?2 and key <= ?3";
const READ_LIST: &str = "select directory, inline from _tinystore_search_lists where key = ?1";
pub(super) const WRITE_LIST: &str =
    "insert or replace into _tinystore_search_lists (key, directory, inline) values (?1, ?2, ?3)";
use super::kept::READ_NORMS;
use super::read::READ_BLOCK;
pub(super) const WRITE_BLOCK: &str =
    "insert or replace into _tinystore_search_blocks (key, first, payload) values (?1, ?2, ?3)";

/// Head rows a step packs at most, unless one key holds more.
pub(crate) const STEP_ROWS: usize = 2048;

/// What a step of packing did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Packed {
    pub(crate) postings: u64,
    /// The head is empty after it.
    pub(crate) done: bool,
}

/// Packs a step of an index's head, `most_rows` of its rows at most unless
/// its first key holds more.
pub(crate) fn step(tx: &Tx<'_>, index: i64, most_rows: usize) -> Result<Packed> {
    norm(tx, index)?;
    let rows = head_rows(tx, index, most_rows)?;
    let done = rows.len() <= most_rows;
    let rows = whole_keys(tx, index, rows, most_rows)?;
    let (Some(first), Some(last)) = (rows.first().map(|row| row.0), rows.last().map(|row| row.0)) else {
        return Ok(Packed { done: true, ..Packed::default() });
    };
    for run in rows.chunk_by(|a, b| a.0 == b.0) {
        let fresh: Vec<Posting> = run.iter().map(|(_, posting)| *posting).collect();
        append(tx, run[0].0, &fresh).map_err(|error| error.within(format!("key {}", run[0].0)))?;
    }
    execute(tx, DROP_HEAD, (index, first, last), "drop packed postings")?;
    Ok(Packed { postings: rows.len() as u64, done })
}

/// Gives every document set since the last step its lengths, row of
/// `norms` by row.
fn norm(tx: &Tx<'_>, index: i64) -> Result<()> {
    let failed = |error| sql_error("read what is normed", error);
    let (next_doc, normed): (i64, i64) = tx
        .prepare_cached(READ_NUMBERS)
        .and_then(|mut select| select.query_row([index], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(failed)?;
    if normed >= next_doc {
        return Ok(());
    }
    let mut rows: BTreeMap<(u32, u32), Vec<(u32, u32)>> = BTreeMap::new();
    let mut select = tx.prepare_cached(READ_LENGTHS).map_err(failed)?;
    let mut docs = select.query((index, normed)).map_err(failed)?;
    while let Some(row) = docs.next().map_err(failed)? {
        let doc: u32 = row.get(0).map_err(failed)?;
        let lengths: Vec<u8> = row.get(1).map_err(failed)?;
        for (part, len) in decode_lengths(&lengths).map_err(|error| error.within(format!("document {doc}")))? {
            rows.entry((part, doc / NORMS)).or_default().push((doc % NORMS, len));
        }
    }
    for ((part, chunk), lengths) in rows {
        write_norms(tx, index, (part, chunk), &lengths)?;
    }
    execute(tx, SET_NORMED, (index, next_doc), "write what is normed").map(drop)
}

/// Sets lengths in a row of `norms`, each at its document's place in it.
fn write_norms(connection: &Connection, index: i64, (part, chunk): (u32, u32), lengths: &[(u32, u32)]) -> Result<()> {
    let failed = |error| sql_error("read lengths", error);
    let bytes: Option<Vec<u8>> = connection
        .prepare_cached(READ_NORMS)
        .and_then(|mut select| select.query_row((index, part, chunk), |row| row.get(0)).optional())
        .map_err(failed)?;
    let mut row = lengths_of(&bytes.unwrap_or_default());
    for &(at, len) in lengths {
        let at = at as usize;
        if row.len() <= at {
            row.resize(at + 1, 0);
        }
        row[at] = u16::try_from(len).unwrap_or(u16::MAX);
    }
    execute(connection, WRITE_NORMS, (index, part, chunk, bytes_of(&row)), "write lengths").map(drop)
}

/// The head's first rows in key order, one more than a step takes, to tell
/// whether a key goes on past it.
fn head_rows(connection: &Connection, index: i64, most_rows: usize) -> Result<Vec<(i64, Posting)>> {
    let failed = |error| sql_error("read the head", error);
    let mut select = connection.prepare_cached(HEAD_FIRST).map_err(failed)?;
    let rows = select.query_map((index, most_rows as i64 + 1), posting);
    rows.and_then(Iterator::collect).map_err(failed)
}

/// The rows of whole keys: the last key left for the next step when a row
/// of it lies past the step, or read whole when it is the only key.
fn whole_keys(
    connection: &Connection,
    index: i64,
    mut rows: Vec<(i64, Posting)>,
    most_rows: usize,
) -> Result<Vec<(i64, Posting)>> {
    if rows.len() <= most_rows {
        return Ok(rows);
    }
    let last = rows[rows.len() - 1].0;
    if rows[0].0 != last {
        rows.retain(|(key, _)| *key != last);
        return Ok(rows);
    }
    let failed = |error| sql_error("read the head", error);
    let mut select = connection.prepare_cached(HEAD_OF).map_err(failed)?;
    let rows = select.query_map((index, last), posting);
    rows.and_then(Iterator::collect).map_err(failed)
}

fn posting(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Posting)> {
    Ok((row.get(0)?, Posting { doc: row.get(1)?, tf: row.get(2)?, len: row.get(3)? }))
}

/// Appends a key's head postings to its list: a key short enough stays in
/// its row, a list's short last block is filled before new blocks follow it.
fn append(connection: &Connection, key: i64, fresh: &[Posting]) -> Result<()> {
    let directory = match read_list(connection, key)? {
        Stored::Blocks(directory) => directory,
        Stored::Inline(inline) => return append_inline(connection, key, unpack_inline(&inline)?, fresh),
        Stored::Nothing => return append_inline(connection, key, Vec::new(), fresh),
    };
    let mut entries = decode_directory(&directory)?;
    let Some(last) = entries.pop() else { return write_blocks(connection, key, entries, fresh) };
    let held = read_block(connection, key, last.first)?;
    let room = BLOCK.saturating_sub(held.len()).min(fresh.len());
    let (filling, rest) = fresh.split_at(room);
    if filling.is_empty() {
        entries.push(last);
        return write_blocks(connection, key, entries, rest);
    }
    // The block's own postings bound it by its entry; their lengths are not needed again.
    let mut block: Vec<Posting> = held.iter().map(|&(doc, tf)| Posting { doc, tf, len: last.least_len }).collect();
    block.extend_from_slice(filling);
    let bound = entry_of(&block);
    entries.push(Entry { most_tf: bound.most_tf.max(last.most_tf), ..bound });
    execute(connection, WRITE_BLOCK, (key, last.first, pack(&block)), "write a block")?;
    write_blocks(connection, key, entries, rest)
}

/// Appends to a key that keeps its postings in its own row: in it still
/// while they are few enough, else in blocks.
fn append_inline(connection: &Connection, key: i64, mut all: Vec<Posting>, fresh: &[Posting]) -> Result<()> {
    all.extend_from_slice(fresh);
    if all.len() <= INLINE {
        return execute(connection, WRITE_LIST, (key, None::<Vec<u8>>, pack_inline(&all)), "write a list").map(drop);
    }
    write_blocks(connection, key, Vec::new(), &all)
}

/// Writes `fresh` in new blocks after `entries`, and the list's directory.
fn write_blocks(connection: &Connection, key: i64, mut entries: Vec<Entry>, fresh: &[Posting]) -> Result<()> {
    for block in fresh.chunks(BLOCK) {
        entries.push(entry_of(block));
        execute(connection, WRITE_BLOCK, (key, block[0].doc, pack(block)), "write a block")?;
    }
    execute(connection, WRITE_LIST, (key, encode_directory(&entries), None::<Vec<u8>>), "write a list").map(drop)
}

/// A key's list as its row keeps it.
#[derive(Debug)]
pub(super) enum Stored {
    /// The directory of its blocks.
    Blocks(Vec<u8>),
    /// Its postings themselves.
    Inline(Vec<u8>),
    /// No row: the key's postings, if any, are all in the head.
    Nothing,
}

pub(super) fn read_list(connection: &Connection, key: i64) -> Result<Stored> {
    let failed = |error| sql_error("read a list", error);
    let stored = |row: &rusqlite::Row<'_>| {
        Ok(match (row.get(0)?, row.get(1)?) {
            (Some(directory), _) => Stored::Blocks(directory),
            (None, Some(inline)) => Stored::Inline(inline),
            (None, None) => Stored::Nothing,
        })
    };
    let row = connection.prepare_cached(READ_LIST).and_then(|mut select| select.query_row([key], stored).optional());
    Ok(row.map_err(failed)?.unwrap_or(Stored::Nothing))
}

pub(super) fn read_block(connection: &Connection, key: i64, first: u32) -> Result<Vec<(u32, u32)>> {
    let failed = |error| sql_error("read a block", error);
    let payload: Option<Vec<u8>> = connection
        .prepare_cached(READ_BLOCK)
        .and_then(|mut select| select.query_row((key, first), |row| row.get(0)).optional())
        .map_err(failed)?;
    let payload =
        payload.ok_or_else(|| Error::corrupt(format!("block at document {first} its directory names is missing")))?;
    unpack(&payload).map_err(|error| error.within(format!("block at document {first}")))
}
