//! Sets and deletes, each a grouped write of search.db.
//!
//! A set gives its document a new number and writes its postings to the
//! head, where searches read them beside the blocks, so the head's
//! documents always come after every block's and packing appends to each
//! key's list. The number the document had is marked gone: searches leave
//! it out of every block until a purge takes it out.
//!
//! A document's vector is kept beside it and goes with it: a set keeps the
//! new number's and forgets the old one's, so a scan of vectors never meets
//! a document gone.
//!
//! Every count a score reads changes here and nowhere else: how many
//! documents the index holds, how many hold each key, how many words each
//! part has. Packing and purging move postings and change no count, so they
//! change no score.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::document::{Analyzed, analyze};
use super::postings::{decode_keys, decode_lengths, encode_keys, encode_lengths};
use super::score::Part;
use super::settings::Settings;
use super::vectors::{self, Vector};
use crate::limits::Limit;
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const FIND_DOC: &str = "select doc, keys, lengths from _tinystore_search_docs where index_id = ?1 and id = ?2";
const MARK_GONE: &str = "insert into _tinystore_search_gone (index_id, doc, keys) values (?1, ?2, ?3)";
const DROP_DOC: &str = "delete from _tinystore_search_docs where index_id = ?1 and doc = ?2";
const READ_KEY: &str = "select part, documents from _tinystore_search_keys where id = ?1";
const LEAVE_KEY: &str = "update _tinystore_search_keys set documents = documents - 1 where id = ?1";
const DROP_HEAD: &str = "delete from _tinystore_search_head where index_id = ?1 and key = ?2 and doc = ?3";
const ADD_TOKENS: &str = "insert into _tinystore_search_parts (index_id, part, tokens) values (?1, ?2, ?3)
    on conflict (index_id, part) do update set tokens = tokens + excluded.tokens";
const LEAVE_INDEX: &str =
    "update _tinystore_search_indexes set documents = documents - 1, gone_version = gone_version + 1,
    words_version = words_version + ?2, vectors = vectors - ?3 where id = ?1";
const NEXT_DOC: &str = "select next_doc from _tinystore_search_indexes where id = ?1";
// A key is found by its term and counted by its id, with no `returning`: over
// 50,000 keys an update by the term returning its row took 2.15 µs, a find
// 0.51 and an update by the id 0.25.
const FIND_KEY: &str =
    "select id, documents from _tinystore_search_keys where index_id = ?1 and part = ?2 and term = ?3";
const COUNT_KEY: &str = "update _tinystore_search_keys set documents = documents + ?2 where id = ?1";
const NEW_KEY: &str = "insert into _tinystore_search_keys (index_id, part, term, documents) values (?1, ?2, ?3, ?4)";
const ADD_HEAD: &str = "insert into _tinystore_search_head (index_id, key, doc, tf, len) values (?1, ?2, ?3, ?4, ?5)";
/// Postings a statement writes into the head, five values each.
const HEAD_ROWS: usize = 64;
static ADD_HEAD_ROWS: LazyLock<String> = LazyLock::new(|| {
    let rows = vec!["(?, ?, ?, ?, ?)"; HEAD_ROWS].join(", ");
    format!("insert into _tinystore_search_head (index_id, key, doc, tf, len) values {rows}")
});
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

/// Sets documents of one index in their order, each replacing the one of its
/// id. What they change is written once for them all rather than once a
/// document: a key's count once however many of them hold it, the index's
/// counts and each part's words once, and their postings into the head in
/// the order of its keys, many a statement. A set of 1,000 passages of 50
/// words wrote some 80 statements a document before.
pub(crate) fn set_all<'p>(tx: &Tx<'_>, documents: impl Iterator<Item = &'p Prepared> + Clone) -> Result<()> {
    let Some(index) = documents.clone().next().map(|document| document.index) else { return Ok(()) };
    let (keys, new_words) = join_keys(tx, index, documents.clone())?;
    let mut head = Head::default();
    let mut next = next_doc(tx, index)?;
    let (mut count, mut vectors) = (0_i64, 0_i64);
    let mut tokens: BTreeMap<Part, i64> = BTreeMap::new();
    let mut set_now: HashSet<&str> = HashSet::new();
    for document in documents {
        if !set_now.insert(&document.id) {
            // the forget below finds the postings this batch gave its id
            head.write(tx, index)?;
        }
        forget(tx, index, &document.id)?;
        let doc = number(next)?;
        next += 1;
        let mut held = Vec::with_capacity(document.analyzed.terms.len());
        for (part, term, tf) in &document.analyzed.terms {
            let key = keys[&(*part, term.as_str())];
            head.rows.push((key, doc, *tf, document.analyzed.length(*part)));
            held.push(key);
        }
        for (part, len) in &document.analyzed.lengths {
            *tokens.entry(*part).or_default() += i64::from(*len);
        }
        keep(tx, (index, doc), document, held)?;
        count += 1;
        vectors += i64::from(document.vector.is_some());
    }
    head.write(tx, index)?;
    for (part, len) in tokens {
        execute(tx, ADD_TOKENS, (index, part.0, len), "count a part's words")?;
    }
    let counted = (index, next, count, i64::from(new_words), vectors);
    execute(tx, JOIN_INDEX, counted, "count documents").map(drop)
}

/// Keys' ids by their part and term.
type Keys<'p> = HashMap<(Part, &'p str), i64>;

/// The id of every key `documents` hold, each counted once for every one
/// that holds it, in the order of the keys' index; and whether any is a
/// word as written no document held before.
fn join_keys<'p>(tx: &Tx<'_>, index: i64, documents: impl Iterator<Item = &'p Prepared>) -> Result<(Keys<'p>, bool)> {
    let mut holding: BTreeMap<(Part, &str), i64> = BTreeMap::new();
    for document in documents {
        for (part, term, _) in &document.analyzed.terms {
            *holding.entry((*part, term.as_str())).or_default() += 1;
        }
    }
    let failed = |error| sql_error("write a key", error);
    let mut find = tx.prepare_cached(FIND_KEY).map_err(failed)?;
    let mut count_more = tx.prepare_cached(COUNT_KEY).map_err(failed)?;
    let mut keep_new = tx.prepare_cached(NEW_KEY).map_err(failed)?;
    let mut keys = HashMap::with_capacity(holding.len());
    let mut new_words = false;
    for ((part, term), count) in holding {
        let found = find.query_row((index, part.0, term), |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)));
        let key = match found.optional().map_err(failed)? {
            Some((key, held)) => {
                count_more.execute((key, count)).map_err(failed)?;
                new_words |= held == 0 && part.is_written();
                key
            }
            None => {
                keep_new.execute((index, part.0, term, count)).map_err(failed)?;
                new_words |= part.is_written();
                tx.last_insert_rowid()
            }
        };
        keys.insert((part, term), key);
    }
    Ok((keys, new_words))
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

/// Postings of a set waiting for the head, as (key, doc, tf, len).
#[derive(Default)]
struct Head {
    rows: Vec<(i64, u32, u32, u32)>,
}

impl Head {
    /// Writes the postings waiting in the order of the head's keys,
    /// `HEAD_ROWS` a statement, each insert near the one before it.
    fn write(&mut self, tx: &Tx<'_>, index: i64) -> Result<()> {
        self.rows.sort_unstable();
        let failed = |error| sql_error("write postings", error);
        let mut many = tx.prepare_cached(&ADD_HEAD_ROWS).map_err(failed)?;
        let (chunks, rest) = self.rows.as_chunks::<HEAD_ROWS>();
        for chunk in chunks {
            let values = chunk
                .iter()
                .flat_map(|&(key, doc, tf, len)| [index, key, i64::from(doc), i64::from(tf), i64::from(len)]);
            many.execute(rusqlite::params_from_iter(values)).map_err(failed)?;
        }
        for &(key, doc, tf, len) in rest {
            execute(tx, ADD_HEAD, (index, key, doc, tf, len), "write a posting")?;
        }
        self.rows.clear();
        Ok(())
    }
}

/// Deletes the document of `id`; says whether there was one.
pub(crate) fn delete(tx: &Tx<'_>, index: i64, id: &str) -> Result<bool> {
    forget(tx, index, id)
}

/// Marks the document of `id` gone and takes it out of every count and of
/// the head; its postings in blocks wait for a purge.
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
    let mut words_left = false;
    for key in decode_keys(&keys).map_err(within)? {
        let (part, documents) = leave_key(tx, key)?;
        words_left |= documents == 0 && part.is_written();
        execute(tx, DROP_HEAD, (index, key, doc), "drop a posting")?;
    }
    for (part, len) in decode_lengths(&lengths).map_err(within)? {
        execute(tx, ADD_TOKENS, (index, part, -i64::from(len)), "count a part's words")?;
    }
    execute(tx, MARK_GONE, (index, doc, &keys), "mark a document gone")?;
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
