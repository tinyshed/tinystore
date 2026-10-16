//! A search, in one snapshot of search.db: the index's statistics, its
//! words' keys, their lists from the blocks and the head, the best of them,
//! and the hits' values.
//!
//! The read lasts the whole search rather than fetching its bytes alone:
//! which block a search opens next depends on the scores of those it opened,
//! and the blocks' bounds skip most of them. A search's statements are
//! point and short range reads, and search-compact measured them at 14–16
//! ms at p99 over a million documents, so the read stays as short.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};

use super::analyzer::{self, Word};
use super::correct::Corrector;
use super::execute::{Fetch, List, best};
use super::kept::{Chunk, Gone, Kept, Moment, NORMS, lock};
use super::pack::{Stored, read_list};
use super::postings::{Posting, decode_directory, unpack, unpack_inline};
use super::score::{Part, average, idf};
use super::settings::Settings;
use crate::sqlite::sql_error;
use crate::{Error, Memory, Result};

const READ_INDEX: &str =
    "select documents, normed, gone_version, words_version from _tinystore_search_indexes where id = ?1";
const READ_PARTS: &str = "select part, tokens from _tinystore_search_parts where index_id = ?1";
const FIND_KEY: &str =
    "select id, documents from _tinystore_search_keys where index_id = ?1 and part = ?2 and term = ?3";
const READ_HEAD: &str = "select doc, tf, len from _tinystore_search_head where index_id = ?1 and key = ?2 order by doc";
const READ_BLOCK: &str = "select payload from _tinystore_search_blocks where key = ?1 and first = ?2";
const READ_BLOCKS: &str =
    "select first, payload from _tinystore_search_blocks where key = ?1 and first >= ?2 and first <= ?3 order by first";
const READ_HIT: &str = "select id, value from _tinystore_search_docs where index_id = ?1 and doc = ?2";

/// Blocks a statement reads at once while a list is read in order.
const AHEAD: usize = 8;

/// A search's text as the analyzer normalized it, its words found in it,
/// and the hits it asks for.
#[derive(Debug)]
pub(crate) struct Query {
    pub(crate) normal: String,
    pub(crate) words: Vec<Word>,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
}

/// A query of an index, and what the index keeps for it.
#[derive(Debug)]
pub(crate) struct Asked<'a> {
    pub(crate) index: i64,
    pub(crate) settings: &'a Settings,
    pub(crate) kept: &'a Kept,
    pub(crate) memory: &'a Arc<Memory>,
    pub(crate) query: Query,
}

/// What a search found: each hit's id, value as JSON and points, and the
/// search's text as it was read, when a word was read as another.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) hits: Vec<(String, Vec<u8>, u64)>,
    pub(crate) corrected: Option<String>,
    /// The index's words changed since its dictionary was built.
    pub(crate) behind: bool,
}

pub(crate) fn search(connection: &Connection, mut asked: Asked<'_>) -> Result<Answer> {
    let (moment, documents, words_version) = moment(connection, asked.index)?;
    let mut behind = false;
    let corrected = correct(connection, &mut asked, words_version, &mut behind)?;
    let stats =
        Stats { documents, tokens: tokens(connection, asked.index)?, gone: asked.kept.gone(connection, moment)? };
    let k = asked.query.offset + asked.query.limit;
    let mut source = Source::new(connection, &asked, moment, &stats);
    let found = best(&mut lists(connection, &asked, &stats)?, &mut source, k)?;
    #[cfg(test)]
    check(connection, &asked, &stats, moment, &found)?;
    let hits =
        found.into_iter().skip(asked.query.offset).map(|(points, doc)| hit(connection, asked.index, doc, points));
    Ok(Answer { hits: hits.collect::<Result<_>>()?, corrected, behind })
}

/// The moment of the index this snapshot holds, its documents and how far
/// its words have changed.
fn moment(connection: &Connection, index: i64) -> Result<(Moment, u64, i64)> {
    let mut select = connection.prepare_cached(READ_INDEX).map_err(|error| sql_error("read an index", error))?;
    let row = select.query_row([index], |row| {
        let moment = Moment { index, normed: row.get(1)?, gone_version: row.get(2)? };
        Ok((moment, row.get::<_, i64>(0)?.max(0) as u64, row.get(3)?))
    });
    row.map_err(|error| sql_error("read an index", error))
}

/// What scores a snapshot's postings: how many documents the index holds,
/// how many words each part has in all of them, and the documents gone.
struct Stats {
    documents: u64,
    tokens: HashMap<Part, u64>,
    gone: Arc<Gone>,
}

/// The lists of a search's keys.
fn lists(connection: &Connection, asked: &Asked<'_>, stats: &Stats) -> Result<Vec<List>> {
    let mut lists = Vec::new();
    for form in forms(&asked.query.words, asked.settings) {
        if let Some(list) = list(connection, asked, stats, &form)? {
            lists.push(list);
        }
    }
    Ok(lists)
}

#[cfg(test)]
thread_local! {
    static CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `run` with every search on this thread scoring every posting too, in
/// its own snapshot, and failing unless both answer alike.
#[cfg(test)]
pub(crate) fn checked<T>(run: impl FnOnce() -> T) -> T {
    CHECKED.set(true);
    let ran = run();
    CHECKED.set(false);
    ran
}

#[cfg(test)]
fn check(
    connection: &Connection,
    asked: &Asked<'_>,
    stats: &Stats,
    moment: Moment,
    found: &[(u64, u32)],
) -> Result<()> {
    if !CHECKED.get() {
        return Ok(());
    }
    let k = asked.query.offset + asked.query.limit;
    let mut source = Source::new(connection, asked, moment, stats);
    let every = super::execute::every(&mut lists(connection, asked, stats)?, &mut source, k)?;
    if every != found {
        return Err(Error::internal(format!("found {found:?} where every posting gives {every:?}")));
    }
    Ok(())
}

/// How many words each part has in every document.
fn tokens(connection: &Connection, index: i64) -> Result<HashMap<Part, u64>> {
    let failed = |error| sql_error("read an index's lengths", error);
    let mut select = connection.prepare_cached(READ_PARTS).map_err(failed)?;
    let rows = select.query_map([index], |row| Ok((Part(row.get(0)?), row.get::<_, i64>(1)?.max(0) as u64)));
    rows.and_then(Iterator::collect).map_err(failed)
}

/// The keys a search's words are found by, each once: a word as written
/// in every field, its stem among the stems, and its stem as written too.
pub(super) fn forms(words: &[Word], settings: &Settings) -> Vec<(Part, String)> {
    let mut forms: Vec<(Part, String)> = Vec::new();
    for word in words {
        let stem = word.stem.as_deref().unwrap_or(&word.text);
        for field in &settings.fields {
            let (written, stems) = (Part::written(field.number), Part::stems(field.number));
            forms.push((written, word.text.clone()));
            forms.push((stems, stem.to_owned()));
            if word.stem.is_some() {
                forms.push((written, stem.to_owned()));
            }
        }
    }
    let mut seen = HashSet::new();
    forms.retain(|form| seen.insert(form.clone()));
    forms
}

/// A key's id and how many documents hold it now.
fn find(connection: &Connection, index: i64, part: Part, term: &str) -> Result<Option<(i64, u64)>> {
    let mut select = connection.prepare_cached(FIND_KEY).map_err(|error| sql_error("find a key", error))?;
    let found = select.query_row((index, part.0, term), |row| Ok((row.get(0)?, row.get::<_, i64>(1)?.max(0) as u64)));
    let found = found.optional();
    found.map_err(|error| sql_error("find a key", error))
}

/// How many documents hold `word`, a document counted once for each of its keys.
fn held(connection: &Connection, asked: &Asked<'_>, word: &Word) -> Result<u64> {
    let mut documents = 0;
    for (part, term) in forms(std::slice::from_ref(word), asked.settings) {
        let found = find(connection, asked.index, part, &term)?;
        documents += found.map_or(0, |(_, documents)| documents);
    }
    Ok(documents)
}

/// Reads each word that matches nothing as its nearest the index holds,
/// and says the search's text as it was read when any was.
fn correct(connection: &Connection, asked: &mut Asked<'_>, version: i64, behind: &mut bool) -> Result<Option<String>> {
    if !asked.settings.typo_tolerance {
        return Ok(None);
    }
    let mut replaced = Vec::new();
    for at in 0..asked.query.words.len() {
        let word = &asked.query.words[at];
        if word.text.chars().count() < 5 || held(connection, asked, word)? > 0 {
            continue;
        }
        let corrector = corrector(connection, asked, version, behind)?;
        let mut chosen: Option<(u64, Word)> = None;
        for candidate in corrector.candidates(&word.text) {
            let read = Word { stem: analyzer::stem(&candidate), text: candidate, at: word.at.clone() };
            let documents = held(connection, asked, &read)?;
            if documents > chosen.as_ref().map_or(0, |(most, _)| *most) {
                chosen = Some((documents, read));
            }
        }
        if let Some((_, read)) = chosen {
            replaced.push((read.at.clone(), read.text.clone()));
            asked.query.words[at] = read;
        }
    }
    if replaced.is_empty() {
        return Ok(None);
    }
    let mut text = asked.query.normal.to_owned();
    for (at, word) in replaced.into_iter().rev() {
        text.replace_range(at, &word);
    }
    Ok(Some(text))
}

/// The dictionary of the index's words kept, built from this snapshot when
/// none is; `behind` says when the kept one is older than the snapshot.
fn corrector(connection: &Connection, asked: &Asked<'_>, version: i64, behind: &mut bool) -> Result<Arc<Corrector>> {
    if let Some(kept) = lock(&asked.kept.corrector).as_ref() {
        *behind = kept.version != version;
        return Ok(Arc::clone(kept));
    }
    let parts: Vec<Part> = asked.settings.fields.iter().map(|field| Part::written(field.number)).collect();
    let built = Arc::new(Corrector::build(connection, asked.index, version, &parts)?);
    *lock(&asked.kept.corrector) = Some(Arc::clone(&built));
    Ok(built)
}

/// A key's list: its blocks by its directory, or its postings in its own
/// row, then its head; nothing when no document holds it.
fn list(
    connection: &Connection,
    asked: &Asked<'_>,
    stats: &Stats,
    (part, term): &(Part, String),
) -> Result<Option<List>> {
    let part = *part;
    let Some((key, holding)) = find(connection, asked.index, part, term)? else { return Ok(None) };
    let Some(weight) = asked.settings.weight(part.field()) else { return Ok(None) };
    if holding == 0 {
        return Ok(None);
    }
    let tokens = stats.tokens.get(&part).copied().unwrap_or(0);
    let scale = part.weight(weight) * idf(stats.documents, holding);
    let mut list = List::new(key, part, scale, average(tokens, stats.documents), Vec::new());
    let gone = &stats.gone.docs;
    let within = |error: Error| error.within(format!("key {key}"));
    match read_list(connection, key).map_err(within)? {
        Stored::Blocks(directory) => {
            for entry in decode_directory(&directory).map_err(within)? {
                list.push(list.stored(entry.first, entry.last, entry.most_tf, entry.least_len));
            }
        }
        Stored::Inline(inline) => push_held(&mut list, unpack_inline(&inline).map_err(within)?, gone),
        Stored::Nothing => {}
    }
    push_held(&mut list, head(connection, asked.index, key)?, gone);
    Ok((!list.is_empty()).then_some(list))
}

/// Postings in hand as a list's next block, those of documents gone left out.
fn push_held(list: &mut List, mut postings: Vec<Posting>, gone: &HashSet<u32>) {
    postings.retain(|posting| !gone.contains(&posting.doc));
    if !postings.is_empty() {
        let block = list.in_hand(postings);
        list.push(block);
    }
}

fn head(connection: &Connection, index: i64, key: i64) -> Result<Vec<Posting>> {
    let failed = |error| sql_error(format!("read key {key}'s head"), error);
    let mut select = connection.prepare_cached(READ_HEAD).map_err(failed)?;
    let rows =
        select.query_map((index, key), |row| Ok(Posting { doc: row.get(0)?, tf: row.get(1)?, len: row.get(2)? }));
    rows.and_then(Iterator::collect).map_err(failed)
}

fn hit(connection: &Connection, index: i64, doc: u32, points: u64) -> Result<(String, Vec<u8>, u64)> {
    let failed = |error| sql_error(format!("read document {doc}"), error);
    let mut select = connection.prepare_cached(READ_HIT).map_err(failed)?;
    let found = select.query_row((index, doc), |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(failed)?;
    let (id, value) = found.ok_or_else(|| Error::corrupt(format!("document {doc} found by its words is not kept")))?;
    Ok((id, value, points))
}

/// Where a search's blocks come from: SQLite, a statement a block or eight
/// of them read in order, their lengths from the rows of lengths kept.
struct Source<'a> {
    connection: &'a Connection,
    asked: &'a Asked<'a>,
    moment: Moment,
    gone: &'a HashSet<u32>,
    chunks: HashMap<(Part, u32), Arc<Chunk>>,
    /// Blocks read ahead of the one asked for, by key and place.
    ahead: HashMap<(i64, usize), Vec<Posting>>,
}

impl Fetch for Source<'_> {
    fn block(&mut self, list: &List, index: usize) -> Result<Vec<Posting>> {
        if let Some(postings) = self.ahead.remove(&(list.key, index)) {
            return Ok(postings);
        }
        let last = if list.in_order(index) { list.stored_after(index, AHEAD - 1) } else { index };
        let payloads = self.payloads(list, index, last).map_err(|error| error.within(format!("key {}", list.key)))?;
        let mut wanted = Vec::new();
        for (offset, payload) in payloads.into_iter().enumerate() {
            let postings = self.postings(list.part, &payload).map_err(|error| {
                error.within(format!("key {}: block at document {}", list.key, list.first_of(index + offset)))
            })?;
            if offset == 0 {
                wanted = postings;
            } else {
                self.ahead.insert((list.key, index + offset), postings);
            }
        }
        Ok(wanted)
    }
}

impl<'a> Source<'a> {
    fn new(connection: &'a Connection, asked: &'a Asked<'a>, moment: Moment, stats: &'a Stats) -> Source<'a> {
        Source { connection, asked, moment, gone: &stats.gone.docs, chunks: HashMap::new(), ahead: HashMap::new() }
    }

    /// The payloads of blocks `index` to `last` of a list, each the one its
    /// directory names.
    fn payloads(&self, list: &List, index: usize, last: usize) -> Result<Vec<Vec<u8>>> {
        let failed = |error| sql_error("read a block", error);
        let missing =
            || Error::corrupt(format!("block at document {} its directory names is missing", list.first_of(index)));
        if last == index {
            let mut select = self.connection.prepare_cached(READ_BLOCK).map_err(failed)?;
            let payload = select.query_row((list.key, list.first_of(index)), |row| row.get(0)).optional();
            return Ok(vec![payload.map_err(failed)?.ok_or_else(missing)?]);
        }
        let mut select = self.connection.prepare_cached(READ_BLOCKS).map_err(failed)?;
        let range = (list.key, list.first_of(index), list.first_of(last));
        let rows = select.query_map(range, |row| Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?)));
        let rows: Vec<(u32, Vec<u8>)> = rows.and_then(Iterator::collect).map_err(failed)?;
        let named = rows.iter().zip(index..=last).all(|((first, _), at)| *first == list.first_of(at));
        if rows.len() != last - index + 1 || !named {
            return Err(missing());
        }
        Ok(rows.into_iter().map(|(_, payload)| payload).collect())
    }

    /// A block's postings, documents gone left out, each with its length.
    fn postings(&mut self, part: Part, payload: &[u8]) -> Result<Vec<Posting>> {
        let mut postings = Vec::with_capacity(super::postings::BLOCK);
        let mut chunk: Option<(u32, Arc<Chunk>)> = None;
        for (doc, tf) in unpack(payload)? {
            if self.gone.contains(&doc) {
                continue;
            }
            if chunk.as_ref().is_none_or(|(at, _)| *at != doc / NORMS) {
                chunk = Some((doc / NORMS, self.chunk(part, doc / NORMS)?));
            }
            let len = chunk.as_ref().map_or(0, |(_, lengths)| lengths.length(doc));
            postings.push(Posting { doc, tf, len });
        }
        Ok(postings)
    }

    fn chunk(&mut self, part: Part, chunk: u32) -> Result<Arc<Chunk>> {
        if let Some(read) = self.chunks.get(&(part, chunk)) {
            return Ok(Arc::clone(read));
        }
        let read = self.asked.kept.chunk(self.connection, self.asked.memory, self.moment, (part, chunk))?;
        self.chunks.insert((part, chunk), Arc::clone(&read));
        Ok(read)
    }
}
