//! Purging: the documents gone so far taken out of the blocks that hold
//! them, a step of keys a commit, rewriting only the blocks whose range
//! holds one. Searches leave the gone out meanwhile, as they did before, so
//! no step changes an answer; the last forgets them.
//!
//! A purge runs once the gone pass 0.5 % of an index's documents, which
//! search-wchurn measured to keep the blocks' bounds tight at a cost as
//! short as packing's.

use std::collections::{BTreeMap, HashSet};

use rusqlite::Connection;

use super::head;
use super::legacy::Legacy;
use super::pack::{
    DROP_RUN, Stored, WRITE_LIST, WRITE_RUN, average_of, note_lists, read_list, read_run, write_directory,
};
use super::packed::{self, Directory, Placed, Run, decode_best};
use super::postings::{Posting, decode_keys, pack_inline, unpack_inline};
use super::write::execute;
use crate::Result;
use crate::sqlite::{Tx, sql_error};

const READ_GONE: &str = "select doc, keys from _tinystore_search_gone where index_id = ?1";
const COUNT_GONE: &str = "select (select count(*) from _tinystore_search_gone where index_id = ?1), documents
    from _tinystore_search_indexes where id = ?1";
const DROP_LIST: &str = "delete from _tinystore_search_lists where key = ?1";
const DROP_EMPTY_KEY: &str = "delete from _tinystore_search_keys where id = ?1 and documents = 0";
const FORGET_GONE: &str = "delete from _tinystore_search_gone where index_id = ?1 and doc = ?2";
const NOTE_PURGED: &str = "update _tinystore_search_indexes set gone_version = gone_version + 1 where id = ?1";

/// Keys a step of a purge goes through at most.
pub(crate) const STEP_KEYS: usize = 512;

/// A purge under way: the documents it takes out, and each key that holds
/// any of them with the ones it holds.
#[derive(Debug)]
pub(crate) struct Purge {
    docs: Vec<u32>,
    keys: Vec<(i64, Vec<u32>)>,
    at: usize,
}

/// Whether an index's gone pass 0.5 % of its documents, or any are gone
/// when `any` says so.
pub(crate) fn due(connection: &Connection, index: i64, any: bool) -> Result<bool> {
    let failed = |error| sql_error("count the documents gone", error);
    let (gone, documents): (i64, i64) = connection
        .prepare_cached(COUNT_GONE)
        .and_then(|mut select| select.query_row([index], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(failed)?;
    Ok(gone > 0 && (any || gone * 200 > documents))
}

/// The purge of the documents gone now.
pub(crate) fn begin(connection: &Connection, index: i64) -> Result<Purge> {
    let failed = |error| sql_error("read the documents gone", error);
    let mut select = connection.prepare_cached(READ_GONE).map_err(failed)?;
    let rows = select.query_map([index], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?)));
    let rows: Vec<(u32, Vec<u8>)> = rows.and_then(Iterator::collect).map_err(failed)?;
    let mut keys: BTreeMap<i64, Vec<u32>> = BTreeMap::new();
    for (doc, held) in &rows {
        for key in decode_keys(held).map_err(|error| error.within(format!("document {doc}")))? {
            keys.entry(key).or_default().push(*doc);
        }
    }
    keys.values_mut().for_each(|docs| docs.sort_unstable());
    Ok(Purge { docs: rows.into_iter().map(|(doc, _)| doc).collect(), keys: keys.into_iter().collect(), at: 0 })
}

impl Purge {
    /// Takes the purge's documents out of the next `most_keys` keys' lists,
    /// and forgets them in the step that reaches the last key. Says where
    /// the purge stands after it, for [`Purge::advance`] once it commits.
    pub(crate) fn step(&self, tx: &Tx<'_>, index: i64, most_keys: usize) -> Result<usize> {
        let end = (self.at + most_keys).min(self.keys.len());
        for (key, docs) in &self.keys[self.at..end] {
            take_out(tx, index, *key, docs).map_err(|error| error.within(format!("key {key}")))?;
        }
        note_lists(tx, index)?;
        if end == self.keys.len() {
            for doc in &self.docs {
                execute(tx, FORGET_GONE, (index, doc), "forget a document gone")?;
            }
            execute(tx, NOTE_PURGED, [index], "count what is gone")?;
        }
        Ok(end)
    }

    /// Moves the purge to where a committed step left it; says whether it is done.
    pub(crate) fn advance(&mut self, at: usize) -> bool {
        self.at = at;
        at == self.keys.len()
    }

    /// The documents it takes out.
    pub(crate) fn documents(&self) -> usize {
        self.docs.len()
    }
}

/// Takes `docs`, sorted, out of a key's head and its list: out of its own row if short,
/// else out of each block whose range holds one, its run written again or
/// dropped once empty, its directory with them. A key a release before 0002
/// wrote in rows of blocks is written in runs first. A key no document holds
/// any more and whose list is empty is forgotten.
fn take_out(connection: &Connection, index: i64, key: i64, docs: &[u32]) -> Result<()> {
    let gone: HashSet<u32> = docs.iter().copied().collect();
    head::take_out(connection, index, key, &gone)?;
    let left = match read_list(connection, key)? {
        Stored::Packed(directory, best) => {
            let best = decode_best(&best)?;
            take_out_of_runs(connection, key, (Directory::decode(&directory)?, best), docs, &gone)?
        }
        Stored::Legacy(directory) => {
            let average = average_of(connection, index, key)?;
            let runs = Legacy::decode(key, &directory)?.into_runs(connection, average)?;
            take_out_of_runs(connection, key, (runs.finish(connection)?, Vec::new()), docs, &gone)?
        }
        Stored::Inline(inline) => {
            let mut postings = unpack_inline(&inline)?;
            postings.retain(|posting| !gone.contains(&posting.doc));
            if !postings.is_empty() {
                execute(connection, WRITE_LIST, (key, None::<Vec<u8>>, pack_inline(&postings)), "write a list")?;
            }
            postings.len()
        }
        Stored::Nothing => 0,
    };
    if left == 0 {
        execute(connection, DROP_LIST, [key], "drop a list")?;
        execute(connection, DROP_EMPTY_KEY, [key], "drop a key")?;
    }
    Ok(())
}

/// Takes `docs` out of the blocks that may hold them, each run that holds
/// one written again; says how many blocks the list keeps.
fn take_out_of_runs(
    connection: &Connection,
    key: i64,
    (directory, mut best): (Directory, Vec<Posting>),
    docs: &[u32],
    gone: &HashSet<u32>,
) -> Result<usize> {
    best.retain(|posting| !gone.contains(&posting.doc));
    let blocks = &directory.blocks;
    let mut touched: Vec<usize> = docs
        .iter()
        .filter_map(|doc| {
            let at = blocks.partition_point(|block| block.first <= *doc).checked_sub(1)?;
            (*doc <= blocks[at].last).then_some(at)
        })
        .collect();
    touched.dedup();
    if touched.is_empty() {
        return Ok(blocks.len());
    }
    let mut kept = Directory::default();
    for (place, run) in directory.runs.iter().enumerate() {
        let range = directory.blocks_of(place);
        let first_block = kept.blocks.len() as u32;
        if !touched.iter().any(|at| range.contains(at)) {
            kept.runs.push(Run { first_block, ..*run });
            kept.blocks.extend_from_slice(&blocks[range]);
            continue;
        }
        let written = take_out_of_run(connection, (&directory, place), (&touched, gone), &mut kept.blocks)?;
        if written.is_empty() {
            execute(connection, DROP_RUN, [run.id], "drop a run")?;
        } else {
            execute(connection, WRITE_RUN, (run.id, &written), "write a run")?;
            kept.runs.push(Run { first_block, ..*run });
        }
    }
    if !kept.blocks.is_empty() {
        write_directory(connection, key, &kept, &best)?;
    }
    Ok(kept.blocks.len())
}

/// The bytes of the run at `place` once the gone are out of its `touched`
/// blocks, a block left empty dropped; each block it keeps onto `kept`, at
/// its new place. A block keeps its bound, which fewer postings never pass,
/// so that no length is read again.
fn take_out_of_run(
    connection: &Connection,
    (directory, place): (&Directory, usize),
    (touched, gone): (&[usize], &HashSet<u32>),
    kept: &mut Vec<Placed>,
) -> Result<Vec<u8>> {
    let run = directory.runs[place].id;
    let bytes = read_run(connection, run)?;
    let mut written = Vec::with_capacity(bytes.len());
    let mut held = Vec::new();
    for at in directory.blocks_of(place) {
        let block = directory.blocks[at];
        let lying = directory.bytes_of(at, bytes.len());
        let start = written.len() as u16;
        if touched.binary_search(&at).is_err() {
            written.extend_from_slice(&bytes[lying]);
            kept.push(Placed { at: start, ..block });
            continue;
        }
        packed::unpack(&bytes[lying], block.first, &mut held).map_err(|error| error.within(format!("run {run}")))?;
        let left: Vec<Posting> =
            held.iter().filter(|(doc, _)| !gone.contains(doc)).map(|&(doc, tf)| Posting { doc, tf, len: 0 }).collect();
        if let (Some(first), Some(last)) = (left.first(), left.last()) {
            packed::pack(&left, &mut written);
            kept.push(Placed { first: first.doc, last: last.doc, at: start, ..block });
        }
    }
    Ok(written)
}
