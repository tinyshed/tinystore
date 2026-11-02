//! Sets and deletes, each a grouped write of search.db.
//!
//! A set gives its document a new number and writes its postings into the
//! newest segment, so a segment's documents always come after every older
//! one's. The number the document had is marked gone: searches leave it out
//! of every piece until a merge or a purge takes it out of them.
//!
//! A document's vector is kept beside it and goes with it: a set keeps the
//! new number's and forgets the old one's, so a scan of vectors never meets
//! a document gone.
//!
//! Every count a score reads changes here and nowhere else: how many
//! documents the index holds, how many hold each key, its row's and its
//! pieces' counts, how many words each part has. Merges and purges move
//! postings and counts and change no sum, so they change no score.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::document::{Analyzed, analyze};
use super::postings::{decode_keys, decode_lengths, encode_keys, encode_lengths};
use super::score::Part;
use super::segments;
use super::settings::Settings;
use super::vectors::{self, Vector};
use crate::limits::Limit;
use crate::sqlite::{Tx, sql_error};
use crate::sync::lock;
use crate::{Error, Result};

const FIND_DOC: &str = "select doc, keys, lengths from _tinystore_search_docs where index_id = ?1 and id = ?2";
const MARK_GONE: &str = "insert into _tinystore_search_gone (index_id, doc, keys) values (?1, ?2, ?3)";
const DROP_DOC: &str = "delete from _tinystore_search_docs where index_id = ?1 and doc = ?2";
const READ_KEY: &str = "select part, documents from _tinystore_search_keys where id = ?1";
const LEAVE_KEY: &str = "update _tinystore_search_keys set documents = documents - 1 where id = ?1";
const ADD_TOKENS: &str = "insert into _tinystore_search_parts (index_id, part, tokens) values (?1, ?2, ?3)
    on conflict (index_id, part) do update set tokens = tokens + excluded.tokens";
const LEAVE_INDEX: &str =
    "update _tinystore_search_indexes set documents = documents - 1, gone_version = gone_version + 1,
    words_version = words_version + ?2, vectors = vectors - ?3 where id = ?1";
const NEXT_DOC: &str = "select next_doc from _tinystore_search_indexes where id = ?1";
// A set counts a key in its piece, not its row: a set of a thousand passages
// held some 12,000 keys, and counting each in its row wrote a page a key.
const FIND_KEY: &str = "select id from _tinystore_search_keys where index_id = ?1 and part = ?2 and term = ?3";
const NEW_KEY: &str = "insert into _tinystore_search_keys (index_id, part, term, documents) values (?1, ?2, ?3, 0)";
const KEEP_DOC: &str =
    "insert into _tinystore_search_docs (index_id, doc, id, value, keys, lengths) values (?1, ?2, ?3, ?4, ?5, ?6)";
const JOIN_INDEX: &str = "update _tinystore_search_indexes set next_doc = ?2, documents = documents + ?3,
    words_version = words_version + ?4, vectors = vectors + ?5 where id = ?1";

/// The last number an index gives: past it, [`END`](super::execute::END).
const LAST_DOC: i64 = u32::MAX as i64 - 1;
const DOCUMENT_NUMBERS: Limit = Limit { name: "document numbers", bound: LAST_DOC as u64 };

/// A document ready to set: its id, its value as JSON, its words and its
/// vector, if it has one.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub(crate) index: i64,
    pub(crate) id: String,
    pub(crate) value: Vec<u8>,
    pub(crate) analyzed: Analyzed,
    pub(crate) vector: Option<Vector>,
    /// Whether the index keeps clusters of its vectors, whose head a vector joins.
    pub(crate) clustered: bool,
}

/// A document as a set or a rebuild gives it: its id, its value as JSON
/// and its bytes, and what it says of its vector: nothing, none, or one.
#[derive(Debug)]
pub(crate) struct Incoming<'v> {
    pub(crate) id: String,
    pub(crate) tree: Value,
    pub(crate) bytes: Vec<u8>,
    pub(crate) vector: Option<Option<&'v [f32]>>,
}

impl Prepared {
    /// A document ready to set in the index `index` by `settings`: its words
    /// as its fields read them and its vector, which an index with vectors
    /// requires a set to give or to say it has none.
    pub(crate) fn new(index: i64, incoming: Incoming<'_>, settings: &Settings) -> Result<Prepared> {
        let analyzed = analyze(&incoming.tree, &settings.fields, &settings.filters)?;
        let vector = match (settings.vectors, incoming.vector) {
            (None, Some(Some(_))) => return Err(Error::invalid("a vector for an index without vectors")),
            (None, _) | (Some(_), Some(None)) => None,
            (Some(_), None) => {
                return Err(Error::invalid(
                    "the index keeps vectors: a set gives the document's vector, or says it has none",
                ));
            }
            (Some(vectors), Some(Some(values))) => Some(Vector::new(values, vectors.dimensions)?),
        };
        let clustered = settings.vectors.is_some_and(|vectors| vectors.recall.is_some());
        Ok(Prepared { index, id: incoming.id, value: incoming.bytes, analyzed, vector, clustered })
    }

    /// What the write adds to its commit, about.
    pub(crate) fn bytes(&self) -> usize {
        let words: usize = self.analyzed.terms.iter().map(|(_, term, _)| term.len() + 16).sum();
        self.value.len() + words + self.vector.as_ref().map_or(0, Vector::size)
    }
}

/// Sets a document, replacing the one of its id.
pub(crate) fn set(tx: &Tx<'_>, document: &Prepared) -> Result<()> {
    set_all(tx, std::iter::once(document))
}

/// Keys' ids by their part and term, as committed sets found or made them,
/// so that a set reads the keys' rows only for the words it has not met
/// lately: a set of a thousand passages looked up some 12,000 keys, a fifth
/// of its time.
#[derive(Debug, Default)]
pub(crate) struct Known {
    parts: HashMap<Part, HashMap<String, i64>>,
    held: usize,
    /// Grows at every clear, so that keys a set read before one are not
    /// learned after it.
    era: u64,
}

/// Keys a [`Known`] holds at most, some 5 MiB; past them it starts over.
const MOST_KNOWN: usize = 1 << 16;

/// The keys a set read from their rows or made, to learn once it commits.
#[derive(Debug)]
pub(crate) struct Learned {
    era: u64,
    keys: Vec<(Part, String, i64)>,
}

impl Known {
    fn get(&self, part: Part, term: &str) -> Option<i64> {
        self.parts.get(&part)?.get(term).copied()
    }

    /// Forgets every key: a purge drops the rows of keys no document holds,
    /// and an id kept would name a key that is gone.
    pub(crate) fn clear(&mut self) {
        self.parts.clear();
        self.held = 0;
        self.era += 1;
    }

    /// Learns the keys a set committed with, unless a clear came between.
    pub(crate) fn learn(&mut self, learned: Learned) {
        if learned.era != self.era {
            return;
        }
        if self.held + learned.keys.len() > MOST_KNOWN {
            self.clear();
        }
        for (part, term, key) in learned.keys {
            if self.parts.entry(part).or_default().insert(term, key).is_none() {
                self.held += 1;
            }
        }
    }
}

/// Sets documents of one index in their order, each replacing the one of its
/// id. What they change is written once for them all rather than once a
/// document: a key's piece once however many of them hold it, the index's
/// counts and each part's words once, and their postings into the newest
/// segment in the order of its keys, many a statement. A set of 1,000
/// passages of 50 words wrote some 80 statements a document before.
pub(crate) fn set_all<'p>(tx: &Tx<'_>, documents: impl Iterator<Item = &'p Prepared> + Clone) -> Result<()> {
    set_knowing(tx, documents, &Mutex::default()).map(drop)
}

/// [`set_all`], each key's id taken from `known` where it holds it; says
/// the keys read from their rows or made, for `known` to learn once the
/// set commits.
pub(crate) fn set_knowing<'p>(
    tx: &Tx<'_>,
    documents: impl Iterator<Item = &'p Prepared> + Clone,
    known: &Mutex<Known>,
) -> Result<Learned> {
    let Some(index) = documents.clone().next().map(|document| document.index) else {
        return Ok(Learned { era: lock(known).era, keys: Vec::new() });
    };
    let (ids, new_words, learned) = join_keys(tx, index, documents.clone(), known)?;
    let mut postings = Vec::new();
    let first = next_doc(tx, index)?;
    let mut next = first;
    let (mut count, mut vectors) = (0_i64, 0_i64);
    let mut tokens: BTreeMap<Part, i64> = BTreeMap::new();
    for (document, held) in documents.zip(ids) {
        forget(tx, index, &document.id)?;
        let doc = number(next)?;
        next += 1;
        for ((part, _, tf), key) in document.analyzed.terms.iter().zip(&held) {
            postings.push((*key, doc, *tf, document.analyzed.length(*part)));
        }
        for (part, len) in &document.analyzed.lengths {
            *tokens.entry(*part).or_default() += i64::from(*len);
        }
        keep(tx, (index, doc), document, held)?;
        count += 1;
        vectors += i64::from(document.vector.is_some());
    }
    segments::write_set(tx, index, (number(first)?, number(next)?, count as u64), &mut postings)?;
    for (part, len) in tokens {
        execute(tx, ADD_TOKENS, (index, part.0, len), "count a part's words")?;
    }
    let counted = (index, next, count, i64::from(new_words), vectors);
    execute(tx, JOIN_INDEX, counted, "count documents")?;
    Ok(learned)
}

/// The id of every key each of `documents` holds, in the order of its
/// terms: those `known` holds from it, the others read from their rows in
/// the order of the keys' index, a key kept for each word none held before;
/// whether any is a word as written that no key was kept for; and the keys
/// read from their rows or made. Each term is looked up once: three
/// lookups of each, its text hashed every time, were most of the 6.8 s a
/// profile of MS MARCO's first million passages found in a set's own code.
fn join_keys<'p>(
    tx: &Tx<'_>,
    index: i64,
    documents: impl Iterator<Item = &'p Prepared>,
    known: &Mutex<Known>,
) -> Result<(Vec<Vec<i64>>, bool, Learned)> {
    let known = lock(known);
    // a key's id, or −1 − its place among the keys `known` does not hold
    let mut placed: HashMap<(Part, &'p str), i64> = HashMap::new();
    let mut unknown: Vec<(Part, &'p str)> = Vec::new();
    let mut ids: Vec<Vec<i64>> = Vec::new();
    for document in documents {
        let held = document.analyzed.terms.iter().map(|(part, term, _)| {
            *placed.entry((*part, term.as_str())).or_insert_with(|| match known.get(*part, term) {
                Some(key) => key,
                None => {
                    unknown.push((*part, term.as_str()));
                    -(unknown.len() as i64)
                }
            })
        });
        ids.push(held.collect());
    }
    let mut learned = Learned { era: known.era, keys: Vec::with_capacity(unknown.len()) };
    drop(known);
    let (found, new_words) = find_keys(tx, index, &unknown, &mut learned)?;
    for id in ids.iter_mut().flatten().filter(|id| **id < 0) {
        *id = found[(-*id - 1) as usize];
    }
    Ok((ids, new_words, learned))
}

/// The ids of `unknown` keys, read in the order of the keys' index, a key
/// kept for each word none held before, each learned; and whether any is a
/// word as written that no key was kept for.
fn find_keys(tx: &Tx<'_>, index: i64, unknown: &[(Part, &str)], learned: &mut Learned) -> Result<(Vec<i64>, bool)> {
    let failed = |error| sql_error("write a key", error);
    let mut find = tx.prepare_cached(FIND_KEY).map_err(failed)?;
    let mut keep_new = tx.prepare_cached(NEW_KEY).map_err(failed)?;
    let mut order: Vec<usize> = (0..unknown.len()).collect();
    order.sort_unstable_by_key(|at| unknown[*at]);
    let (mut found, mut new_words) = (vec![0; unknown.len()], false);
    for at in order {
        let (part, term) = unknown[at];
        let key = match find.query_row((index, part.0, term), |row| row.get::<_, i64>(0)).optional().map_err(failed)? {
            Some(key) => key,
            None => {
                keep_new.execute((index, part.0, term)).map_err(failed)?;
                new_words |= part.is_written();
                tx.last_insert_rowid()
            }
        };
        found[at] = key;
        learned.keys.push((part, term.to_owned(), key));
    }
    Ok((found, new_words))
}

fn next_doc(tx: &Tx<'_>, index: i64) -> Result<i64> {
    tx.prepare_cached(NEXT_DOC)
        .and_then(|mut select| select.query_row([index], |row| row.get(0)))
        .map_err(|error| sql_error("number a document", error))
}

/// The number `next` as a document's, while the index has numbers left.
fn number(next: i64) -> Result<u32> {
    if next > LAST_DOC {
        return Err(
            DOCUMENT_NUMBERS.run_out(format!("the index gave its {LAST_DOC} numbers: a set takes a number each"))
        );
    }
    Ok(next as u32)
}

/// Keeps a document's row, numbered `doc`, holding `keys`, and its vector.
fn keep(tx: &Tx<'_>, (index, doc): (i64, u32), document: &Prepared, mut keys: Vec<i64>) -> Result<()> {
    keys.sort_unstable();
    let lengths: Vec<(u32, u32)> = document.analyzed.lengths.iter().map(|(part, len)| (part.0, *len)).collect();
    let row = (index, doc, &document.id, &document.value, encode_keys(&keys), encode_lengths(&lengths));
    execute(tx, KEEP_DOC, row, "keep a document")?;
    if let Some(vector) = &document.vector {
        vectors::keep(tx, index, doc, vector)?;
        if document.clustered {
            vectors::join_head(tx, index, doc)?;
        }
    }
    Ok(())
}

/// Deletes the document of `id`; says whether there was one.
pub(crate) fn delete(tx: &Tx<'_>, index: i64, id: &str) -> Result<bool> {
    forget(tx, index, id)
}

/// Takes the document of `id` out of every count, and out of the open
/// segment's pieces when a set wrote it there lately, so that a document
/// set again and again leaves no postings behind; else marks it gone, its
/// postings waiting in their pieces for a merge or a purge.
fn forget(tx: &Tx<'_>, index: i64, id: &str) -> Result<bool> {
    let failed = |error| sql_error("find a document", error);
    let found: Option<(i64, Vec<u8>, Vec<u8>)> = {
        let mut select = tx.prepare_cached(FIND_DOC).map_err(failed)?;
        let mut rows = select.query((index, id)).map_err(failed)?;
        match rows.next().map_err(failed)? {
            Some(row) => Some((row.get(0).map_err(failed)?, row.get(1).map_err(failed)?, row.get(2).map_err(failed)?)),
            None => None,
        }
    };
    let Some((doc, keys, lengths)) = found else { return Ok(false) };
    let within = |error: Error| error.within(format!("document {doc}"));
    let open = segments::open_of(tx, index)?.filter(|open| (open.first..open.past).contains(&(doc as u32)));
    let mut words_left = false;
    for key in decode_keys(&keys).map_err(within)? {
        // a piece taken from counts the document out itself
        if let Some(open) = &open
            && segments::take_out(tx, open, key, doc as u32)?
        {
            continue;
        }
        let (part, documents) = leave_key(tx, key)?;
        words_left |= documents == 0 && part.is_written();
    }
    for (part, len) in decode_lengths(&lengths).map_err(within)? {
        execute(tx, ADD_TOKENS, (index, part, -i64::from(len)), "count a part's words")?;
    }
    if open.is_none() {
        execute(tx, MARK_GONE, (index, doc, &keys), "mark a document gone")?;
    }
    execute(tx, DROP_DOC, (index, doc), "drop a document")?;
    let had_vector = vectors::forget(tx, index, doc as u32)?;
    execute(tx, LEAVE_INDEX, (index, i64::from(words_left), i64::from(had_vector)), "count a document")?;
    Ok(true)
}

/// A key one fewer document holds: its part, and how many hold it now.
fn leave_key(connection: &Connection, key: i64) -> Result<(Part, i64)> {
    let failed = |error| sql_error(format!("write key {key}"), error);
    let mut select = connection.prepare_cached(READ_KEY).map_err(failed)?;
    let (part, held): (u32, i64) = select.query_row([key], |row| Ok((row.get(0)?, row.get(1)?))).map_err(failed)?;
    execute(connection, LEAVE_KEY, [key], "write a key")?;
    Ok((Part(part), held - 1))
}

pub(crate) use crate::sqlite::execute_cached as execute;
