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

use rusqlite::Connection;

use super::document::Analyzed;
use super::postings::{decode_keys, decode_lengths, encode_keys, encode_lengths};
use super::score::Part;
use super::vectors::{self, Vector};
use crate::sqlite::{Tx, sql_error};
use crate::{Error, Result};

const FIND_DOC: &str = "select doc, keys, lengths from _tinystore_search_docs where index_id = ?1 and id = ?2";
const MARK_GONE: &str = "insert into _tinystore_search_gone (index_id, doc, keys) values (?1, ?2, ?3)";
const DROP_DOC: &str = "delete from _tinystore_search_docs where index_id = ?1 and doc = ?2";
const LEAVE_KEY: &str =
    "update _tinystore_search_keys set documents = documents - 1 where id = ?1 returning part, documents";
const DROP_HEAD: &str = "delete from _tinystore_search_head where index_id = ?1 and key = ?2 and doc = ?3";
const ADD_TOKENS: &str = "insert into _tinystore_search_parts (index_id, part, tokens) values (?1, ?2, ?3)
    on conflict (index_id, part) do update set tokens = tokens + excluded.tokens";
const LEAVE_INDEX: &str =
    "update _tinystore_search_indexes set documents = documents - 1, gone_version = gone_version + 1,
    words_version = words_version + ?2 where id = ?1";
const NEXT_DOC: &str = "select next_doc from _tinystore_search_indexes where id = ?1";
const JOIN_KEY: &str = "insert into _tinystore_search_keys (index_id, part, term, documents) values (?1, ?2, ?3, 1)
    on conflict (index_id, part, term) do update set documents = documents + 1 returning id, documents";
const ADD_HEAD: &str = "insert into _tinystore_search_head (index_id, key, doc, tf, len) values (?1, ?2, ?3, ?4, ?5)";
const KEEP_DOC: &str =
    "insert into _tinystore_search_docs (index_id, doc, id, value, keys, lengths) values (?1, ?2, ?3, ?4, ?5, ?6)";
const JOIN_INDEX: &str = "update _tinystore_search_indexes set next_doc = ?2 + 1, documents = documents + 1,
    words_version = words_version + ?3 where id = ?1";

/// The last number an index gives: past it, [`END`](super::execute::END).
const LAST_DOC: i64 = u32::MAX as i64 - 1;

/// A document ready to set: its id, its value as JSON, its words and its
/// vector, if it has one.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub(crate) index: i64,
    pub(crate) id: String,
    pub(crate) value: Vec<u8>,
    pub(crate) analyzed: Analyzed,
    pub(crate) vector: Option<Vector>,
}

impl Prepared {
    /// What the write adds to its commit, about.
    pub(crate) fn bytes(&self) -> usize {
        let words: usize = self.analyzed.terms.iter().map(|(_, term, _)| term.len() + 16).sum();
        self.value.len() + words + self.vector.as_ref().map_or(0, Vector::size)
    }
}

/// Sets a document, replacing the one of its id.
pub(crate) fn set(tx: &Tx<'_>, document: &Prepared) -> Result<()> {
    let index = document.index;
    forget(tx, index, &document.id)?;
    let doc: i64 = tx
        .prepare_cached(NEXT_DOC)
        .and_then(|mut select| select.query_row([index], |row| row.get(0)))
        .map_err(|error| sql_error("number a document", error))?;
    if doc > LAST_DOC {
        return Err(Error::limit(format!("the index gave its {LAST_DOC} numbers: a set takes a number each")));
    }
    let mut keys = Vec::with_capacity(document.analyzed.terms.len());
    let mut new_words = false;
    for (part, term, tf) in &document.analyzed.terms {
        let (key, documents) = join_key(tx, index, *part, term)?;
        new_words |= documents == 1 && part.is_written();
        let len = document.analyzed.length(*part);
        execute(tx, ADD_HEAD, (index, key, doc, tf, len), "write a posting")?;
        keys.push(key);
    }
    for (part, len) in &document.analyzed.lengths {
        execute(tx, ADD_TOKENS, (index, part.0, len), "count a part's words")?;
    }
    keys.sort_unstable();
    let lengths: Vec<(u32, u32)> = document.analyzed.lengths.iter().map(|(part, len)| (part.0, *len)).collect();
    let row = (index, doc, &document.id, &document.value, encode_keys(&keys), encode_lengths(&lengths));
    execute(tx, KEEP_DOC, row, "keep a document")?;
    if let Some(vector) = &document.vector {
        vectors::keep(tx, index, doc as u32, vector)?;
    }
    execute(tx, JOIN_INDEX, (index, doc, i64::from(new_words)), "count a document")
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
    vectors::forget(tx, index, doc as u32)?;
    execute(tx, LEAVE_INDEX, (index, i64::from(words_left)), "count a document")?;
    Ok(true)
}

/// A key one more document holds: its id, and how many hold it now.
fn join_key(connection: &Connection, index: i64, part: Part, term: &str) -> Result<(i64, i64)> {
    let mut upsert = connection.prepare_cached(JOIN_KEY).map_err(|error| sql_error("write a key", error))?;
    let joined = upsert.query_row((index, part.0, term), |row| Ok((row.get(0)?, row.get(1)?)));
    joined.map_err(|error| sql_error("write a key", error))
}

/// A key one fewer document holds: its part, and how many hold it now.
fn leave_key(connection: &Connection, key: i64) -> Result<(Part, i64)> {
    let mut update = connection.prepare_cached(LEAVE_KEY).map_err(|error| sql_error("write a key", error))?;
    let left = update.query_row([key], |row| Ok((Part(row.get(0)?), row.get(1)?)));
    left.map_err(|error| sql_error(format!("write key {key}"), error))
}

/// Runs a statement the connection keeps prepared; `what` says it in an error.
pub(crate) fn execute(connection: &Connection, sql: &str, params: impl rusqlite::Params, what: &str) -> Result<()> {
    let ran = connection.prepare_cached(sql).and_then(|mut statement| statement.execute(params));
    ran.map(drop).map_err(|error| sql_error(what, error))
}
