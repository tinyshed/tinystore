//! Segments: an index's postings as runs of its documents, each segment
//! every key's postings of its documents in one piece a key, laid one after
//! another, as Lucene and SQLite's FTS5 keep theirs.
//!
//! ```text
//! segments (id)           → index, level, first, past, docs, folded, merge, through
//! pieces   (segment, key) → count, postings or directory, best
//! runs     (id)           → a long piece's blocks, the segment's id × 2²⁴ + the run's number
//! ```
//!
//! A set writes the newest segment, level 0, while it holds fewer than a
//! thousand documents, else a new one: its pieces lie together, so a set
//! writes as many pages as its postings fill. A key's list kept whole in
//! one place made every set write a page a key it held: on MS MARCO 300,000
//! passages wrote 11.8 GB, set and packed a thousand a commit, where FTS5
//! builds all 8.8 million in minutes. Merges join `MERGE_AT` segments of a
//! level into one of the next, in steps, so a posting is written again once
//! a level; a search reads a key's piece in each segment, in the order of
//! their documents, as one list.
//!
//! How many documents hold a key is its row's `documents` and the counts of
//! its pieces in segments not folded yet, levels 0 and 1, which a search
//! reads beside them: a set counts in its piece and writes no key's row, a
//! merge to level 2 or above folds its pieces' counts into their keys' rows,
//! in the keys' order. A delete takes one from the key's row at once.

use rusqlite::{Connection, OptionalExtension};

use super::packed::{Directory, decode_best};
use super::postings::{Posting, pack_piece, unpack_piece};
use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

/// Documents a set writes into the newest segment till it holds as many,
/// rather than opening another: a set of fewer, one row of a table at a
/// time, appends to it.
pub(crate) const OPEN_DOCS: u64 = 1000;
/// Segments of a level a merge joins into one of the next.
pub(crate) const MERGE_AT: usize = 8;
/// The level from which a segment's counts are in its keys' rows.
pub(crate) const FOLDED_FROM: i64 = 2;
/// The level of the segment a store's lists of a release before 0002 are
/// written into: above any merge's, which never reaches it.
pub(crate) const BASE: i64 = 63;
/// A run's number among its segment's is the low bits of its id.
const RUN_BITS: u32 = 24;

const READ_SEGMENTS: &str = "select id, level, first, past, docs, folded, merge, through
    from _tinystore_search_segments where index_id = ?1 order by first, id";
const READ_NEWEST: &str = "select id, level, first, past, docs, folded, merge, through
    from _tinystore_search_segments where index_id = ?1 order by first desc, id desc limit 1";
const ADD_SEGMENT: &str = "insert into _tinystore_search_segments (index_id, level, first, past, docs, folded)
    values (?1, ?2, ?3, ?4, ?5, ?6)";
const GROW_SEGMENT: &str = "update _tinystore_search_segments set past = ?2, docs = docs + ?3 where id = ?1";
const READ_PIECE: &str = "select count, postings, directory, best from _tinystore_search_pieces
    where segment = ?1 and key = ?2";
// SQLite joins two blobs as text; the cast keeps their bytes, a blob.
const ADD_PIECE: &str = "insert into _tinystore_search_pieces (segment, key, count, postings) values (?1, ?2, ?3, ?4)
    on conflict (segment, key) do update set count = count + excluded.count,
    postings = cast(postings || excluded.postings as blob)";
/// Pieces a statement of a set writes, four values each.
const PIECES: usize = 64;
static ADD_PIECES: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let rows = vec!["(?, ?, ?, ?)"; PIECES].join(", ");
    format!(
        "insert into _tinystore_search_pieces (segment, key, count, postings) values {rows}
        on conflict (segment, key) do update set count = count + excluded.count,
        postings = cast(postings || excluded.postings as blob)"
    )
});
const WRITE_PIECE: &str =
    "insert or replace into _tinystore_search_pieces (segment, key, count, postings, directory, best)
    values (?1, ?2, ?3, ?4, ?5, ?6)";
const DROP_PIECE: &str = "delete from _tinystore_search_pieces where segment = ?1 and key = ?2";

/// A segment of an index, as its row says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub(crate) id: i64,
    pub(crate) level: i64,
    /// Its documents are numbered from `first` to before `past`.
    pub(crate) first: u32,
    pub(crate) past: u32,
    pub(crate) docs: u64,
    /// Its pieces' counts are in their keys' rows.
    pub(crate) folded: bool,
    /// The segment a merge under way writes its keys into.
    pub(crate) merge: Option<i64>,
    /// Of a segment a merge writes, the last key it holds, while it writes.
    pub(crate) through: Option<i64>,
}

impl Segment {
    fn of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
        Ok(Segment {
            id: row.get(0)?,
            level: row.get(1)?,
            first: row.get(2)?,
            past: row.get(3)?,
            docs: row.get::<_, i64>(4)?.max(0) as u64,
            folded: row.get(5)?,
            merge: row.get(6)?,
            through: row.get(7)?,
        })
    }

    /// Whether a set may still write into it: the newest of an index, of
    /// level 0, no merge's, holding fewer than [`OPEN_DOCS`].
    fn open(&self) -> bool {
        self.level == 0 && self.merge.is_none() && self.docs < OPEN_DOCS
    }

    /// The ids its runs may take, its own × 2²⁴ onward.
    pub(crate) fn runs(&self) -> std::ops::Range<i64> {
        self.id << RUN_BITS..(self.id + 1) << RUN_BITS
    }
}

/// An index's segments in the order of their documents.
pub(crate) fn of_index(connection: &Connection, index: i64) -> Result<Vec<Segment>> {
    let failed = |error| sql_error("read an index's segments", error);
    let mut select = connection.prepare_cached(READ_SEGMENTS).map_err(failed)?;
    let rows = select.query_map([index], Segment::of);
    rows.and_then(Iterator::collect).map_err(failed)
}

/// A key's postings in a segment: its own, when it holds few, or its
/// blocks' directory and its best.
#[derive(Clone, Debug)]
pub(crate) enum Piece {
    Short(Vec<Posting>),
    Packed(Directory, Vec<Posting>),
}

/// A key's piece in `segment`, and how many postings it holds.
pub(crate) fn piece(connection: &Connection, segment: &Segment, key: i64) -> Result<Option<(i64, Piece)>> {
    let failed = |error| sql_error("read a piece", error);
    let mut select = connection.prepare_cached(READ_PIECE).map_err(failed)?;
    let row = select.query_row((segment.id, key), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?, row.get::<_, Option<Vec<u8>>>(2)?, row.get(3)?))
    });
    let Some((count, postings, directory, best)) = row.optional().map_err(failed)? else { return Ok(None) };
    piece_of(segment, (postings, directory, best)).map(|piece| Some((count, piece)))
}

/// A piece's row's bytes: its postings, or its directory and its best.
pub(crate) type PieceBytes = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

/// A piece of its row's bytes in `segment`.
pub(crate) fn piece_of(segment: &Segment, (postings, directory, best): PieceBytes) -> Result<Piece> {
    let within = |error: Error| error.within(format!("segment {}", segment.id));
    match (postings, directory) {
        (Some(postings), None) => Ok(Piece::Short(unpack_piece(&postings, segment.first).map_err(within)?)),
        (None, Some(directory)) => {
            let best = decode_best(&best.unwrap_or_default()).map_err(within)?;
            Ok(Piece::Packed(Directory::decode(&directory).map_err(within)?, best))
        }
        _ => Err(within(Error::corrupt("a piece holds neither postings nor a directory"))),
    }
}

/// How many documents hold a key whose row counts `documents`: those and
/// what its pieces in segments not folded count.
pub(crate) fn holding(connection: &Connection, segments: &[Segment], key: i64, documents: i64) -> Result<u64> {
    let mut holding = documents;
    for segment in segments.iter().filter(|segment| !segment.folded) {
        holding += counted(connection, segment, key)?;
    }
    Ok(holding.max(0) as u64)
}

fn counted(connection: &Connection, segment: &Segment, key: i64) -> Result<i64> {
    let failed = |error| sql_error("read a piece", error);
    let mut select = connection.prepare_cached(READ_PIECE).map_err(failed)?;
    let count = select.query_row((segment.id, key), |row| row.get::<_, i64>(0)).optional().map_err(failed)?;
    Ok(count.unwrap_or(0))
}

/// Writes a set's postings, `(key, doc, tf, len)`, of its documents from
/// `first` to before `past`, into the open segment or a new one: a piece a
/// key, many a statement, in the order of the segment's keys. Empties
/// `postings`.
pub(crate) fn write_set(
    tx: &Tx<'_>,
    index: i64,
    (first, past, docs): (u32, u32, u64),
    postings: &mut Vec<(i64, u32, u32, u32)>,
) -> Result<()> {
    let segment = open(tx, index, (first, past, docs))?;
    postings.sort_unstable();
    let rows: Vec<(i64, i64, Vec<u8>)> = postings
        .chunk_by(|a, b| a.0 == b.0)
        .map(|run| {
            let held: Vec<Posting> = run.iter().map(|&(_, doc, tf, len)| Posting { doc, tf, len }).collect();
            (run[0].0, run.len() as i64, pack_piece(&held, segment.first))
        })
        .collect();
    let failed = |error| sql_error("write postings", error);
    let mut many = tx.prepare_cached(&ADD_PIECES).map_err(failed)?;
    let (chunks, rest) = rows.as_chunks::<PIECES>();
    for chunk in chunks {
        let values: Vec<&dyn rusqlite::ToSql> = chunk
            .iter()
            .flat_map(|(key, count, bytes)| [&segment.id as &dyn rusqlite::ToSql, key, count, bytes])
            .collect();
        many.execute(values.as_slice()).map_err(failed)?;
    }
    for (key, count, bytes) in rest {
        execute(tx, ADD_PIECE, (segment.id, key, count, bytes), "write postings")?;
    }
    postings.clear();
    Ok(())
}

/// The segment a set of `docs` documents from `first` to before `past`
/// writes into: the newest while it is open, grown to hold them, else a
/// new one of level 0.
fn open(tx: &Tx<'_>, index: i64, (first, past, docs): (u32, u32, u64)) -> Result<Segment> {
    if let Some(newest) = open_of(tx, index)? {
        execute(tx, GROW_SEGMENT, (newest.id, past, docs as i64), "grow a segment")?;
        return Ok(Segment { past, docs: newest.docs + docs, ..newest });
    }
    add(tx, index, (0, first, past), docs)
}

/// The newest segment of an index, while a set may still write into it.
pub(crate) fn open_of(connection: &Connection, index: i64) -> Result<Option<Segment>> {
    let failed = |error| sql_error("read the newest segment", error);
    let newest = connection
        .prepare_cached(READ_NEWEST)
        .and_then(|mut select| select.query_row([index], Segment::of).optional())
        .map_err(failed)?;
    Ok(newest.filter(Segment::open))
}

/// Takes `doc` out of a key's short piece in `segment`, the piece dropped
/// once empty; says whether it held the document.
pub(crate) fn take_out(connection: &Connection, segment: &Segment, key: i64, doc: u32) -> Result<bool> {
    let Some((count, Piece::Short(mut postings))) = piece(connection, segment, key)? else { return Ok(false) };
    let Ok(at) = postings.binary_search_by_key(&doc, |posting| posting.doc) else { return Ok(false) };
    postings.remove(at);
    if postings.is_empty() {
        drop_piece(connection, segment, key)?;
    } else {
        write_piece(connection, (segment, key), count - 1, &Written::Short(postings))?;
    }
    Ok(true)
}

/// Adds a segment of `level` for documents from `first` to before `past`.
pub(crate) fn add(tx: &Tx<'_>, index: i64, (level, first, past): (i64, u32, u32), docs: u64) -> Result<Segment> {
    let folded = level >= FOLDED_FROM;
    execute(tx, ADD_SEGMENT, (index, level, first, past, docs as i64, folded), "add a segment")?;
    let id = tx.last_insert_rowid();
    Ok(Segment { id, level, first, past, docs, folded, merge: None, through: None })
}

/// Writes a key's piece in `segment`, `count` postings, in place of any.
pub(crate) fn write_piece(
    connection: &Connection,
    (segment, key): (&Segment, i64),
    count: i64,
    written: &Written,
) -> Result<()> {
    let row = match written {
        Written::Short(postings) => (Some(pack_piece(postings, segment.first)), None, None),
        Written::Packed(directory, best) => (None, Some(directory.encode()), Some(best.clone())),
    };
    execute(connection, WRITE_PIECE, (segment.id, key, count, row.0, row.1, row.2), "write a piece").map(drop)
}

/// A piece as it is written: its postings, or its directory and its best's bytes.
pub(crate) enum Written {
    Short(Vec<Posting>),
    Packed(Directory, Vec<u8>),
}

pub(crate) fn drop_piece(connection: &Connection, segment: &Segment, key: i64) -> Result<()> {
    execute(connection, DROP_PIECE, (segment.id, key), "drop a piece").map(drop)
}
