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

use super::pack::{Stored, WRITE_BLOCK, WRITE_LIST, read_block, read_list};
use super::postings::{
    Entry, Posting, decode_directory, decode_keys, encode_directory, entry_of, pack, pack_inline, unpack_inline,
};
use super::write::execute;
use crate::Result;
use crate::sqlite::{Tx, sql_error};

const READ_GONE: &str = "select doc, keys from _tinystore_search_gone where index_id = ?1";
const COUNT_GONE: &str = "select (select count(*) from _tinystore_search_gone where index_id = ?1), documents
    from _tinystore_search_indexes where id = ?1";
const DROP_BLOCK: &str = "delete from _tinystore_search_blocks where key = ?1 and first = ?2";
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
            take_out(tx, *key, docs).map_err(|error| error.within(format!("key {key}")))?;
        }
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

/// Takes `docs`, sorted, out of a key's list: out of its own row if short,
/// else out of each block whose range holds one, written again or dropped
/// once empty, its directory with them. A key no document holds any more
/// and whose list is empty is forgotten.
fn take_out(connection: &Connection, key: i64, docs: &[u32]) -> Result<()> {
    let gone: HashSet<u32> = docs.iter().copied().collect();
    let left = match read_list(connection, key)? {
        Stored::Blocks(directory) => take_out_of_blocks(connection, key, decode_directory(&directory)?, docs, &gone)?,
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

/// Takes `docs` out of the blocks that may hold them; says how many blocks
/// the list keeps.
fn take_out_of_blocks(
    connection: &Connection,
    key: i64,
    mut entries: Vec<Entry>,
    docs: &[u32],
    gone: &HashSet<u32>,
) -> Result<usize> {
    let mut touched: Vec<usize> = docs
        .iter()
        .filter_map(|doc| {
            let at = entries.partition_point(|entry| entry.first <= *doc).checked_sub(1)?;
            (*doc <= entries[at].last).then_some(at)
        })
        .collect();
    touched.dedup();
    for at in touched.into_iter().rev() {
        let first = entries[at].first;
        let held = read_block(connection, key, first)?;
        let kept: Vec<Posting> = held
            .iter()
            .filter(|(doc, _)| !gone.contains(doc))
            .map(|&(doc, tf)| Posting { doc, tf, len: entries[at].least_len })
            .collect();
        if kept.len() == held.len() {
            continue;
        }
        execute(connection, DROP_BLOCK, (key, first), "drop a block")?;
        if kept.is_empty() {
            entries.remove(at);
            continue;
        }
        // Lengths are not read again: the block keeps its least as a bound.
        let bound = entry_of(&kept);
        entries[at] = Entry { most_tf: bound.most_tf, least_len: entries[at].least_len, ..bound };
        execute(connection, WRITE_BLOCK, (key, kept[0].doc, pack(&kept)), "write a block")?;
    }
    if !entries.is_empty() {
        execute(connection, WRITE_LIST, (key, encode_directory(&entries), None::<Vec<u8>>), "write a list")?;
    }
    Ok(entries.len())
}
