//! A write on a copy of search.db, passed to the file's home: the index by
//! its name and each document as its JSON, run there on the index as it is.
//!
//! ```text
//! name         the index's
//! u8 call      0 a set of documents, 1 a delete
//! a set        documents, each its id, its JSON, then u8 0 nothing said of
//!              its vector, 1 it has none, 2 its floats follow
//! a delete     the id
//! ```

use serde::Serialize;

use super::index::{Given, open_as_it_is};
use crate::replication::{In, Out};
use crate::{Error, ErrorKind, Result, Store};

/// Opens the engine at the home, which makes its file for a copy waiting
/// for it.
pub(crate) fn make(store: &Store) -> Result<()> {
    super::engine::Search::of(store).map(drop)
}

/// search's file, where its home passes writes.
pub(crate) const FILE: &str = "search.db";

/// A document as it passes on.
pub(super) struct Passed {
    id: String,
    json: Vec<u8>,
    vector: Option<Option<Vec<f32>>>,
}

impl Passed {
    pub(super) fn of<T: Serialize>(given: &Given<'_, T>) -> Result<Passed> {
        let json = serde_json::to_vec(given.value)
            .map_err(|error| Error::invalid(format!("a document that is no JSON: {error}")))?;
        Ok(Passed { id: given.id.clone(), json, vector: given.vector.map(|vector| vector.map(<[f32]>::to_vec)) })
    }
}

/// Passes a set of `documents` in the index `name` to the home of the copy
/// search's file is.
pub(super) fn pass_on_set(store: &Store, name: &str, documents: &[Passed]) -> Result<()> {
    let mut out = Out(vec![crate::replication::SEARCH]);
    out.text(name).byte(0).list(documents, |out, document| {
        out.text(&document.id).bytes(&document.json);
        match &document.vector {
            None => out.byte(0),
            Some(None) => out.byte(1),
            Some(Some(vector)) => out.byte(2).list(vector, |out, x| {
                out.word(x.to_bits());
            }),
        };
    });
    crate::replication::pass_on(store, FILE, out.0).map(drop)
}

/// Passes a delete of the document `id` to the home.
pub(super) fn pass_on_delete(store: &Store, name: &str, id: &str) -> Result<bool> {
    let mut out = Out(vec![crate::replication::SEARCH]);
    out.text(name).byte(1).text(id);
    let answer = crate::replication::pass_on(store, FILE, out.0)?;
    In { bytes: &answer, at: 0 }.byte().map(|found| found == 1)
}

/// Runs at the home a write another node passed on, on the index as it is.
pub(crate) fn run(store: &Store, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut at = In { bytes, at: 0 };
    let name = at.text()?;
    let index = open_as_it_is::<serde_json::Value>(store, &name)?;
    match at.byte()? {
        0 => {
            let documents = at.list(1 << 12, |at| {
                let id = at.text()?;
                let value: serde_json::Value =
                    serde_json::from_slice(at.bytes()?).map_err(|_| corrupt("a document's JSON"))?;
                let vector = match at.byte()? {
                    0 => None,
                    1 => Some(None),
                    2 => Some(Some(at.list(1 << 16, |at| at.word().map(f32::from_bits))?)),
                    _ => return Err(corrupt("a document's vector")),
                };
                Ok((id, value, vector))
            })?;
            let given = documents
                .iter()
                .map(|(id, value, vector)| Given {
                    id: id.clone(),
                    value,
                    vector: vector.as_ref().map(|v| v.as_deref()),
                })
                .collect();
            index.set_all(given)?;
            Ok(Vec::new())
        }
        1 => {
            let found = index.delete(at.text()?)?;
            Ok(vec![u8::from(found)])
        }
        _ => Err(corrupt("a call")),
    }
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a search write passed on: {what}"))
}
