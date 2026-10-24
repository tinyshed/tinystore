//! What the async API's calls of an index stand on, `tinystore::aio::search`:
//! a set and a delete in completion form, a search's hits by the query its
//! call made, and a wait for a rebuild that holds no thread.
//!
//! A write's form runs where its awaited future is first polled, inside
//! `polled::leading_on`: it holds the index's rows without waiting, makes the
//! documents' words and submits them, and the commit is led on the store's
//! pool. What would wait there, a change that holds the rows or a copy's
//! home, it hands to the pool, through [`elsewhere`](super::engine::elsewhere),
//! and so it does documents of so many bytes that their words take long to
//! make.

#[cfg(feature = "replication")]
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[cfg(feature = "replication")]
use super::engine::elsewhere;
#[cfg(feature = "replication")]
use super::forward::{self, Passed};
use super::index::{Found, Index, MOST_MANY, ToSet, checked, json, prepared_many};
#[cfg(feature = "replication")]
use super::index::{Given, pass_on_documents};
use super::open::Data;
use super::read::Query;
use super::write::{Incoming, Prepared};
use crate::{Error, Result, Store};

/// A document an awaited set owns, made when its call is: its id, its value
/// as JSON or why it has none, and what the call said of its vector.
pub(crate) struct Encoded {
    id: String,
    json: Result<(Value, Vec<u8>)>,
    vector: Option<Option<Vec<f32>>>,
}

impl Encoded {
    pub(crate) fn new<T: Serialize>(id: String, value: &T, vector: Option<Option<&[f32]>>) -> Encoded {
        Encoded { id, json: json(value), vector: vector.map(|vector| vector.map(<[f32]>::to_vec)) }
    }

    /// Its bytes as JSON: none where it has no JSON, which its set refuses.
    fn json_bytes(&self) -> usize {
        self.json.as_ref().map_or(0, |(_, bytes)| bytes.len())
    }

    /// The document as a copy passes it to its home.
    #[cfg(feature = "replication")]
    fn passed(self) -> Result<Passed> {
        let (tree, _) = self.json?;
        Passed::of(&Given { id: self.id, value: &tree, vector: self.vector.as_ref().map(Option::as_deref) })
    }
}

impl ToSet for Encoded {
    fn id(&self) -> &str {
        &self.id
    }

    fn prepared(self, data: &Data) -> Result<Prepared> {
        let (tree, bytes) = self.json?;
        checked(&self.id)?;
        let incoming = Incoming { id: self.id, tree, bytes, vector: self.vector.as_ref().map(Option::as_deref) };
        Prepared::new(data.id, incoming, &data.settings)
    }
}

impl<T> Index<T> {
    /// [`set`](Index::set) for an awaited set: `done` is called with the
    /// answer where its commit runs, or at once where it is refused first.
    pub(crate) fn set_then(&self, document: Encoded, done: impl FnOnce(Result<()>) + Send + 'static) {
        let named = format!("index {}: id {:?}", self.open.name, document.id);
        let done = move |set: Result<()>| done(set.map_err(|error| error.within(named)));
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            let (search, name) = (Arc::clone(&self.search), self.open.name.clone());
            return elsewhere(move || done(pass_on_documents(&search, &name, || Ok(vec![document.passed()?]))));
        }
        let json = document.json_bytes();
        let document = move |data: &Data| Ok(vec![document.prepared(data)?]);
        self.search.set_then(&self.open, json, document, done);
    }

    /// [`set_many`](Index::set_many) for an awaited one, all of `documents`
    /// in one commit or none, as [`set_then`](Self::set_then) sets one.
    pub(crate) fn set_many_then(&self, documents: Vec<Encoded>, done: impl FnOnce(Result<()>) + Send + 'static) {
        let named = format!("index {}", self.open.name);
        let done = move |set: Result<()>| done(set.map_err(|error| error.within(named)));
        if documents.len() > MOST_MANY {
            return done(Err(Error::limit(format!("a set_many of {} documents, past {MOST_MANY}", documents.len()))));
        }
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            let (search, name) = (Arc::clone(&self.search), self.open.name.clone());
            let passed = move || documents.into_iter().map(Encoded::passed).collect();
            return elsewhere(move || done(pass_on_documents(&search, &name, passed)));
        }
        let json = documents.iter().map(Encoded::json_bytes).sum();
        self.search.set_then(&self.open, json, move |data: &Data| prepared_many(documents, data), done);
    }

    /// [`delete`](Index::delete) for an awaited delete, as
    /// [`set_then`](Self::set_then) sets.
    pub(crate) fn delete_then(&self, id: String, done: impl FnOnce(Result<bool>) + Send + 'static) {
        let named = format!("index {}: id {id:?}", self.open.name);
        let done = move |deleted: Result<bool>| done(deleted.map_err(|error| error.within(named)));
        if let Err(refused) = checked(&id) {
            return done(Err(refused));
        }
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            let (search, name) = (Arc::clone(&self.search), self.open.name.clone());
            let passed = move || forward::pass_on_delete(&search.store()?, &name, &id);
            return elsewhere(move || done(passed()));
        }
        self.search.delete_then(&self.open, id, done);
    }

    /// Whether the index serves the settings it was opened with, read once:
    /// false while it is read again, and an error naming why a rebuild failed.
    pub(crate) fn is_ready(&self) -> Result<bool> {
        self.search.is_ready(&self.open).map_err(|error| self.within(error))
    }

    /// Whether a wait for the index runs its maintenance itself, as one does
    /// on a store opened without background work.
    pub(crate) fn maintains_itself(&self) -> bool {
        !self.search.runs_in_background()
    }

    /// One pass of the maintenance a wait for the index runs itself.
    pub(crate) fn maintain_once(&self) -> Result<()> {
        self.search.maintain(true).map(drop).map_err(|error| self.within(error))
    }

    /// Calls `done` at the index's next settle, or once a moment passed on
    /// the store's timer, for an awaited wait that reads its status again.
    pub(crate) fn settled_then(&self, done: impl FnOnce(Result<()>) + Send + 'static) {
        let named = format!("index {}", self.open.name);
        self.search.settled_then(&self.open, move |settled| done(settled.map_err(|error| error.within(named))));
    }

    /// The store the index opened in, while it is open: what an async handle
    /// made from this one takes the store's pool from.
    pub(crate) fn store(&self) -> Option<Store> {
        self.search.store().ok()
    }

    fn within(&self, error: Error) -> Error {
        error.within(format!("index {}", self.open.name))
    }
}

impl<T: DeserializeOwned> Index<T> {
    /// The hits of `query`, which a search's call made and checked, for an
    /// awaited search that runs it on the store's pool.
    pub(crate) fn run_query(&self, query: Query) -> Result<Found<T>> {
        #[cfg(feature = "sql")]
        if let Some(read_rows) = &self.read_rows {
            return super::table_index::found(self, &**read_rows, query, None).map_err(|error| self.within(error));
        }
        self.found(query).map_err(|error| self.within(error))
    }
}
