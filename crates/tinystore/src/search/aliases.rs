//! A document's aliases: a word that stands for the whole text of one of
//! its fields, in that document alone, as a synonym stands for its phrase in
//! every document of an index.
//!
//! ```text
//! set "1" { name: "Криптографические методы защиты информации" }, aliases { кмзи: name }
//!   its postings: the name's words, and кмзи in the name's parts, counted in no length
//!   a search for кмзи finds "1" by its name, and marks the name whole
//! set "2" { name: "Теория принятия решений" }, aliases { тпр: name }
//!   кмзи finds "1" alone; a set of "1" again without aliases takes кмзи out of it
//! ```
//!
//! An alias is a word of the document's own postings, so a change of one is
//! the set of its document and reads nothing else again. Its rows keep the
//! pairs as the set gave them, by the document's number, which a rebuild
//! reads with the document and a set again or a delete takes with it.

use rusqlite::Connection;

use super::analyzer::Word;
use super::settings::{Field, shown};
use super::synonyms::one_word;
use crate::sqlite::{Tx, execute_cached as execute, sql_error};
use crate::{Error, Result};

/// The aliases a document holds at most.
const MOST_ALIASES: usize = 64;

const KEEP: &str = "insert into _tinystore_search_aliases (index_id, doc, alias, field) values (?1, ?2, ?3, ?4)";
const FORGET: &str = "delete from _tinystore_search_aliases where index_id = ?1 and doc = ?2";
const READ: &str = "select alias, field from _tinystore_search_aliases where index_id = ?1 and doc = ?2";

/// A document's aliases: each pair as its set gave it, the alias and the
/// field's path, sorted; and the word of each that a field of the index
/// reads, with that field's path.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Aliases {
    given: Vec<(String, String)>,
    held: Vec<(String, Word)>,
}

impl Aliases {
    /// The aliases a set gives, checked against the index's `fields`: at
    /// most 64, each one word of 64 bytes at most, for a field the index
    /// reads, and no alias twice.
    pub(crate) fn new(mut given: Vec<(String, String)>, fields: &[Field]) -> Result<Aliases> {
        if given.len() > MOST_ALIASES {
            return Err(Error::invalid(format!("{} aliases, past {MOST_ALIASES}", given.len())));
        }
        given.sort_unstable();
        if let Some(twice) = given.windows(2).find(|pairs| pairs[0].0 == pairs[1].0) {
            return Err(Error::invalid(format!("alias {:?}: given twice", twice[0].0)));
        }
        let mut held = Vec::with_capacity(given.len());
        for (alias, field) in &given {
            let refused = |why: String| Error::invalid(format!("alias {alias:?} for {}: {why}", shown(field)));
            if !fields.iter().any(|read| read.path == *field) {
                let read: Vec<&str> = fields.iter().map(|field| field.path.as_str()).collect();
                return Err(refused(format!("a field the index does not read: {read:?}")));
            }
            held.push((field.clone(), one_word(alias, "an alias").map_err(refused)?));
        }
        Ok(Aliases { given, held })
    }

    /// The aliases a document's rows keep, read by the index's `fields` as
    /// they are now, without a set's checks: a pair for a field it reads no
    /// more, or a word a later analyzer reads as two, stays and finds
    /// nothing, so that a rebuild never fails for it and a later one with
    /// the field again finds by it again.
    pub(crate) fn kept(given: Vec<(String, String)>, fields: &[Field]) -> Aliases {
        let read = |(alias, field): &(String, String)| {
            let word = one_word(alias, "an alias").ok()?;
            fields.iter().any(|read| read.path == *field).then(|| (field.clone(), word))
        };
        let held = given.iter().filter_map(read).collect();
        Aliases { given, held }
    }

    /// The words that stand for the text of the field `path`.
    pub(crate) fn of_field<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a Word> {
        self.held.iter().filter(move |(field, _)| field == path).map(|(_, word)| word)
    }

    /// What its rows add to a commit, about.
    pub(crate) fn bytes(&self) -> usize {
        self.given.iter().map(|(alias, field)| alias.len() + field.len() + 16).sum()
    }

    /// Keeps the rows of the aliases of the document numbered `doc`.
    pub(crate) fn keep(&self, tx: &Tx<'_>, (index, doc): (i64, u32)) -> Result<()> {
        for (alias, field) in &self.given {
            execute(tx, KEEP, (index, doc, alias, field), "keep a document's aliases")?;
        }
        Ok(())
    }
}

/// Drops the rows of the aliases of the document numbered `doc`.
pub(crate) fn forget(tx: &Tx<'_>, (index, doc): (i64, i64)) -> Result<()> {
    execute(tx, FORGET, (index, doc), "drop a document's aliases").map(drop)
}

/// The aliases of the document numbered `doc` as its set gave them, each
/// the alias and its field's path; none for a document set without.
pub(crate) fn read(connection: &Connection, (index, doc): (i64, u32)) -> Result<Vec<(String, String)>> {
    let failed = |error| sql_error(format!("read the aliases of document {doc}"), error);
    let mut select = connection.prepare_cached(READ).map_err(failed)?;
    let rows = select.query_map((index, doc), |row| Ok((row.get(0)?, row.get(1)?))).map_err(failed)?;
    rows.collect::<rusqlite::Result<_>>().map_err(failed)
}

#[cfg(test)]
#[path = "aliases_tests.rs"]
mod tests;
