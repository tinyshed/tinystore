//! An index's handle: documents set by id, found again by their words.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::analyzer;
use super::document::analyze;
use super::engine::{OpenIndex, Search};
use super::read::Query;
use super::score::score;
use super::settings::Wanted;
use super::write::Prepared;
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
        let wanted = Wanted { fields: Vec::new(), typo_tolerance: true };
        IndexBuilder { store, name: name.to_owned(), wanted, _value: PhantomData }
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

    /// Whether a misspelt word is read as the index's nearest: on unless
    /// turned off.
    pub fn typo_tolerance(mut self, on: bool) -> Self {
        self.wanted.typo_tolerance = on;
        self
    }

    /// Opens the index, made the first time. An index that holds other
    /// settings is changed to these: a weight, a field removed and typo
    /// tolerance at once, and each change said in the log.
    pub fn open(self) -> Result<Index<T>> {
        let within = |error: Error| error.within(format!("index {}", self.name));
        let search = Search::of(self.store).map_err(within)?;
        let open = search.open_index(&self.name, &self.wanted).map_err(within)?;
        Ok(Index { search, open, _value: PhantomData })
    }
}

/// Documents of type `T` by id in a named index, found again by their words.
pub struct Index<T> {
    search: Arc<Search>,
    open: Arc<OpenIndex>,
    _value: PhantomData<fn() -> T>,
}

impl<T> Clone for Index<T> {
    fn clone(&self) -> Self {
        Index { search: Arc::clone(&self.search), open: Arc::clone(&self.open), _value: PhantomData }
    }
}

impl<T> std::fmt::Debug for Index<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").field("name", &self.open.name).finish()
    }
}

impl<T: Serialize + DeserializeOwned> Index<T> {
    pub fn name(&self) -> &str {
        &self.open.name
    }

    /// Sets the document `id` to `value`, replacing it whole; the next
    /// search sees it.
    pub fn set(&self, id: impl Key, value: &T) -> Result<()> {
        let id = id.text();
        let within = |error: Error| error.within(format!("index {}: id {id:?}", self.open.name));
        let index = self.open.id;
        let document = |settings: &super::settings::Settings| {
            let value = json(value)?;
            let analyzed = analyze(&value.0, &settings.fields)?;
            Ok(Prepared { index, id: checked(&id)?.to_owned(), value: value.1, analyzed })
        };
        self.search.set(&self.open, document).map_err(within)
    }

    /// Deletes the document `id`; says whether there was one.
    pub fn delete(&self, id: impl Key) -> Result<bool> {
        let id = id.text();
        let within = |error: Error| error.within(format!("index {}: id {id:?}", self.open.name));
        let checked = checked(&id).map_err(within)?.to_owned();
        self.search.delete(&self.open, checked).map_err(within)
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

    /// A search by `text`'s words: a document needs any of them, not all,
    /// and the best ten come first unless the search says otherwise. Of
    /// equal scores the document set first comes first, so pages of one
    /// search join without a hit twice.
    pub fn search(&self, text: &str) -> SearchCall<'_, T> {
        SearchCall { index: self, text: text.to_owned(), offset: 0, limit: LIMIT }
    }
}

/// A search of an index, run by [`SearchCall::run`].
#[must_use = "a search runs at `run`"]
pub struct SearchCall<'i, T> {
    index: &'i Index<T>,
    text: String,
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

impl<T: Serialize + DeserializeOwned> SearchCall<'_, T> {
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
        let answer = self.index.search.search(&self.index.open, query).map_err(within)?;
        let mut hits = Vec::with_capacity(answer.hits.len());
        for (id, value, points) in answer.hits {
            let value = decode(&value).map_err(|error| within(error.within(format!("id {id:?}"))))?;
            hits.push(Hit { id, value, score: score(points) });
        }
        Ok(Found { hits, corrected: answer.corrected })
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
        Ok(Query { normal, words, offset: self.offset, limit: self.limit })
    }
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
