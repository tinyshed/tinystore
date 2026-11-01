//! Merging: `MERGE_AT` segments of a level joined into one of the next, a
//! step of keys a commit, so that the writer is held for a step and never
//! for the whole merge. A search reads a key's piece in whichever segment
//! holds it: a step writes its keys' pieces into the new segment and drops
//! them from the old ones in one commit, and the old segments go once the
//! last key has moved.
//!
//! A step first gives every document set since the last step its lengths
//! in `norms`: a posting in a block finds its length there, and a
//! document's lengths are written once and never change. A merged piece
//! leaves out the documents gone, and its blocks are packed anew, each
//! bounded at its part's average now; a merge to a folded level adds its
//! pieces' counts to their keys' rows.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};

use super::kept::{NORMS, READ_NORMS, bytes_of, lengths_of};
use super::packed::{self, BEST_AT, Directory, Placed, RUN_BYTES, Run, best_of, bound_of, encode_best, reference};
use super::postings::{BLOCK, INLINE, Posting, decode_lengths};
use super::score::average;
use super::segments::{self, FOLDED_FROM, MERGE_AT, Piece, PieceBytes, Segment, Written};
use super::write::execute;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const READ_NUMBERS: &str = "select next_doc, normed from _tinystore_search_indexes where id = ?1";
const READ_LENGTHS: &str = "select doc, lengths from _tinystore_search_docs where index_id = ?1 and doc >= ?2";
const WRITE_NORMS: &str =
    "insert or replace into _tinystore_search_norms (index_id, part, chunk, lengths) values (?1, ?2, ?3, ?4)";
const SET_NORMED: &str = "update _tinystore_search_indexes set normed = ?2 where id = ?1";
const READ_PART: &str = "select part from _tinystore_search_keys where id = ?1";
const READ_TOKENS: &str = "select p.tokens, i.documents from _tinystore_search_parts as p
    join _tinystore_search_indexes as i on i.id = p.index_id where p.index_id = ?1 and p.part = ?2";
const READ_GONE: &str = "select doc from _tinystore_search_gone where index_id = ?1";
const READ_RUN: &str = "select blocks from _tinystore_search_runs where id = ?1";
const WRITE_RUN: &str = "insert or replace into _tinystore_search_runs (id, blocks) values (?1, ?2)";
const LAST_RUN: &str = "select max(id) from _tinystore_search_runs where id >= ?1 and id < ?2";
const DROP_RUNS: &str = "delete from _tinystore_search_runs where id >= ?1 and id < ?2";
const READ_PIECES_AFTER: &str = "select key, count, postings, directory, best from _tinystore_search_pieces
    where segment = ?1 and key > ?2 order by key limit cast(?3 as integer)";
const DROP_PIECES: &str = "delete from _tinystore_search_pieces where segment = ?1 and key > ?2 and key <= ?3";
const DROP_SEGMENT: &str = "delete from _tinystore_search_segments where id = ?1";
const JOIN_MERGE: &str = "update _tinystore_search_segments set merge = ?2 where id = ?1";
const WRITE_THROUGH: &str = "update _tinystore_search_segments set through = ?2 where id = ?1";
const COUNT_KEY: &str = "update _tinystore_search_keys set documents = documents + ?2 where id = ?1";
const NOTE_LISTS: &str = "update _tinystore_search_indexes set lists_version = lists_version + 1 where id = ?1";

/// Pieces a step reads of each segment it merges at most, unless one key
/// holds more.
pub(crate) const STEP_PIECES: usize = 4096;

/// What a step of merging did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Merged {
    pub(crate) postings: u64,
    /// No merge is due after it.
    pub(crate) done: bool,
}

/// Merges a step of an index's segments: the merge under way, else the
/// oldest `MERGE_AT` of the lowest level that holds as many, else, when
/// `all` asks, every segment into one folded, once there are two or one not
/// folded. Reads `most` pieces of each segment at most.
pub(crate) fn step(tx: &Tx<'_>, index: i64, most: usize, all: bool) -> Result<Merged> {
    norm(tx, index)?;
    let segments = segments::of_index(tx, index)?;
    let merge = match under_way(&segments) {
        Some(merge) => merge,
        None => match due(&segments, all) {
            Some((inputs, level)) => begin(tx, index, inputs, level)?,
            None => return Ok(Merged { done: true, ..Merged::default() }),
        },
    };
    let postings = merge.step(tx, index, most)?;
    note_lists(tx, index)?;
    Ok(Merged { postings, done: false })
}

/// A merge: the segments it joins, in their documents' order, and the one
/// it writes.
struct Merge {
    inputs: Vec<Segment>,
    output: Segment,
}

fn under_way(segments: &[Segment]) -> Option<Merge> {
    let output = *segments.iter().find(|segment| segment.through.is_some())?;
    let inputs = segments.iter().filter(|segment| segment.merge == Some(output.id)).copied().collect();
    Some(Merge { inputs, output })
}

/// The segments a merge is due to join and the level it writes: `MERGE_AT`
/// one after another of the lowest level that holds as many, the open
/// segment left out; or every one, when `all` asks and there is work.
fn due(segments: &[Segment], all: bool) -> Option<(Vec<Segment>, i64)> {
    let open = segments.last().filter(|newest| newest.level == 0 && newest.docs < segments::OPEN_DOCS);
    let closed = &segments[..segments.len() - usize::from(open.is_some())];
    let mut levels: Vec<i64> = closed.iter().map(|segment| segment.level).collect();
    levels.sort_unstable();
    levels.dedup();
    for level in levels {
        let window = closed.windows(MERGE_AT).find(|window| window.iter().all(|segment| segment.level == level));
        if let Some(window) = window {
            return Some((window.to_vec(), level + 1));
        }
    }
    let settled = segments.len() == 1 && segments[0].folded;
    if !all || segments.is_empty() || settled {
        return None;
    }
    let highest = segments.iter().map(|segment| segment.level).max().unwrap_or(0);
    Some((segments.to_vec(), (highest + 1).max(FOLDED_FROM)))
}

/// Begins a merge of `inputs` into a new segment of `level`.
fn begin(tx: &Tx<'_>, index: i64, inputs: Vec<Segment>, level: i64) -> Result<Merge> {
    let (Some(first), Some(last)) = (inputs.first(), inputs.last()) else {
        return Err(Error::internal("a merge of no segments"));
    };
    let docs = inputs.iter().map(|segment| segment.docs).sum();
    let mut output = segments::add(tx, index, (level, first.first, last.past), docs)?;
    execute(tx, WRITE_THROUGH, (output.id, 0), "begin a merge")?;
    output.through = Some(0);
    for input in &inputs {
        execute(tx, JOIN_MERGE, (input.id, output.id), "begin a merge")?;
    }
    Ok(Merge { inputs, output })
}

/// A piece's row as a merge reads it: its key, its count and its bytes.
type Row = (i64, i64, PieceBytes);
/// Pieces by their key, each with its input's place.
type Keyed = BTreeMap<i64, Vec<(usize, Row)>>;

impl Merge {
    /// Moves the next keys of the inputs into the output: those up to the
    /// least last key of an input that has more than `most` left. Says how
    /// many postings it wrote.
    fn step(&self, tx: &Tx<'_>, index: i64, most: usize) -> Result<u64> {
        let through = self.output.through.unwrap_or(0);
        let (keys, upto) = self.next_keys(tx, through, most)?;
        // dropped before the output is written, which takes their pages again
        for input in &self.inputs {
            execute(tx, DROP_PIECES, (input.id, through, upto), "drop merged pieces")?;
        }
        let gone = gone(tx, index)?;
        let mut reading = Reading::of(index);
        let mut next_run = next_run(tx, &self.output)?;
        let mut moved = 0;
        for (key, rows) in keys {
            let within = |error: Error| error.within(format!("key {key}"));
            let (count, next) = self.merge_key(tx, (key, rows), (&gone, &mut reading), next_run).map_err(within)?;
            next_run = next;
            moved += count;
        }
        if upto == i64::MAX {
            self.finish(tx)?;
        } else {
            execute(tx, WRITE_THROUGH, (self.output.id, upto), "write how far a merge went")?;
        }
        Ok(moved)
    }

    /// The pieces of each key past `through` up to the last a step takes,
    /// each with its input's place; and that key, or `i64::MAX` once every
    /// input's last piece is among them.
    fn next_keys(&self, tx: &Tx<'_>, through: i64, most: usize) -> Result<(Keyed, i64)> {
        let failed = |error| sql_error("read a segment's pieces", error);
        let mut select = tx.prepare_cached(READ_PIECES_AFTER).map_err(failed)?;
        let mut upto = i64::MAX;
        let mut read = Vec::with_capacity(self.inputs.len());
        for input in &self.inputs {
            let rows = select.query_map((input.id, through, most as i64), |row| {
                Ok((row.get(0)?, row.get(1)?, (row.get(2)?, row.get(3)?, row.get(4)?)))
            });
            let rows: Vec<Row> = rows.and_then(Iterator::collect).map_err(failed)?;
            if rows.len() >= most
                && let Some(last) = rows.last()
            {
                upto = upto.min(last.0);
            }
            read.push(rows);
        }
        let mut keys = Keyed::new();
        for (place, rows) in read.into_iter().enumerate() {
            for row in rows.into_iter().take_while(|row| row.0 <= upto) {
                keys.entry(row.0).or_default().push((place, row));
            }
        }
        Ok((keys, upto))
    }

    /// Writes a key's pieces of the inputs as one of the output, those gone
    /// left out, and counts what folding or leaving them out changes in its
    /// row. Says how many postings it wrote, and the output's next run.
    fn merge_key(
        &self,
        tx: &Tx<'_>,
        (key, rows): (i64, Vec<(usize, Row)>),
        (gone, reading): (&HashSet<u32>, &mut Reading),
        next_run: i64,
    ) -> Result<(u64, i64)> {
        let (part, average) = reading.averages.of_key(tx, key)?;
        let mut writer = Writer::new(self.output, average, next_run);
        let (mut counted, mut unfolded) = (0, 0);
        for (place, (_, count, bytes)) in rows {
            let input = &self.inputs[place];
            counted += count;
            if !input.folded {
                unfolded += count;
            }
            let piece = segments::piece_of(input, bytes)?;
            each_posting(tx, &piece, (part, &mut reading.lengths), &mut |posting| {
                if gone.contains(&posting.doc) { Ok(()) } else { writer.push(tx, posting) }
            })?;
            // its runs given back as it is read, so that what the step writes
            // next takes their pages and a merge needs no room for two copies
            if let Piece::Packed(directory, _) = &piece {
                directory.runs.iter().try_for_each(|run| drop_run(tx, run.id))?;
            }
        }
        let (count, written, next_run) = writer.finish(tx)?;
        if let Some(written) = written {
            segments::write_piece(tx, (&self.output, key), count, &written)?;
        }
        let folding = if self.output.folded { unfolded } else { counted - count };
        if folding != 0 {
            execute(tx, COUNT_KEY, (key, folding), "count a key's documents")?;
        }
        Ok((count as u64, next_run))
    }

    /// Ends the merge: the inputs go, rows and runs, and the output is whole.
    fn finish(&self, tx: &Tx<'_>) -> Result<()> {
        for input in &self.inputs {
            let runs = input.runs();
            execute(tx, DROP_RUNS, (runs.start, runs.end), "drop a merged segment's runs")?;
            execute(tx, DROP_SEGMENT, [input.id], "drop a merged segment")?;
        }
        execute(tx, WRITE_THROUGH, (self.output.id, None::<i64>), "end a merge").map(drop)
    }
}

/// What a step reads beside the pieces: the parts' averages and their
/// documents' lengths.
pub(super) struct Reading {
    pub(super) averages: Averages,
    pub(super) lengths: Lengths,
}

impl Reading {
    pub(super) fn of(index: i64) -> Reading {
        Reading { averages: Averages::of(index), lengths: Lengths::new(index) }
    }
}

/// Calls `each` with every posting of a piece, in order, with its length:
/// a short piece's own, a long one's from `lengths`, its part's.
pub(super) fn each_posting(
    connection: &Connection,
    piece: &Piece,
    (part, lengths): (u32, &mut Lengths),
    each: &mut dyn FnMut(Posting) -> Result<()>,
) -> Result<()> {
    let directory = match piece {
        Piece::Short(postings) => return postings.iter().try_for_each(|posting| each(*posting)),
        Piece::Packed(directory, _) => directory,
    };
    let mut pairs = Vec::with_capacity(BLOCK);
    for (place, run) in directory.runs.iter().enumerate() {
        let bytes = read_run(connection, run.id)?;
        for at in directory.blocks_of(place) {
            let lying = directory.bytes_of(at, bytes.len());
            packed::unpack(&bytes[lying], directory.blocks[at].first, &mut pairs)?;
            for &(doc, tf) in &pairs {
                each(Posting { doc, tf, len: lengths.of(connection, part, doc)? })?;
            }
        }
    }
    Ok(())
}

/// Every document a piece holds, in order.
pub(super) fn documents(connection: &Connection, piece: &Piece) -> Result<Vec<u32>> {
    let directory = match piece {
        Piece::Short(postings) => return Ok(postings.iter().map(|posting| posting.doc).collect()),
        Piece::Packed(directory, _) => directory,
    };
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

/// A key's piece as a merge or upkeep writes it into a segment: its
/// postings held till a block's are, then packed, a run of blocks a row
/// till it holds 32 KiB. A piece of no more than a short one's postings
/// keeps them.
pub(super) struct Writer {
    segment: Segment,
    average: u16,
    next_run: i64,
    count: i64,
    held: Vec<Posting>,
    directory: Directory,
    run: Vec<u8>,
    best: Vec<Posting>,
}

impl Writer {
    /// A writer of a piece in `segment`, its blocks bounded at `average`,
    /// its runs numbered from `next_run`.
    pub(super) fn new(segment: Segment, average: u16, next_run: i64) -> Writer {
        let directory = Directory::default();
        let (held, run, best) = (Vec::with_capacity(BLOCK), Vec::new(), Vec::new());
        Writer { segment, average, next_run, count: 0, held, directory, run, best }
    }

    /// Takes the next posting, after every one before.
    pub(super) fn push(&mut self, connection: &Connection, posting: Posting) -> Result<()> {
        self.count += 1;
        self.held.push(posting);
        if self.held.len() == BLOCK { self.block(connection) } else { Ok(()) }
    }

    /// Packs the held postings as the next block, in the last run or in a
    /// new one once it holds 32 KiB.
    fn block(&mut self, connection: &Connection) -> Result<()> {
        if self.directory.runs.is_empty() || self.run.len() >= RUN_BYTES {
            self.next_run(connection)?;
        }
        let at = u16::try_from(self.run.len()).map_err(|_| Error::internal("a block starts past its run's place"))?;
        let (first, last) = (self.held[0].doc, self.held[self.held.len() - 1].doc);
        let bound = bound_of(&self.held, self.average);
        self.best = best_of(std::mem::take(&mut self.best), &self.held, self.average);
        packed::pack(&self.held, &mut self.run);
        self.directory.blocks.push(Placed { first, last, bound, average: self.average, at });
        self.held.clear();
        Ok(())
    }

    /// Writes the last run and begins the next.
    fn next_run(&mut self, connection: &Connection) -> Result<()> {
        self.write_run(connection)?;
        if !self.segment.runs().contains(&self.next_run) {
            return Err(Error::limit(format!("segment {}: past the runs its ids can name", self.segment.id)));
        }
        let first_block = self.directory.blocks.len() as u32;
        self.directory.runs.push(Run { id: self.next_run, first_block });
        self.next_run += 1;
        self.run.clear();
        Ok(())
    }

    fn write_run(&self, connection: &Connection) -> Result<()> {
        match self.directory.runs.last() {
            Some(run) => execute(connection, WRITE_RUN, (run.id, &self.run), "write a run").map(drop),
            None => Ok(()),
        }
    }

    /// Ends the piece: how many postings it holds, how it is written, none
    /// when it holds none, and the segment's next run.
    pub(super) fn finish(mut self, connection: &Connection) -> Result<(i64, Option<Written>, i64)> {
        if self.count as usize <= INLINE {
            let written = (self.count > 0).then(|| Written::Short(std::mem::take(&mut self.held)));
            return Ok((self.count, written, self.next_run));
        }
        if !self.held.is_empty() {
            self.block(connection)?;
        }
        self.write_run(connection)?;
        let best = if self.directory.blocks.len() >= BEST_AT { encode_best(&self.best) } else { Vec::new() };
        Ok((self.count, Some(Written::Packed(self.directory, best)), self.next_run))
    }
}

/// The id a segment's next run takes: past its last.
pub(super) fn next_run(connection: &Connection, segment: &Segment) -> Result<i64> {
    let runs = segment.runs();
    let failed = |error| sql_error("read a segment's runs", error);
    let last: Option<i64> = connection
        .prepare_cached(LAST_RUN)
        .and_then(|mut select| select.query_row((runs.start, runs.end), |row| row.get(0)))
        .map_err(failed)?;
    Ok(last.map_or(runs.start, |last| last + 1))
}

pub(super) fn read_run(connection: &Connection, id: i64) -> Result<Vec<u8>> {
    let failed = |error| sql_error("read a run", error);
    let bytes: Option<Vec<u8>> = connection
        .prepare_cached(READ_RUN)
        .and_then(|mut select| select.query_row([id], |row| row.get(0)).optional())
        .map_err(failed)?;
    bytes.ok_or_else(|| Error::corrupt(format!("run {id} a directory names is missing")))
}

pub(super) fn write_run(connection: &Connection, id: i64, bytes: &[u8]) -> Result<()> {
    execute(connection, WRITE_RUN, (id, bytes), "write a run").map(drop)
}

pub(super) fn drop_run(connection: &Connection, id: i64) -> Result<()> {
    execute(connection, DROP_RUNS, (id, id + 1), "drop a run").map(drop)
}

/// Counts a commit that changes the index's pieces, so that a piece a
/// search keeps from before is read again.
pub(super) fn note_lists(connection: &Connection, index: i64) -> Result<()> {
    execute(connection, NOTE_LISTS, [index], "count a change of pieces").map(drop)
}

/// The index's documents gone, not purged yet.
pub(super) fn gone(connection: &Connection, index: i64) -> Result<HashSet<u32>> {
    let failed = |error| sql_error("read the documents gone", error);
    let mut select = connection.prepare_cached(READ_GONE).map_err(failed)?;
    let docs = select.query_map([index], |row| row.get::<_, u32>(0));
    docs.and_then(Iterator::collect).map_err(failed)
}

/// Gives every document set since the last step its lengths, row of
/// `norms` by row.
pub(super) fn norm(tx: &Tx<'_>, index: i64) -> Result<()> {
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

/// Each key's part and the average its new blocks are bounded at, its
/// part's now, each part read once.
pub(super) struct Averages {
    index: i64,
    parts: HashMap<u32, u16>,
}

impl Averages {
    pub(super) fn of(index: i64) -> Averages {
        Averages { index, parts: HashMap::new() }
    }

    pub(super) fn of_key(&mut self, connection: &Connection, key: i64) -> Result<(u32, u16)> {
        let failed = |error| sql_error("read a key's part", error);
        let part: u32 = connection
            .prepare_cached(READ_PART)
            .and_then(|mut select| select.query_row([key], |row| row.get(0)))
            .map_err(failed)?;
        if let Some(average) = self.parts.get(&part) {
            return Ok((part, *average));
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
        Ok((part, bounded_at))
    }
}

/// The lengths of documents in their parts, a row of `norms` read once.
pub(super) struct Lengths {
    index: i64,
    rows: HashMap<(u32, u32), Vec<u16>>,
}

impl Lengths {
    pub(super) fn new(index: i64) -> Lengths {
        Lengths { index, rows: HashMap::new() }
    }

    /// A document's length in `part`: 0 where no document of its row has
    /// words in the part, as a filter's part has none.
    pub(super) fn of(&mut self, connection: &Connection, part: u32, doc: u32) -> Result<u32> {
        let chunk = doc / NORMS;
        if !self.rows.contains_key(&(part, chunk)) {
            let failed = |error| sql_error("read lengths", error);
            let bytes: Option<Vec<u8>> = connection
                .prepare_cached(READ_NORMS)
                .and_then(|mut select| select.query_row((self.index, part, chunk), |row| row.get(0)).optional())
                .map_err(failed)?;
            self.rows.insert((part, chunk), lengths_of(&bytes.unwrap_or_default()));
        }
        let row = &self.rows[&(part, chunk)];
        Ok(u32::from(row.get((doc % NORMS) as usize).copied().unwrap_or(0)))
    }
}
