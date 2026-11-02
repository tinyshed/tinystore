//! Lists as search.db held them from v0.1.0 to v0.5.0, before its
//! migration 0002: nothing writes them now. A search reads one as it is,
//! and upkeep writes a key's list and its head again as its piece of the
//! index's base segment, whole, a step of keys a commit, so that the format
//! leaves a file soon after a release opens it. Nothing outside this file
//! reads its bytes.
//!
//! The head was a row a posting, which a search reads after the key's list:
//! its documents were all set after the list's, and before any segment's.
//!
//! ```text
//! lists  (key)        → directory, or inline
//! blocks (key, first) → payload
//! head   (index_id, key, doc) → tf, len
//!
//! directory   (first − last before, last − first, most tf, least len) a block    varints
//! payload     (doc − doc before, tf) a posting; its length is in norms           varints
//! inline      (doc − doc before, tf, len) a posting, a key in at most 16 docs   varints
//! ```

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use super::execute::List;
use super::kept::Listed;
use super::merge::{self, Reading, Writer};
use super::postings::{BLOCK, Posting, after, frequency, number, unpack_inline};
use super::segments::{self, BASE, Segment};
use crate::compress::varint::Reader;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_BLOCK: &str = "select payload from _tinystore_search_blocks where key = ?1 and first = ?2";
const READ_BLOCKS: &str = "select first, payload from _tinystore_search_blocks where key = ?1 order by first";
const DROP_BLOCKS: &str = "delete from _tinystore_search_blocks where key = ?1";
const READ_LIST: &str = "select directory, inline from _tinystore_search_lists where key = ?1";
const DROP_LIST: &str = "delete from _tinystore_search_lists where key = ?1";
const READ_INDEX_OF: &str = "select index_id from _tinystore_search_keys where id = ?1";
const NEXT_LIST: &str = "select key from _tinystore_search_lists order by key limit 1";
const NEXT_HEAD: &str = "select index_id, key from _tinystore_search_head order by index_id, key limit 1";
const ANY: &str =
    "select exists (select 1 from _tinystore_search_lists) or exists (select 1 from _tinystore_search_head)";
const READ_HEAD: &str = "select doc, tf, len from _tinystore_search_head where index_id = ?1 and key = ?2 order by doc";
const DROP_HEAD: &str = "delete from _tinystore_search_head where index_id = ?1 and key = ?2";
const DROP_POSTING: &str = "delete from _tinystore_search_head where index_id = ?1 and key = ?2 and doc = ?3";
const READ_NEXT_DOC: &str = "select next_doc from _tinystore_search_indexes where id = ?1";

/// Postings a step of upkeep writes again at most, unless its one key holds
/// more: a step holds the writer about as long as a merge's.
pub(crate) const CONVERT_POSTINGS: usize = 1 << 19;

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
        Ok(Legacy { entries })
    }

    /// How many blocks it names.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Its blocks onto a search's list, each bounded by its largest frequency
    /// over its shortest length.
    pub(crate) fn onto(&self, list: &mut List) {
        for entry in &self.entries {
            list.push(list.stored(entry.first, entry.last, entry.most_tf, entry.least_len));
        }
    }
}

/// A key's list as a release before 0002 wrote it: its directory, its
/// postings in its own row, or none.
pub(crate) fn list(connection: &Connection, key: i64) -> Result<Listed> {
    let failed = |error| sql_error("read a list", error);
    let mut select = connection.prepare_cached(READ_LIST).map_err(failed)?;
    let mut rows = select.query([key]).map_err(failed)?;
    let Some(row) = rows.next().map_err(failed)? else { return Ok(Listed::Nothing) };
    let (directory, inline): (Option<Vec<u8>>, Option<Vec<u8>>) =
        (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?);
    Ok(match (directory, inline) {
        (Some(directory), _) => Listed::Legacy(Legacy::decode(key, &directory)?),
        (None, Some(inline)) => Listed::Short(unpack_inline(&inline)?),
        (None, None) => Listed::Nothing,
    })
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

/// A key's postings in the head a release before 0002 kept.
pub(crate) fn head(connection: &Connection, index: i64, key: i64) -> Result<Vec<Posting>> {
    let failed = |error| sql_error(format!("read key {key}'s head"), error);
    let mut select = connection.prepare_cached(READ_HEAD).map_err(failed)?;
    let rows =
        select.query_map((index, key), |row| Ok(Posting { doc: row.get(0)?, tf: row.get(1)?, len: row.get(2)? }));
    rows.and_then(Iterator::collect).map_err(failed)
}

/// Takes the documents `gone` out of a key's postings in the old head.
pub(crate) fn take_out_of_head(connection: &Connection, index: i64, key: i64, gone: &HashSet<u32>) -> Result<()> {
    let failed = |error| sql_error("take postings out of the head", error);
    let mut delete = connection.prepare_cached(DROP_POSTING).map_err(failed)?;
    for doc in gone {
        delete.execute((index, key, doc)).map_err(failed)?;
    }
    Ok(())
}

/// Every document a key's list and its head hold, in order.
pub(crate) fn documents(connection: &Connection, index: i64, key: i64) -> Result<Vec<u32>> {
    let mut docs: Vec<u32> = match list(connection, key)? {
        Listed::Legacy(_) => {
            blocks(connection, key)?.into_iter().flat_map(|(_, pairs)| pairs).map(|(doc, _)| doc).collect()
        }
        Listed::Short(postings) => postings.iter().map(|posting| posting.doc).collect(),
        Listed::Packed(..) | Listed::Nothing => Vec::new(),
    };
    docs.extend(head(connection, index, key)?.iter().map(|posting| posting.doc));
    Ok(docs)
}

/// Whether search.db holds a list or a head a release before 0002 wrote.
pub(crate) fn any(connection: &Connection) -> Result<bool> {
    let failed = |error| sql_error("look for lists of a release before", error);
    connection.prepare_cached(ANY).and_then(|mut select| select.query_row([], |row| row.get(0))).map_err(failed)
}

/// Writes again the next keys' lists and heads a release before 0002 wrote,
/// of any index, as their pieces of its base segment: `most_postings` at
/// most unless the first key holds more, so that a key no write reaches
/// again is not left so for good. Says whether any is left.
pub(crate) fn convert_step(tx: &Tx<'_>, most_postings: usize) -> Result<bool> {
    let mut converted = 0;
    while converted < most_postings {
        let Some((index, key)) = next_key(tx)? else { return Ok(false) };
        converted += convert(tx, index, key).map_err(|error| error.within(format!("key {key}")))?;
    }
    Ok(next_key(tx)?.is_some())
}

/// Writes a key's list and its head a release before 0002 wrote, if it
/// has either, as its piece of the index's base segment.
pub(crate) fn convert_key(tx: &Tx<'_>, index: i64, key: i64) -> Result<()> {
    if matches!(list(tx, key)?, Listed::Nothing) && head(tx, index, key)?.is_empty() {
        return Ok(());
    }
    convert(tx, index, key).map(drop)
}

/// Writes a key's list and its head as its piece of the index's base
/// segment, each block's postings with their lengths, and drops them; says
/// how many postings it wrote.
fn convert(tx: &Tx<'_>, index: i64, key: i64) -> Result<usize> {
    merge::norm(tx, index)?;
    let base = base(tx, index)?;
    let mut reading = Reading::of(index);
    let (part, average) = reading.averages.of_key(tx, key)?;
    let mut writer = Writer::new(base, average, merge::next_run(tx, &base)?);
    match list(tx, key)? {
        Listed::Legacy(_) => {
            for (_, pairs) in blocks(tx, key)? {
                for (doc, tf) in pairs {
                    writer.push(Posting { doc, tf, len: reading.lengths.of(tx, part, doc)? })?;
                }
            }
        }
        Listed::Short(postings) => postings.into_iter().try_for_each(|posting| writer.push(posting))?,
        Listed::Packed(..) | Listed::Nothing => {}
    }
    head(tx, index, key)?.into_iter().try_for_each(|posting| writer.push(posting))?;
    let (count, written, runs, _) = writer.finish()?;
    runs.iter().try_for_each(|(id, run)| merge::write_run(tx, *id, run))?;
    if let Some(written) = written {
        segments::write_piece(tx, (&base, key), count, &written)?;
    }
    execute(tx, DROP_BLOCKS, key, "drop a key's rows of blocks")?;
    execute(tx, DROP_LIST, key, "drop a list")?;
    tx.prepare_cached(DROP_HEAD)
        .and_then(|mut delete| delete.execute((index, key)))
        .map_err(|error| sql_error("drop a key's head", error))?;
    merge::note_lists(tx, index)?;
    Ok(count as usize)
}

/// The segment an index's lists of a release before are written into, its
/// first: made the first time, its documents those before every other's.
fn base(tx: &Tx<'_>, index: i64) -> Result<Segment> {
    let segments = segments::of_index(tx, index)?;
    if let Some(base) = segments.iter().find(|segment| segment.level == BASE) {
        return Ok(*base);
    }
    let past = match segments.first() {
        Some(first) => first.first,
        None => tx
            .prepare_cached(READ_NEXT_DOC)
            .and_then(|mut select| select.query_row([index], |row| row.get(0)))
            .map_err(|error| sql_error("read an index", error))?,
    };
    segments::add(tx, index, (BASE, 1, past), 0)
}

/// The next key a release before 0002 left a list or a head of, with its index.
fn next_key(connection: &Connection) -> Result<Option<(i64, i64)>> {
    let failed = |error| sql_error("find a list of a release before", error);
    let list: Option<i64> = connection
        .prepare_cached(NEXT_LIST)
        .and_then(|mut select| select.query_row([], |row| row.get(0)).optional())
        .map_err(failed)?;
    if let Some(key) = list {
        let index = connection
            .prepare_cached(READ_INDEX_OF)
            .and_then(|mut select| select.query_row([key], |row| row.get(0)))
            .map_err(failed)?;
        return Ok(Some((index, key)));
    }
    let head = connection
        .prepare_cached(NEXT_HEAD)
        .and_then(|mut select| select.query_row([], |row| Ok((row.get(0)?, row.get(1)?))).optional());
    head.map_err(failed)
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
/// payload, which tests write such lists with, and an index's segments
/// written so again.
#[cfg(test)]
pub(crate) mod written {
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;

    use super::Entry;
    use crate::compress::varint::put_count;
    use crate::search::merge::{Lengths, each_posting};
    use crate::search::postings::{BLOCK, INLINE, Posting, pack_inline};
    use crate::search::segments::{self, PieceBytes};

    /// Writes the segments of the index `name` again as a release before
    /// 0002 wrote its lists, beside the store: the pieces of its folded
    /// segments as each key's list, rows of blocks or its own row, and the
    /// others' as its head, a row a posting, counted in their keys' rows.
    pub(crate) fn rewrite(dir: &Path, name: &str) {
        let connection = rusqlite::Connection::open(dir.join("search.db")).unwrap();
        let index: i64 = connection
            .query_row("select id from _tinystore_search_indexes where name = ?1", [name], |row| row.get(0))
            .unwrap();
        let segments = segments::of_index(&connection, index).unwrap();
        assert!(!segments.is_empty(), "the index has segments");
        // one transaction, or each of a large index's rows would wait for its own sync
        connection.execute_batch("begin").unwrap();
        let mut lengths = Lengths::new(index);
        let mut lists: BTreeMap<i64, Vec<Posting>> = BTreeMap::new();
        let mut heads: Vec<(i64, Posting)> = Vec::new();
        let mut counted: HashMap<i64, i64> = HashMap::new();
        for segment in &segments {
            let mut select = connection
                .prepare(
                    "select key, count, postings, directory, best from _tinystore_search_pieces where segment = ?1",
                )
                .unwrap();
            let pieces: Vec<(i64, i64, PieceBytes)> = select
                .query_map([segment.id], |row| Ok((row.get(0)?, row.get(1)?, (row.get(2)?, row.get(3)?, row.get(4)?))))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            for (key, count, bytes) in pieces {
                let part: u32 = connection
                    .query_row("select part from _tinystore_search_keys where id = ?1", [key], |row| row.get(0))
                    .unwrap();
                let piece = segments::piece_of(segment, bytes).unwrap();
                let mut postings = Vec::new();
                each_posting(&connection, &piece, (part, &mut lengths), &mut |posting| {
                    postings.push(posting);
                    Ok(())
                })
                .unwrap();
                if segment.folded {
                    lists.entry(key).or_default().extend(postings);
                } else {
                    heads.extend(postings.into_iter().map(|posting| (key, posting)));
                    *counted.entry(key).or_default() += count;
                }
            }
            let runs = segment.runs();
            connection
                .execute("delete from _tinystore_search_runs where id >= ?1 and id < ?2", (runs.start, runs.end))
                .unwrap();
            connection.execute("delete from _tinystore_search_pieces where segment = ?1", [segment.id]).unwrap();
            connection.execute("delete from _tinystore_search_segments where id = ?1", [segment.id]).unwrap();
        }
        for (key, postings) in lists {
            write_list(&connection, key, &postings);
        }
        for (key, posting) in heads {
            let insert = "insert into _tinystore_search_head (index_id, key, doc, tf, len) values (?1, ?2, ?3, ?4, ?5)";
            connection.execute(insert, (index, key, posting.doc, posting.tf, posting.len)).unwrap();
        }
        for (key, count) in counted {
            let count_key = "update _tinystore_search_keys set documents = documents + ?2 where id = ?1";
            connection.execute(count_key, (key, count)).unwrap();
        }
        // as a file another release wrote: what the store keeps of its lists is behind
        let behind = "update _tinystore_search_indexes set lists_version = lists_version + 1 where id = ?1";
        connection.execute(behind, [index]).unwrap();
        connection.execute_batch("commit").unwrap();
    }

    /// A key's list as a release before 0002 wrote it: its postings in its
    /// own row while they were few, else in rows of blocks and a directory.
    fn write_list(connection: &rusqlite::Connection, key: i64, postings: &[Posting]) {
        let insert = "insert into _tinystore_search_lists (key, directory, inline) values (?1, ?2, ?3)";
        if postings.len() <= INLINE {
            connection.execute(insert, (key, None::<Vec<u8>>, pack_inline(postings))).unwrap();
            return;
        }
        let mut entries = Vec::new();
        for block in postings.chunks(BLOCK) {
            entries.push(entry_of(block));
            let insert = "insert into _tinystore_search_blocks (key, first, payload) values (?1, ?2, ?3)";
            connection.execute(insert, (key, block[0].doc, payload(block))).unwrap();
        }
        connection.execute(insert, (key, directory(&entries), None::<Vec<u8>>)).unwrap();
    }

    /// The rows of blocks and of the old head search.db holds.
    pub(crate) fn rows(dir: &Path) -> (i64, i64) {
        let connection = rusqlite::Connection::open(dir.join("search.db")).unwrap();
        let count =
            |table: &str| connection.query_row(&format!("select count(*) from {table}"), [], |row| row.get(0)).unwrap();
        (count("_tinystore_search_blocks"), count("_tinystore_search_head"))
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
