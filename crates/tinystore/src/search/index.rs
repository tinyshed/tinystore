//! An index's handle: documents set by id, found again by their words, their
//! vectors or both.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::analyzer;
use super::engine::Search;
use super::filter::{self, Condition};
use super::open::{Data, OpenIndex, Status};
use super::read::Query;
use super::settings::{Settings, Wanted, check_hybrid};
#[cfg(feature = "sql")]
use super::table_index::ReadRow;
use super::vectors::Question;
use super::write::{Incoming, Prepared};
#[cfg(feature = "sql")]
use crate::Transaction;
use crate::{Error, Key, Result, Store};

/// The largest document, as its JSON.
const MOST_DOCUMENT: usize = 1 << 20;
/// The longest id, as kv keeps a key.
const MOST_ID: usize = 1 << 10;
/// The longest text a search reads, and the words of it it reads.
const MOST_TEXT: usize = 1 << 10;
const MOST_WORDS: usize = 32;
/// The hits a search reaches at most, `offset` and `limit` together.
const MOST_HITS: usize = 1000;
/// The hits a search answers unless it says.
const LIMIT: usize = 10;
/// The documents a `set_many` takes at most, and their bytes as JSON, so
/// that no import makes one commit and a write-ahead log of its size.
const MOST_MANY: usize = 1000;
const MOST_MANY_BYTES: usize = 16 << 20;

/// What [`Store::index`] opens: an index's settings, given where it opens.
#[must_use = "an index opens at `open`"]
pub struct IndexBuilder<'s, T> {
    store: &'s Store,
    name: String,
    wanted: Wanted,
    _value: PhantomData<fn() -> T>,
}

impl<T> std::fmt::Debug for IndexBuilder<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBuilder").field("name", &self.name).field("wanted", &self.wanted).finish()
    }
}

impl<'s, T: Serialize + DeserializeOwned> IndexBuilder<'s, T> {
    pub(crate) fn new(store: &'s Store, name: &str) -> Self {
        IndexBuilder { store, name: name.to_owned(), wanted: Wanted::default(), _value: PhantomData }
    }

    /// A field whose text is read, by its path into the document, `author.name`
    /// for a field of a field, and what a word found there weighs: a word in
    /// a field of 2 scores twice what it would in a field of 1. An index of
    /// no field reads a document that is a string as its text.
    pub fn field(mut self, path: &str, weight: f64) -> Self {
        self.wanted.fields.push((path.to_owned(), weight));
        self
    }

    /// Fields that each weigh 1.
    pub fn fields<S: AsRef<str>>(mut self, paths: impl IntoIterator<Item = S>) -> Self {
        self.wanted.fields.extend(paths.into_iter().map(|path| (path.as_ref().to_owned(), 1.0)));
        self
    }

    /// The fields a search may keep its hits to, each a path into the
    /// document whose value is text, a number, a boolean or a list of them;
    /// the index keeps their values and no other field's.
    pub fn filters<S: AsRef<str>>(mut self, paths: impl IntoIterator<Item = S>) -> Self {
        self.wanted.filters.extend(paths.into_iter().map(|path| path.as_ref().to_owned()));
        self
    }

    /// Whether a misspelt word is read as the index's nearest: on unless
    /// turned off.
    pub fn typo_tolerance(mut self, on: bool) -> Self {
        self.wanted.typo_tolerance = on;
        self
    }

    /// Keeps a vector of `dimensions` numbers with each document, which the
    /// program's own model gives it, and finds documents by their cosine
    /// with a search's.
    pub fn vectors(mut self, dimensions: usize) -> Self {
        self.wanted.dimensions = Some(dimensions);
        self
    }

    /// Searches by a vector read the clusters nearest their question rather
    /// than every vector, and find `recall` of the exact ten on average,
    /// from above 0 to below 1: the clusters are built in the background,
    /// and searches stay exact until then. How many clusters a search reads
    /// is the index's to choose, from questions drawn from its own vectors.
    pub fn recall(mut self, recall: f64) -> Self {
        self.wanted.recall = Some(recall);
        self
    }

    /// What a vector weighs against the words in a search by both, from 0,
    /// the words alone, to 1, the vector alone; 0.5 unless set, and a
    /// search may give its own.
    pub fn vector_weight(mut self, weight: f64) -> Self {
        self.wanted.vector_weight = Some(weight);
        self
    }

    /// Makes a search by both order the hits of its words by their vectors,
    /// finding nothing its words do not, unless it gives a weight.
    pub fn rerank(mut self) -> Self {
        self.wanted.rerank = true;
        self
    }

    /// Whether a change of settings that needs the index read again begins
    /// by itself, as it does unless said, or waits for [`reindex`](Self::reindex),
    /// for a program that schedules its own load.
    pub fn rebuild(mut self, rebuild: Rebuild) -> Self {
        self.wanted.manual = rebuild == Rebuild::Manual;
        self
    }

    /// Opens the index, made the first time. An index that holds other
    /// settings is changed to these, each change said in the log: a weight,
    /// a field or a filter removed, typo tolerance, vectors added, recall
    /// and how a search by both combines them at once; a field or a filter
    /// added to an index that holds documents by reading them again beside
    /// it, in the background, the index answering as before until then.
    /// Vectors of other dimensions, or none where it keeps some, are
    /// refused. Opening never waits for the work a change needs.
    pub fn open(self) -> Result<Index<T>> {
        self.opened(false)
    }

    /// Opens the index as [`open`](Self::open) does, and reads it again
    /// beside itself in the background: a change [`Rebuild::Manual`] left
    /// waiting, or the documents numbered again.
    pub fn reindex(self) -> Result<Index<T>> {
        self.opened(true)
    }

    fn opened(self, reindex: bool) -> Result<Index<T>> {
        let within = |error: Error| error.within(format!("index {}", self.name));
        let search = Search::of(self.store).map_err(within)?;
        let open = crate::sqlite::own_work(|| search.open_index(&self.name, &self.wanted, reindex)).map_err(within)?;
        Ok(Index::of_documents(search, open))
    }
}

/// The index `name` as it is, for a connection that may only read, or for
/// the writes a copy passed on: none is made and no setting changed.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) fn open_as_it_is<T: Serialize + DeserializeOwned>(store: &Store, name: &str) -> Result<Index<T>> {
    let within = |error: Error| error.within(format!("index {name}"));
    let search = Search::of(store).map_err(within)?;
    let open = search.open_existing(name).map_err(within)?;
    Ok(Index::of_documents(search, open))
}

/// Whether a change of an index's settings that needs it read again begins
/// by itself or waits for [`IndexBuilder::reindex`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rebuild {
    #[default]
    Automatic,
    Manual,
}

impl<'s, T: Serialize + DeserializeOwned> IndexBuilder<'s, T> {}

/// Documents of type `T` by id in a named index, found again by their words,
/// their vectors or both.
pub struct Index<T> {
    pub(crate) search: Arc<Search>,
    pub(crate) open: Arc<OpenIndex>,
    /// How a table's index reads its hits' rows, which are their values.
    #[cfg(feature = "sql")]
    pub(crate) read_row: Option<ReadRow<T>>,
    pub(crate) _value: PhantomData<fn() -> T>,
}

impl<T> Clone for Index<T> {
    fn clone(&self) -> Self {
        Index {
            search: Arc::clone(&self.search),
            open: Arc::clone(&self.open),
            #[cfg(feature = "sql")]
            read_row: self.read_row.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> Index<T> {
    /// Whether it is a table's index, whose rows are written to the table.
    #[cfg(feature = "wire")]
    pub(crate) fn follows_table(&self) -> bool {
        #[cfg(feature = "sql")]
        return self.read_row.is_some();
        #[cfg(not(feature = "sql"))]
        false
    }

    /// An index of documents of its own, in search.db.
    fn of_documents(search: Arc<Search>, open: Arc<OpenIndex>) -> Self {
        #[cfg(feature = "sql")]
        return Index { search, open, read_row: None, _value: PhantomData };
        #[cfg(not(feature = "sql"))]
        Index { search, open, _value: PhantomData }
    }
}

impl<T> std::fmt::Debug for Index<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").field("name", &self.open.name).finish()
    }
}

impl<T: DeserializeOwned> Index<T> {
    pub fn name(&self) -> &str {
        &self.open.name
    }

    /// Where the index stands: serving the settings it was opened with, read
    /// again beside them and how far, or a rebuild that failed and why. A
    /// search never waits: it answers by the settings the index serves.
    pub fn status(&self) -> Result<Status> {
        self.search.status(&self.open).map_err(|error| error.within(format!("index {}", self.open.name)))
    }

    /// Waits until the index serves the settings it was opened with: at once
    /// when no rebuild is under way, and an error naming why when one failed.
    /// A store opened without background work runs [`maintain`](super::maintain)
    /// itself meanwhile.
    pub fn ready(&self) -> Result<()> {
        self.search.ready(&self.open).map_err(|error| error.within(format!("index {}", self.open.name)))
    }

    /// A search by `text`'s words: a document needs any of them, not all,
    /// and the best ten come first unless the search says otherwise. Of
    /// equal scores the document set first comes first, so pages of one
    /// search join without a hit twice. With a [`vector`](SearchCall::vector)
    /// it searches by both.
    pub fn search(&self, text: &str) -> SearchCall<'_, T> {
        SearchCall {
            index: self,
            #[cfg(feature = "sql")]
            tx: None,
            text: text.to_owned(),
            vector: None,
            vector_weight: None,
            rerank: false,
            filter: Vec::new(),
            offset: 0,
            limit: LIMIT,
        }
    }

    /// A search by `vector` alone, which the program's model gave the
    /// question: the exact best by their cosine with it, which is each
    /// hit's score. A document without a vector is never found by one.
    pub fn search_vector<'i>(&'i self, vector: &'i [f32]) -> SearchCall<'i, T> {
        self.search("").vector(vector)
    }

    /// The hits of `query`, each with its document as its value.
    fn found(&self, query: Query) -> Result<Found<T>> {
        let answer = self.search.search(&self.open, query)?;
        let hits = answer.hits.into_iter().map(|(id, value, score)| {
            let value = decode(&value).map_err(|error| error.within(format!("id {id:?}")))?;
            Ok(Hit { id, value, score })
        });
        Ok(Found { hits: hits.collect::<Result<_>>()?, corrected: answer.corrected })
    }
}

impl<T: Serialize + DeserializeOwned> Index<T> {
    /// Sets the document `id` to `value`, replacing it whole; the next
    /// search sees it. On an index with vectors a set gives its vector, or
    /// says it has none, through [`id`](Index::id).
    pub fn set(&self, id: impl Key, value: &T) -> Result<()> {
        self.set_given(Given { id: id.text().into_owned(), value, vector: None })
    }

    /// A set of the document `id` with its vector, run by its last step:
    /// `products.id("p-3").vector(&embedding).set(&product)`.
    pub fn id(&self, id: impl Key) -> DocumentCall<'_, T> {
        DocumentCall { index: self, id: id.text().into_owned(), vector: None }
    }

    fn set_given(&self, given: Given<'_, T>) -> Result<()> {
        let within = |error: Error| error.within(format!("index {}: id {:?}", self.open.name, given.id));
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            return self.pass_on_set(std::slice::from_ref(&given)).map_err(within);
        }
        let document = |data: &Data| Ok(vec![prepared(data.id, &given, &data.settings)?]);
        self.search.set(&self.open, document).map_err(within)
    }

    /// Sets each document to its value, all of them in one commit or none:
    /// `(id, &value)`, or on an index with vectors `(id, &value, &vector)`,
    /// and in a list where some have none `(id, &value, Some(&vector[..]))`
    /// and `(id, &value, None)`. A refused one names its place in the list
    /// and its id. A call holds 1,000 documents and 16 MiB of them as JSON
    /// at most; a larger import goes in calls of that size.
    pub fn set_many<'v>(&self, documents: impl IntoIterator<Item = impl Document<'v, T>>) -> Result<()>
    where
        T: 'v,
    {
        let documents: Vec<Given<'v, T>> = documents.into_iter().map(Given::of).collect();
        self.set_all(documents)
    }

    /// Sets each of `documents`, all in one commit or none.
    pub(super) fn set_all(&self, documents: Vec<Given<'_, T>>) -> Result<()> {
        let within = |error: Error| error.within(format!("index {}", self.open.name));
        if documents.len() > MOST_MANY {
            let said = format!("a set_many of {} documents, past {MOST_MANY}", documents.len());
            return Err(within(Error::limit(said)));
        }
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            return self.pass_on_set(&documents).map_err(within);
        }
        let all = |data: &Data| {
            let mut prepared_all = Vec::with_capacity(documents.len());
            let mut bytes = 0;
            for (at, given) in documents.iter().enumerate() {
                let place = |error: Error| error.within(format!("document {at}, id {:?}", given.id));
                let document = prepared(data.id, given, &data.settings).map_err(place)?;
                bytes += document.value.len();
                if bytes > MOST_MANY_BYTES {
                    return Err(Error::limit(format!("a set_many past {} MiB as JSON", MOST_MANY_BYTES >> 20)));
                }
                prepared_all.push(document);
            }
            Ok(prepared_all)
        };
        self.search.set(&self.open, all).map_err(within)
    }

    /// Deletes the document `id`; says whether there was one.
    pub fn delete(&self, id: impl Key) -> Result<bool> {
        let id = id.text();
        let within = |error: Error| error.within(format!("index {}: id {id:?}", self.open.name));
        let checked = checked(&id).map_err(within)?.to_owned();
        #[cfg(feature = "replication")]
        if self.search.file.is_copy() {
            let store = self.search.store().map_err(within)?;
            return super::forward::pass_on_delete(&store, &self.open.name, &checked).map_err(within);
        }
        self.search.delete(&self.open, checked).map_err(within)
    }

    /// Passes a set of documents on a copy of search's file to its home.
    #[cfg(feature = "replication")]
    fn pass_on_set(&self, documents: &[Given<'_, T>]) -> Result<()> {
        if self.search.lent_by.is_some() {
            return Err(Error::new(
                crate::ErrorKind::Unavailable,
                "a copy passes no set of an index in a database's file to its home yet",
            ));
        }
        let passed = documents.iter().map(super::forward::Passed::of).collect::<Result<Vec<_>>>()?;
        super::forward::pass_on_set(&self.search.store()?, &self.open.name, &passed)
    }

    /// The document `id`, or none.
    pub fn get(&self, id: impl Key) -> Result<Option<T>> {
        let id = id.text();
        let within = |error: Error| error.within(format!("index {}: id {id:?}", self.open.name));
        let Some(value) = self.search.get(&self.open, checked(&id).map_err(within)?).map_err(within)? else {
            return Ok(None);
        };
        decode(&value).map(Some).map_err(within)
    }
}

/// A set of one document with its vector, run by [`DocumentCall::set`].
#[must_use = "a document is set at `set`"]
pub struct DocumentCall<'a, T> {
    index: &'a Index<T>,
    id: String,
    vector: Option<Option<&'a [f32]>>,
}

impl<T> std::fmt::Debug for DocumentCall<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentCall").field("id", &self.id).finish_non_exhaustive()
    }
}

impl<'a, T: Serialize + DeserializeOwned> DocumentCall<'a, T> {
    /// The document's vector, which the program's own model gave it: of
    /// the index's dimensions, every number finite and not all zeros.
    pub fn vector(mut self, vector: &'a [f32]) -> Self {
        self.vector = Some(Some(vector));
        self
    }

    /// The document has no vector, and only its words find it.
    pub fn without_vector(mut self) -> Self {
        self.vector = Some(None);
        self
    }

    /// Sets the document to `value`, replacing it whole, its words and its
    /// vector; the next search sees it.
    pub fn set(self, value: &T) -> Result<()> {
        self.index.set_given(Given { id: self.id, value, vector: self.vector })
    }
}

/// A document as [`Index::set_many`] takes it: `(id, &value)`, or on an index
/// with vectors `(id, &value, &vector[..])` and `(id, &value, None)`.
pub trait Document<'v, T> {
    /// Its id, its value, and its vector: `None` when it says nothing of one,
    /// `Some(None)` when it has none.
    fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>);
}

impl<'v, K: Key, T> Document<'v, T> for (K, &'v T) {
    fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>) {
        (self.0.text().into_owned(), self.1, None)
    }
}

impl<'v, K: Key, T> Document<'v, T> for (K, &'v T, &'v [f32]) {
    fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>) {
        (self.0.text().into_owned(), self.1, Some(Some(self.2)))
    }
}

impl<'v, K: Key, T> Document<'v, T> for (K, &'v T, &'v Vec<f32>) {
    fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>) {
        (self.0.text().into_owned(), self.1, Some(Some(self.2)))
    }
}

impl<'v, K: Key, T> Document<'v, T> for (K, &'v T, Option<&'v [f32]>) {
    fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>) {
        (self.0.text().into_owned(), self.1, Some(self.2))
    }
}

/// A document as a call gave it: its id, its value, and what it said of its
/// vector.
pub(super) struct Given<'v, T> {
    pub(super) id: String,
    pub(super) value: &'v T,
    pub(super) vector: Option<Option<&'v [f32]>>,
}

impl<'v, T> Given<'v, T> {
    fn of(document: impl Document<'v, T>) -> Self {
        let (id, value, vector) = document.parts();
        Given { id, value, vector }
    }
}

/// A search of an index, run by [`SearchCall::run`].
#[must_use = "a search runs at `run`"]
pub struct SearchCall<'i, T> {
    index: &'i Index<T>,
    /// The transaction a table's index searches in, `tx.with(&index)`.
    #[cfg(feature = "sql")]
    tx: Option<&'i Transaction<'i>>,
    text: String,
    vector: Option<&'i [f32]>,
    vector_weight: Option<f64>,
    rerank: bool,
    filter: Vec<Condition>,
    offset: usize,
    limit: usize,
}

impl<T> std::fmt::Debug for SearchCall<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchCall").field("text", &self.text).field("offset", &self.offset).finish()
    }
}

/// What a search found, the best first, and the text it read in fact when
/// it read a misspelt word as the index's nearest.
#[derive(Clone, Debug, PartialEq)]
pub struct Found<T> {
    pub hits: Vec<Hit<T>>,
    pub corrected: Option<String>,
}

/// A document a search found, and the score that ordered it: a score orders
/// one search's hits and means nothing beside another's.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit<T> {
    pub id: String,
    pub value: T,
    pub score: f64,
}

impl<'i, T: DeserializeOwned> SearchCall<'i, T> {
    /// Searches by `vector` too, which the program's model gave the question:
    /// each half's best hundred, or `offset + limit` if more, their scores
    /// each scaled to 0–1 by its own lowest and highest, 0 where the other
    /// half did not find a document, and summed at the index's vector
    /// weight; a hit's score is that sum. Pages join within the first
    /// hundred, where both halves stay the same; past it each page's halves
    /// are longer, and their sums may order it otherwise.
    pub fn vector(mut self, vector: &'i [f32]) -> Self {
        self.vector = Some(vector);
        self
    }

    /// What the vector weighs in this search, in place of the index's: from
    /// 0, the search by its words alone, to 1, by its vector alone, each
    /// with its own scores.
    pub fn vector_weight(mut self, weight: f64) -> Self {
        self.vector_weight = Some(weight);
        self
    }

    /// Orders the hundred best of the search's words by their vectors'
    /// cosines with its vector, finding nothing the words do not; a hit
    /// without a vector comes after those with one, scored −1.
    pub fn rerank(mut self) -> Self {
        self.rerank = true;
        self
    }

    /// Keeps its hits to the documents `condition` passes, and to those
    /// every earlier one passes: `.filter(search::eq("owner_id", &42))`,
    /// `.filter(search::is_in("category", &["shoes", "boots"]))`. It answers
    /// the best of those, scored by the words of the whole index, never the
    /// best of all with the others taken out after.
    pub fn filter(mut self, condition: Condition) -> Self {
        self.filter.push(condition);
        self
    }

    /// The search run inside `tx`, a transaction of a table's database.
    #[cfg(feature = "sql")]
    pub(crate) fn in_tx(mut self, tx: &'i Transaction<'i>) -> Self {
        self.tx = Some(tx);
        self
    }

    /// The hits it answers, 10 unless it says.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// The hits it passes before the first it answers.
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn run(self) -> Result<Found<T>> {
        let name = &self.index.open.name;
        let within = |error: Error| error.within(format!("index {name}"));
        let query = self.query().map_err(within)?;
        #[cfg(feature = "sql")]
        if let Some(read_row) = &self.index.read_row {
            return super::table_index::found(self.index, &**read_row, query, self.tx).map_err(within);
        }
        self.index.found(query).map_err(within)
    }

    /// The search's index and query, for a caller that reads its hits
    /// itself: the wire reads a table's rows by their cells.
    #[cfg(all(feature = "wire", feature = "sql"))]
    pub(crate) fn asked(self) -> Result<(&'i Index<T>, Query)> {
        let name = &self.index.open.name;
        let query = self.query().map_err(|error| error.within(format!("index {name}")))?;
        Ok((self.index, query))
    }

    fn query(&self) -> Result<Query> {
        if self.text.len() > MOST_TEXT {
            return Err(Error::limit(format!("a search's text of {} bytes, past {MOST_TEXT}", self.text.len())));
        }
        if self.limit == 0 {
            return Err(Error::invalid("a search asks for at least one hit"));
        }
        if self.offset.saturating_add(self.limit) > MOST_HITS {
            return Err(Error::limit(format!(
                "a search reaches its {MOST_HITS}th hit at most, offset and limit together"
            )));
        }
        let normal = analyzer::normalize(&self.text);
        let mut words = analyzer::words_of(&normal);
        words.truncate(MOST_WORDS);
        Ok(Query {
            normal,
            words,
            vector: self.question()?,
            vector_weight: self.vector_weight,
            rerank: self.rerank,
            filter: if self.filter.is_empty() { None } else { Some(filter::asked(&self.filter)?) },
            offset: self.offset,
            limit: self.limit,
        })
    }

    /// The search's vector, refusing a weight or a rerank without one and a
    /// rerank without words to order.
    fn question(&self) -> Result<Option<Question>> {
        let Some(vector) = self.vector else {
            if self.vector_weight.is_some() || self.rerank {
                return Err(Error::invalid("a vector weight or a rerank for a search without a vector"));
            }
            return Ok(None);
        };
        check_hybrid(self.vector_weight, self.rerank)?;
        if self.rerank && self.text.trim().is_empty() {
            return Err(Error::invalid("a rerank orders a search's words' hits, and a search without words has none"));
        }
        Question::new(vector).map(Some)
    }
}

/// A document ready to set: its id checked, its value as JSON, its words as
/// the index's fields read them and its vector.
fn prepared<T: Serialize>(index: i64, given: &Given<'_, T>, settings: &Settings) -> Result<Prepared> {
    let (tree, bytes) = json(given.value)?;
    let incoming = Incoming { id: checked(&given.id)?.to_owned(), tree, bytes, vector: given.vector };
    Prepared::new(index, incoming, settings)
}

/// A document's value as JSON, and its bytes, within the bound.
fn json<T: Serialize>(value: &T) -> Result<(Value, Vec<u8>)> {
    let tree = serde_json::to_value(value).map_err(|error| Error::invalid(format!("a document: {error}")))?;
    let bytes = serde_json::to_vec(&tree).map_err(|error| Error::invalid(format!("a document: {error}")))?;
    if bytes.len() > MOST_DOCUMENT {
        return Err(Error::limit(format!("a document of {} bytes as JSON, past 1 MiB", bytes.len())));
    }
    Ok((tree, bytes))
}

fn decode<T: DeserializeOwned>(value: &[u8]) -> Result<T> {
    serde_json::from_slice(value).map_err(|error| Error::invalid(format!("a document of another type: {error}")))
}

fn checked(id: &str) -> Result<&str> {
    if id.is_empty() {
        return Err(Error::invalid("an empty id"));
    }
    if id.len() > MOST_ID {
        return Err(Error::invalid(format!("an id of {} bytes, past {MOST_ID}", id.len())));
    }
    Ok(id)
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
