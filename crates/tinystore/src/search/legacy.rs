//! Lists as search.db held them from v0.1.0 to v0.5.0, before its
//! migration 0002: nothing writes them now. A search reads one as it is,
//! and packing, a purge or upkeep writes a key's list again in runs, whole,
//! at the first chance, so that the format leaves a file soon after a
//! release opens it. Nothing outside this file reads its bytes.
//!
//! ```text
//! lists  (key)        → directory
//! blocks (key, first) → payload
//!
//! directory   (first − last before, last − first, most tf, least len) a block    varints
//! payload     (doc − doc before, tf) a posting; its length is in norms           varints
//! ```

use rusqlite::{Connection, OptionalExtension};

use super::execute::List;
use super::pack::{Runs, average_of};
use super::packed::bound_of;
use super::postings::{BLOCK, Posting, after, frequency, number};
use crate::compress::varint::Reader;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_BLOCK: &str = "select payload from _tinystore_search_blocks where key = ?1 and first = ?2";
const READ_BLOCKS: &str = "select first, payload from _tinystore_search_blocks where key = ?1 order by first";
const DROP_BLOCKS: &str = "delete from _tinystore_search_blocks where key = ?1";
const READ_DIRECTORY: &str = "select directory from _tinystore_search_lists where key = ?1";
const READ_INDEX_OF: &str = "select index_id from _tinystore_search_keys where id = ?1";
const NEXT_KEY: &str = "select key from _tinystore_search_blocks order by key limit 1";
const ANY_BLOCK: &str = "select exists (select 1 from _tinystore_search_blocks)";

/// Rows of blocks a step of upkeep writes again in runs at most, unless its
/// one key holds more: a step holds the writer about as long as packing's.
pub(crate) const CONVERT_BLOCKS: usize = 4096;

/// A block as its key's directory says it: its documents' range and what
/// bounds what it adds to any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) most_tf: u32,
    pub(crate) least_len: u32,
}

/// A key's list as a release before 0002 wrote it: its directory.
#[derive(Debug)]
pub(crate) struct Legacy {
    key: i64,
    entries: Vec<Entry>,
}

impl Legacy {
    pub(crate) fn decode(key: i64, directory: &[u8]) -> Result<Legacy> {
        let mut reader = Reader::new(directory);
        let mut entries = Vec::with_capacity(directory.len() / 4);
        let mut before = 0;
        while !reader.remaining().is_empty() {
            let first = after(&mut reader, before, "a directory's blocks out of order");
            let last = first.saturating_add(number(&mut reader));
            let (most_tf, least_len) = (number(&mut reader), number(&mut reader));
            entries.push(Entry { first, last, most_tf, least_len });
            before = last;
        }
        reader.finish().map_err(|error| error.within(format!("key {key}")))?;
        Ok(Legacy { key, entries })
    }

    /// Its blocks onto a search's list, each bounded by its largest frequency
    /// over its shortest length.
    pub(crate) fn onto(&self, list: &mut List) {
        for entry in &self.entries {
            list.push(list.stored(entry.first, entry.last, entry.most_tf, entry.least_len));
        }
    }

    /// Written again in runs, its rows of blocks dropped. Lengths are not
    /// read: each block is bounded at `average` as its entry bounded it, by
    /// its largest frequency over its shortest length, which no posting of
    /// it passes.
    pub(crate) fn into_runs(self, connection: &Connection, average: u16) -> Result<Runs> {
        let key = self.key;
        let mut list = Runs::new(key)?;
        let blocks = blocks(connection, key)?;
        if blocks.len() != self.entries.len() || blocks.iter().zip(&self.entries).any(|(at, entry)| at.0 != entry.first)
        {
            return Err(Error::corrupt(format!("key {key}: a block its directory names is missing")));
        }
        for ((_, pairs), entry) in blocks.into_iter().zip(&self.entries) {
            let block: Vec<Posting> = pairs.into_iter().map(|(doc, tf)| Posting { doc, tf, len: 0 }).collect();
            let most = Posting { doc: entry.first, tf: entry.most_tf, len: entry.least_len };
            list.push(connection, &block, bound_of(&[most], average), average)?;
        }
        execute(connection, DROP_BLOCKS, key, "drop a key's rows of blocks")?;
        Ok(list)
    }
}

/// The documents and frequencies of the block of `key` that starts at `first`.
pub(crate) fn block(connection: &Connection, key: i64, first: u32) -> Result<Vec<(u32, u32)>> {
    let failed = |error| sql_error("read a block", error);
    let payload: Option<Vec<u8>> = connection
        .prepare_cached(READ_BLOCK)
        .and_then(|mut select| select.query_row((key, first), |row| row.get(0)).optional())
        .map_err(failed)?;
    let missing = || Error::corrupt(format!("block at document {first} its directory names is missing"));
    unpack(&payload.ok_or_else(missing)?)
}

/// Every document a key's rows of blocks hold, in order.
pub(crate) fn documents(connection: &Connection, key: i64) -> Result<Vec<u32>> {
    let blocks = blocks(connection, key)?;
    Ok(blocks.into_iter().flat_map(|(_, pairs)| pairs.into_iter().map(|(doc, _)| doc)).collect())
}

/// Whether search.db holds a list a release before 0002 wrote.
pub(crate) fn any(connection: &Connection) -> Result<bool> {
    let failed = |error| sql_error("look for rows of blocks", error);
    connection.prepare_cached(ANY_BLOCK).and_then(|mut select| select.query_row([], |row| row.get(0))).map_err(failed)
}

/// Writes again in runs the next lists a release before 0002 wrote, of any
/// index, `most_blocks` rows of blocks at most unless the first key holds
/// more, so that a key no write reaches again is not left so for good. Says
/// whether any is left.
pub(crate) fn convert_step(tx: &Tx<'_>, most_blocks: usize) -> Result<bool> {
    let mut converted = 0;
    while converted < most_blocks {
        let Some(key) = next_key(tx)? else { return Ok(false) };
        let within = |error: Error| error.within(format!("key {key}"));
        let failed = |error| sql_error("read a key's list", error);
        let directory: Option<Vec<u8>> = tx
            .prepare_cached(READ_DIRECTORY)
            .and_then(|mut select| select.query_row([key], |row| row.get(0)).optional())
            .map_err(failed)?
            .flatten();
        let directory =
            directory.ok_or_else(|| Error::corrupt(format!("key {key}: rows of blocks no directory names")))?;
        let index: i64 = tx
            .prepare_cached(READ_INDEX_OF)
            .and_then(|mut select| select.query_row([key], |row| row.get(0)))
            .map_err(failed)?;
        let legacy = Legacy::decode(key, &directory)?;
        converted += legacy.entries.len();
        let average = average_of(tx, index, key).map_err(within)?;
        legacy.into_runs(tx, average).and_then(|list| list.finish(tx)).map_err(within)?;
    }
    Ok(next_key(tx)?.is_some())
}

fn next_key(connection: &Connection) -> Result<Option<i64>> {
    let failed = |error| sql_error("find a key's rows of blocks", error);
    let key =
        connection.prepare_cached(NEXT_KEY).and_then(|mut select| select.query_row([], |row| row.get(0)).optional());
    key.map_err(failed)
}

/// A block's documents and their frequencies.
type Pairs = Vec<(u32, u32)>;

/// A key's blocks, each with the document it starts at.
fn blocks(connection: &Connection, key: i64) -> Result<Vec<(u32, Pairs)>> {
    let failed = |error| sql_error("read a key's blocks", error);
    let mut select = connection.prepare_cached(READ_BLOCKS).map_err(failed)?;
    let rows = select.query_map([key], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?)));
    let rows: Vec<(u32, Vec<u8>)> = rows.and_then(Iterator::collect).map_err(failed)?;
    let read = |(first, payload): (u32, Vec<u8>)| {
        unpack(&payload).map(|pairs| (first, pairs)).map_err(|error| error.within(format!("block at document {first}")))
    };
    rows.into_iter().map(read).collect()
}

fn execute(connection: &Connection, sql: &str, key: i64, what: &str) -> Result<()> {
    connection
        .prepare_cached(sql)
        .and_then(|mut statement| statement.execute([key]))
        .map_err(|error| sql_error(what, error))?;
    Ok(())
}

/// A block's documents and frequencies, as they were written.
pub(crate) fn unpack(payload: &[u8]) -> Result<Vec<(u32, u32)>> {
    let mut reader = Reader::new(payload);
    let mut out = Vec::with_capacity(BLOCK);
    let mut doc = 0;
    while !reader.remaining().is_empty() {
        doc = after(&mut reader, doc, "a block's documents out of order");
        out.push((doc, frequency(&mut reader)));
    }
    reader.finish()?;
    Ok(out)
}

/// How a release before 0002 wrote a block's entry, its directory and its
/// payload, which tests write such lists with, and an index's lists written
/// so again.
#[cfg(test)]
pub(crate) mod written {
    use std::collections::HashMap;
    use std::path::Path;

    use super::Entry;
    use crate::compress::varint::put_count;
    use crate::search::kept::{NORMS, lengths_of};
    use crate::search::packed::{self, Directory};
    use crate::search::postings::Posting;

    /// Writes every list of the index `name` that is in runs again as rows
    /// of blocks, as a release before 0002 wrote them, beside the store.
    pub(crate) fn rewrite(dir: &Path, name: &str) {
        let connection = rusqlite::Connection::open(dir.join("search.db")).unwrap();
        let index: i64 = connection
            .query_row("select id from _tinystore_search_indexes where name = ?1", [name], |row| row.get(0))
            .unwrap();
        let mut select = connection
            .prepare(
                "select k.id, k.part, l.packed from _tinystore_search_keys as k join _tinystore_search_lists as l
                on l.key = k.id where k.index_id = ?1 and l.packed is not null",
            )
            .unwrap();
        let lists: Vec<(i64, u32, Vec<u8>)> = select
            .query_map([index], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(!lists.is_empty(), "the index has lists in runs");
        let mut rows: HashMap<(u32, u32), Vec<u16>> = HashMap::new();
        let mut length = |part: u32, doc: u32| -> u32 {
            let row = rows.entry((part, doc / NORMS)).or_insert_with(|| {
                let sql =
                    "select lengths from _tinystore_search_norms where index_id = ?1 and part = ?2 and chunk = ?3";
                lengths_of(
                    &connection.query_row(sql, (index, part, doc / NORMS), |row| row.get::<_, Vec<u8>>(0)).unwrap(),
                )
            });
            u32::from(row[(doc % NORMS) as usize])
        };
        // one transaction, or each of a large index's rows would wait for its own sync
        connection.execute_batch("begin").unwrap();
        let mut pairs = Vec::new();
        for (key, part, packed) in lists {
            let runs = Directory::decode(&packed).unwrap();
            let mut entries = Vec::new();
            for (place, run) in runs.runs.iter().enumerate() {
                let bytes: Vec<u8> = connection
                    .query_row("select blocks from _tinystore_search_runs where id = ?1", [run.id], |row| row.get(0))
                    .unwrap();
                for at in runs.blocks_of(place) {
                    packed::unpack(&bytes[runs.bytes_of(at, bytes.len())], runs.blocks[at].first, &mut pairs).unwrap();
                    let block: Vec<Posting> =
                        pairs.iter().map(|&(doc, tf)| Posting { doc, tf, len: length(part, doc) }).collect();
                    entries.push(entry_of(&block));
                    let insert = "insert into _tinystore_search_blocks (key, first, payload) values (?1, ?2, ?3)";
                    connection.execute(insert, (key, block[0].doc, payload(&block))).unwrap();
                }
                connection.execute("delete from _tinystore_search_runs where id = ?1", [run.id]).unwrap();
            }
            let update = "update _tinystore_search_lists set directory = ?2, packed = null where key = ?1";
            connection.execute(update, (key, directory(&entries))).unwrap();
        }
        connection.execute_batch("commit").unwrap();
    }

    /// The rows of blocks search.db holds.
    pub(crate) fn blocks(dir: &Path) -> i64 {
        let connection = rusqlite::Connection::open(dir.join("search.db")).unwrap();
        connection.query_row("select count(*) from _tinystore_search_blocks", [], |row| row.get(0)).unwrap()
    }

    pub(crate) fn entry_of(block: &[Posting]) -> Entry {
        Entry {
            first: block.first().map_or(0, |posting| posting.doc),
            last: block.last().map_or(0, |posting| posting.doc),
            most_tf: block.iter().map(|posting| posting.tf).max().unwrap_or(0),
            least_len: block.iter().map(|posting| posting.len).min().unwrap_or(0),
        }
    }

    pub(crate) fn directory(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::with_capacity(entries.len() * 6);
        let mut before = 0;
        for entry in entries {
            put_count(&mut out, u64::from(entry.first - before));
            put_count(&mut out, u64::from(entry.last - entry.first));
            put_count(&mut out, u64::from(entry.most_tf));
            put_count(&mut out, u64::from(entry.least_len));
            before = entry.last;
        }
        out
    }

    pub(crate) fn payload(block: &[Posting]) -> Vec<u8> {
        let mut out = Vec::with_capacity(block.len() * 3);
        let mut before = 0;
        for posting in block {
            put_count(&mut out, u64::from(posting.doc - before));
            put_count(&mut out, u64::from(posting.tf));
            before = posting.doc;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posting(doc: u32, tf: u32, len: u32) -> Posting {
        Posting { doc, tf, len }
    }

    /// A block of postings at documents 3, 300 and 70,000 of frequencies 1,
    /// 4 and 2, the shortest of length 5, and its directory, as v0.5.0
    /// (4390117b) wrote them.
    const BLOCK_OF_V0_5: &str = "0301a90204c4a00402";
    const DIRECTORY_OF_V0_5: &str = "03eda2040405";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn what_tests_write_is_what_v0_5_wrote() {
        let block = [posting(3, 1, 9), posting(300, 4, 5), posting(70_000, 2, 6)];
        assert_eq!(hex(&written::payload(&block)), BLOCK_OF_V0_5);
        assert_eq!(hex(&written::directory(&[written::entry_of(&block)])), DIRECTORY_OF_V0_5);
        let read = Legacy::decode(1, &written::directory(&[written::entry_of(&block)])).unwrap();
        assert_eq!(read.entries, [Entry { first: 3, last: 70_000, most_tf: 4, least_len: 5 }]);
    }

    #[test]
    fn a_directory_and_a_block_read_back_as_a_release_wrote_them() {
        let entries = [
            Entry { first: 4, last: 900, most_tf: 3, least_len: 7 },
            Entry { first: 901, last: 70_000, most_tf: 1, least_len: 2 },
        ];
        assert_eq!(Legacy::decode(1, &written::directory(&entries)).unwrap().entries, entries);
        let block = [posting(3, 1, 9), posting(300, 4, 120), posting(70_000, 2, 5)];
        assert_eq!(unpack(&written::payload(&block)).unwrap(), [(3, 1), (300, 4), (70_000, 2)]);
    }

    #[test]
    fn bytes_that_break_an_invariant_say_which() {
        let twice = [posting(5, 1, 1), posting(5, 1, 1)];
        let broken = |error: crate::Error| error.to_string();
        assert_eq!(broken(unpack(&written::payload(&twice)).unwrap_err()), "a block's documents out of order: corrupt");
        assert_eq!(broken(unpack(&[1, 0]).unwrap_err()), "a posting of no occurrences: corrupt");
        assert_eq!(broken(unpack(&[1]).unwrap_err()), "a row ends inside a value: corrupt");
    }
}
