//! Packing: the head's postings appended to their keys' lists, a step of
//! keys a commit, so that the writer is held for a step and never for the
//! whole head. search-wchurn measured steps of 2,048 rows holding it 92 ms
//! at most, where one commit held it a second.
//!
//! A step first gives every document set since the last step its lengths
//! in `norms`, and only then appends: a posting in a block finds its length
//! there, and a document's lengths are written once and never change. A key
//! is packed whole in one step, so a key's head always follows its blocks.
//!
//! A key's blocks are appended to its runs, as `packed.rs` lays them out; a
//! key a release before 0002 wrote is written in runs first, as
//! `legacy.rs` writes it.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};

use super::head;
use super::kept::{NORMS, bytes_of, lengths_of};
use super::legacy::{self, Legacy};
use super::packed::{
    self, BEST_AT, Directory, Placed, RUN_BYTES, Run, best_of, bound_of, decode_best, encode_best, reference,
};
use super::postings::{BLOCK, INLINE, Posting, decode_lengths, pack_inline, unpack_inline};
use super::score::average;
use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_NUMBERS: &str = "select next_doc, normed from _tinystore_search_indexes where id = ?1";
const READ_LENGTHS: &str = "select doc, lengths from _tinystore_search_docs where index_id = ?1 and doc >= ?2";
const WRITE_NORMS: &str =
    "insert or replace into _tinystore_search_norms (index_id, part, chunk, lengths) values (?1, ?2, ?3, ?4)";
const SET_NORMED: &str = "update _tinystore_search_indexes set normed = ?2 where id = ?1";
const READ_LIST: &str = "select directory, inline, packed, best from _tinystore_search_lists where key = ?1";
pub(super) const WRITE_LIST: &str =
    "insert or replace into _tinystore_search_lists (key, directory, inline) values (?1, ?2, ?3)";
const WRITE_PACKED: &str = "insert or replace into _tinystore_search_lists (key, packed, best) values (?1, ?2, ?3)";
use super::kept::READ_NORMS;
const READ_RUN: &str = "select blocks from _tinystore_search_runs where id = ?1";
pub(super) const WRITE_RUN: &str = "insert or replace into _tinystore_search_runs (id, blocks) values (?1, ?2)";
pub(super) const DROP_RUN: &str = "delete from _tinystore_search_runs where id = ?1";
const READ_PART: &str = "select part from _tinystore_search_keys where id = ?1";
const READ_GONE: &str = "select doc from _tinystore_search_gone where index_id = ?1";
const NOTE_LISTS: &str = "update _tinystore_search_indexes set lists_version = lists_version + 1 where id = ?1";
const READ_TOKENS: &str = "select p.tokens, i.documents from _tinystore_search_parts as p
    join _tinystore_search_indexes as i on i.id = p.index_id where p.index_id = ?1 and p.part = ?2";

/// A run's number among its key's is the low bits of its id, the key's id above them.
const RUN_BITS: u32 = 20;

/// Head rows a step packs at most, unless one key holds more.
pub(crate) const STEP_ROWS: usize = 2048;

/// What a step of packing did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Packed {
    pub(crate) postings: u64,
    /// The head is empty after it.
    pub(crate) done: bool,
}

/// Packs a step of an index's head: its first keys whole, till they hold
/// `most` postings, or, while a release before 0002 left a head of a row a
/// posting, a step of that head, whose documents are all before this one's.
pub(crate) fn step(tx: &Tx<'_>, index: i64, most: usize) -> Result<Packed> {
    norm(tx, index)?;
    if legacy::any_head(tx, index)? {
        return legacy::head_step(tx, index, most);
    }
    let (keys, done) = head::first_keys(tx, index, most)?;
    let (Some(first), Some(last)) = (keys.first().map(|key| key.0), keys.last().map(|key| key.0)) else {
        return Ok(Packed { done: true, ..Packed::default() });
    };
    let postings = append_keys(tx, index, &keys)?;
    head::drop_keys(tx, index, (first, last))?;
    Ok(Packed { postings, done })
}

/// Appends each key's postings to its list, those of documents gone left
/// out: a set that replaces a document leaves the old one's in the head,
/// and they never reach a block. Says how many it appended.
pub(super) fn append_keys(tx: &Tx<'_>, index: i64, keys: &[(i64, Vec<Posting>)]) -> Result<u64> {
    let gone = gone(tx, index)?;
    let mut averages = Averages::of(index);
    let mut appended = 0;
    for (key, postings) in keys {
        let fresh: Vec<Posting> = postings.iter().filter(|posting| !gone.contains(&posting.doc)).copied().collect();
        if fresh.is_empty() {
            continue;
        }
        let packed = averages.of_key(tx, *key).and_then(|average| append(tx, *key, &fresh, average));
        packed.map_err(|error| error.within(format!("key {key}")))?;
        appended += fresh.len() as u64;
    }
    note_lists(tx, index)?;
    Ok(appended)
}

/// The index's documents gone, not purged yet.
fn gone(connection: &Connection, index: i64) -> Result<HashSet<u32>> {
    let failed = |error| sql_error("read the documents gone", error);
    let mut select = connection.prepare_cached(READ_GONE).map_err(failed)?;
    let docs = select.query_map([index], |row| row.get::<_, u32>(0));
    docs.and_then(Iterator::collect).map_err(failed)
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

/// The average a key's new blocks are bounded at, its part's now, each part
/// read once a step.
struct Averages {
    index: i64,
    parts: HashMap<u32, u16>,
}

impl Averages {
    fn of(index: i64) -> Averages {
        Averages { index, parts: HashMap::new() }
    }

    fn of_key(&mut self, connection: &Connection, key: i64) -> Result<u16> {
        let failed = |error| sql_error("read a key's part", error);
        let part: u32 = connection
            .prepare_cached(READ_PART)
            .and_then(|mut select| select.query_row([key], |row| row.get(0)))
            .map_err(failed)?;
        if let Some(average) = self.parts.get(&part) {
            return Ok(*average);
        }
        let failed = |error| sql_error("read a part's lengths", error);
        let (tokens, documents): (i64, i64) = connection
            .prepare_cached(READ_TOKENS)
            .and_then(|mut select| {
                select.query_row((self.index, part), |row| Ok((row.get(0)?, row.get(1)?))).optional()
            })
            .map_err(failed)?
            .unwrap_or((0, 0));
        let bounded_at = reference(average(tokens.max(0) as u64, documents.max(0) as u64));
        self.parts.insert(part, bounded_at);
        Ok(bounded_at)
    }
}

/// The average a key's blocks are bounded at when a purge writes them,
/// read for the one key.
pub(super) fn average_of(connection: &Connection, index: i64, key: i64) -> Result<u16> {
    Averages::of(index).of_key(connection, key)
}

/// Appends a key's head postings to its list: a key short enough stays in
/// its row, else its last block is filled before new blocks follow it in
/// its runs, each bounded at `average`.
fn append(connection: &Connection, key: i64, fresh: &[Posting], average: u16) -> Result<()> {
    let mut list = match read_list(connection, key)? {
        Stored::Packed(directory, best) => {
            Runs::of(connection, key, Directory::decode(&directory)?, decode_best(&best)?)?
        }
        Stored::Legacy(directory) => Legacy::decode(key, &directory)?.into_runs(connection, average)?,
        Stored::Inline(inline) => return append_inline(connection, key, unpack_inline(&inline)?, fresh, average),
        Stored::Nothing => return append_inline(connection, key, Vec::new(), fresh, average),
    };
    list.consider(fresh, average);
    let filled = list.fill(fresh)?;
    for block in fresh[filled..].chunks(BLOCK) {
        list.push(connection, block, bound_of(block, average), average)?;
    }
    list.finish(connection).map(drop)
}

/// Appends to a key that keeps its postings in its own row: in it still
/// while they are few enough, else in runs.
fn append_inline(
    connection: &Connection,
    key: i64,
    mut all: Vec<Posting>,
    fresh: &[Posting],
    average: u16,
) -> Result<()> {
    all.extend_from_slice(fresh);
    if all.len() <= INLINE {
        return execute(connection, WRITE_LIST, (key, None::<Vec<u8>>, pack_inline(&all)), "write a list").map(drop);
    }
    let mut list = Runs::new(key)?;
    list.consider(&all, average);
    for block in all.chunks(BLOCK) {
        list.push(connection, block, bound_of(block, average), average)?;
    }
    list.finish(connection).map(drop)
}

/// A key's list in runs as packing or a purge writes it: its directory, and
/// the bytes of its last run, which takes the blocks pushed till it holds
/// 32 KiB. Every other run is written as it fills; the last and the
/// directory when it finishes.
pub(super) struct Runs {
    key: i64,
    directory: Directory,
    last: Vec<u8>,
    best: Vec<Posting>,
}

impl Runs {
    pub(super) fn new(key: i64) -> Result<Runs> {
        // a run's id holds its key's above its own number
        if key >> (63 - RUN_BITS) != 0 {
            return Err(Error::limit(format!("key {key}: past the keys a run's id can name")));
        }
        Ok(Runs { key, directory: Directory::default(), last: Vec::new(), best: Vec::new() })
    }

    /// A key's list as its directory, its best postings and its last run hold it.
    pub(super) fn of(connection: &Connection, key: i64, directory: Directory, best: Vec<Posting>) -> Result<Runs> {
        let last = match directory.runs.last() {
            Some(run) => read_run(connection, run.id)?,
            None => Vec::new(),
        };
        Ok(Runs { directory, last, best, ..Runs::new(key)? })
    }

    /// Keeps the best of `postings` among the list's best, at `average`.
    fn consider(&mut self, postings: &[Posting], average: u16) {
        self.best = best_of(std::mem::take(&mut self.best), postings, average);
    }

    /// Fills the last block with the first of `fresh` while it holds fewer
    /// than a block's postings, bounded at its own average; says how many it
    /// took. The block's own postings keep its bound, their lengths unread.
    fn fill(&mut self, fresh: &[Posting]) -> Result<usize> {
        let Some(index) = self.directory.blocks.len().checked_sub(1) else { return Ok(0) };
        let placed = self.directory.blocks[index];
        let bytes = self.directory.bytes_of(index, self.last.len());
        let mut held = Vec::new();
        packed::unpack(&self.last[bytes.clone()], placed.first, &mut held)?;
        let room = BLOCK.saturating_sub(held.len()).min(fresh.len());
        if room == 0 {
            return Ok(0);
        }
        let mut block: Vec<Posting> = held.into_iter().map(|(doc, tf)| Posting { doc, tf, len: 0 }).collect();
        block.extend_from_slice(&fresh[..room]);
        self.last.truncate(bytes.start);
        packed::pack(&block, &mut self.last);
        let bound = placed.bound.max(bound_of(&fresh[..room], placed.average));
        self.directory.blocks[index] = Placed { last: block[block.len() - 1].doc, bound, ..placed };
        Ok(room)
    }

    /// Appends a block to the last run, or to a new one once it holds 32 KiB.
    pub(super) fn push(&mut self, connection: &Connection, block: &[Posting], bound: f32, average: u16) -> Result<()> {
        if self.directory.runs.is_empty() || self.last.len() >= RUN_BYTES {
            self.next_run(connection)?;
        }
        let at = u16::try_from(self.last.len()).map_err(|_| Error::internal("a block starts past its run's place"))?;
        packed::pack(block, &mut self.last);
        let (first, last) = (block[0].doc, block[block.len() - 1].doc);
        self.directory.blocks.push(Placed { first, last, bound, average, at });
        Ok(())
    }

    /// Writes the last run and begins the next, numbered after it.
    fn next_run(&mut self, connection: &Connection) -> Result<()> {
        let number = match self.directory.runs.last() {
            Some(run) => {
                execute(connection, WRITE_RUN, (run.id, &self.last), "write a run")?;
                (run.id & ((1 << RUN_BITS) - 1)) + 1
            }
            None => 0,
        };
        if number >> RUN_BITS != 0 {
            return Err(Error::limit("a key's postings past the runs its directory can name"));
        }
        let first_block = self.directory.blocks.len() as u32;
        self.directory.runs.push(Run { id: self.key << RUN_BITS | number, first_block });
        self.last.clear();
        Ok(())
    }

    /// Writes the last run and the directory; says the directory.
    pub(super) fn finish(self, connection: &Connection) -> Result<Directory> {
        if let Some(run) = self.directory.runs.last() {
            execute(connection, WRITE_RUN, (run.id, &self.last), "write a run")?;
        }
        let best = if self.directory.blocks.len() >= BEST_AT { self.best.as_slice() } else { &[] };
        write_directory(connection, self.key, &self.directory, best)?;
        Ok(self.directory)
    }
}

/// Counts a commit that changes the index's lists, so that a list a search
/// keeps from before is read again.
pub(super) fn note_lists(connection: &Connection, index: i64) -> Result<()> {
    execute(connection, NOTE_LISTS, [index], "count a change of lists").map(drop)
}

pub(super) fn write_directory(
    connection: &Connection,
    key: i64,
    directory: &Directory,
    best: &[Posting],
) -> Result<()> {
    execute(connection, WRITE_PACKED, (key, directory.encode(), encode_best(best)), "write a list").map(drop)
}

/// Every document a key's runs hold, in order.
pub(super) fn documents(connection: &Connection, directory: &Directory) -> Result<Vec<u32>> {
    let (mut docs, mut pairs) = (Vec::new(), Vec::new());
    for (place, run) in directory.runs.iter().enumerate() {
        let bytes = read_run(connection, run.id)?;
        for at in directory.blocks_of(place) {
            packed::unpack(&bytes[directory.bytes_of(at, bytes.len())], directory.blocks[at].first, &mut pairs)?;
            docs.extend(pairs.iter().map(|(doc, _)| *doc));
        }
    }
    Ok(docs)
}

pub(super) fn read_run(connection: &Connection, id: i64) -> Result<Vec<u8>> {
    let failed = |error| sql_error("read a run", error);
    let bytes: Option<Vec<u8>> = connection
        .prepare_cached(READ_RUN)
        .and_then(|mut select| select.query_row([id], |row| row.get(0)).optional())
        .map_err(failed)?;
    bytes.ok_or_else(|| Error::corrupt(format!("run {id} a directory names is missing")))
}

/// A key's list as its row keeps it.
#[derive(Debug)]
pub(super) enum Stored {
    /// The directory of its blocks in runs, and its best postings.
    Packed(Vec<u8>, Vec<u8>),
    /// The directory of its rows of blocks, as a release before 0002 wrote it.
    Legacy(Vec<u8>),
    /// Its postings themselves.
    Inline(Vec<u8>),
    /// No row: the key's postings, if any, are all in the head.
    Nothing,
}

pub(super) fn read_list(connection: &Connection, key: i64) -> Result<Stored> {
    let failed = |error| sql_error("read a list", error);
    let stored = |row: &rusqlite::Row<'_>| {
        Ok(match (row.get(2)?, row.get(0)?, row.get(1)?) {
            (Some(packed), _, _) => Stored::Packed(packed, row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default()),
            (None, Some(directory), _) => Stored::Legacy(directory),
            (None, None, Some(inline)) => Stored::Inline(inline),
            (None, None, None) => Stored::Nothing,
        })
    };
    let row = connection.prepare_cached(READ_LIST).and_then(|mut select| select.query_row([key], stored).optional());
    Ok(row.map_err(failed)?.unwrap_or(Stored::Nothing))
}
