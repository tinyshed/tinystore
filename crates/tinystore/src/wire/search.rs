//! search's methods of protocol 2, protocol/search.wire: indexes a
//! connection opened by handle, documents of JSON set, read and deleted by
//! id, searches with their vector and `where`, an index's status, a drop and
//! the list. A vector travels as its floats' bytes, little-endian.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use super::codec::Message;
use super::protocol::{
    Empty, Failure, Handle, SearchDeleted, SearchDocument, SearchDropped, SearchFound, SearchGot, SearchHit,
    SearchIndex, SearchKey, SearchListed, SearchName, SearchOpen, SearchQuery, SearchSet, SearchSetMany, SearchStatus,
    method,
};
use crate::search::{self, Document, Index, Rebuild, Status};
use crate::{Store, unix_millis};

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// The indexes a connection opened, by handle.
#[derive(Default)]
pub(crate) struct Handles {
    last: AtomicU64,
    open: Mutex<HashMap<u64, Index<Value>>>,
}

impl Handles {
    fn index(&self, handle: u64) -> Result<Index<Value>, Failure> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.get(&handle).cloned().ok_or_else(|| Failure::invalid(format!("index {handle}: no such handle")))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Index<Value>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], read_only: bool) -> Answered {
    match called {
        method::SEARCH_OPEN => open(store, handles, SearchOpen::decode(body)?, read_only),
        method::SEARCH_SET => set(handles, SearchSet::decode(body)?),
        method::SEARCH_SET_MANY => set_many(handles, SearchSetMany::decode(body)?),
        method::SEARCH_DELETE => {
            let key = SearchKey::decode(body)?;
            Ok(SearchDeleted { deleted: handles.index(key.handle)?.delete(&key.id)? }.encode())
        }
        method::SEARCH_GET => {
            let key = SearchKey::decode(body)?;
            let value = handles.index(key.handle)?.get(&key.id)?;
            Ok(SearchGot { found: value.is_some(), value: value.map(|value| value.to_string()) }.encode())
        }
        method::SEARCH_SEARCH => searched(handles, SearchQuery::decode(body)?),
        method::SEARCH_STATUS => Ok(status(&handles.index(Handle::decode(body)?.handle)?.status()?).encode()),
        method::SEARCH_DROP_INDEX => {
            Ok(SearchDropped { dropped: store.drop_index(&SearchName::decode(body)?.name)? }.encode())
        }
        method::SEARCH_LIST => {
            Empty::decode(body)?;
            Ok(SearchListed { indexes: search::list(store)?.iter().map(listed).collect() }.encode())
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Opens an index by the settings asked; a read connection's only as it is.
fn open(store: &Store, handles: &Handles, asked: SearchOpen, read_only: bool) -> Answered {
    let index = if read_only {
        search::open_as_it_is(store, &asked.name)?
    } else {
        let mut builder = store.index::<Value>(&asked.name).typo_tolerance(asked.typo_tolerance.unwrap_or(true));
        for field in &asked.fields {
            builder = builder.field(&field.path, field.weight);
        }
        builder = builder.filters(&asked.filters);
        if let Some(dimensions) = asked.dimensions {
            builder = builder.vectors(usize::try_from(dimensions).unwrap_or(usize::MAX));
        }
        if let Some(recall) = asked.recall {
            builder = builder.recall(recall);
        }
        if let Some(weight) = asked.vector_weight {
            builder = builder.vector_weight(weight);
        }
        if asked.rerank {
            builder = builder.rerank();
        }
        let builder = builder.rebuild(if asked.manual { Rebuild::Manual } else { Rebuild::Automatic });
        if asked.reindex { builder.reindex()? } else { builder.open()? }
    };
    let handle = handles.last.fetch_add(1, Ordering::Relaxed) + 1;
    handles.lock().insert(handle, index);
    Ok(Handle { handle }.encode())
}

fn set(handles: &Handles, asked: SearchSet) -> Answered {
    let index = handles.index(asked.handle)?;
    let value = document(&asked.value)?;
    let vector = floats(asked.vector.as_deref())?;
    let call = index.id(&asked.id);
    let call = match (&vector, asked.no_vector) {
        (Some(vector), _) => call.vector(vector),
        (None, true) => call.without_vector(),
        (None, false) => {
            index.set(&asked.id, &value)?;
            return Ok(Empty {}.encode());
        }
    };
    call.set(&value)?;
    Ok(Empty {}.encode())
}

/// A document of a `setMany`, read from the wire.
struct Read {
    id: String,
    value: Value,
    vector: Option<Option<Vec<f32>>>,
}

impl<'v> Document<'v, Value> for &'v Read {
    fn parts(self) -> (String, &'v Value, Option<Option<&'v [f32]>>) {
        (self.id.clone(), &self.value, self.vector.as_ref().map(Option::as_deref))
    }
}

fn set_many(handles: &Handles, asked: SearchSetMany) -> Answered {
    let index = handles.index(asked.handle)?;
    let documents = asked.documents.iter().map(read).collect::<Result<Vec<Read>, Failure>>()?;
    index.set_many(&documents)?;
    Ok(Empty {}.encode())
}

fn read(given: &SearchDocument) -> Result<Read, Failure> {
    let vector = match (floats(given.vector.as_deref())?, given.no_vector) {
        (Some(vector), _) => Some(Some(vector)),
        (None, true) => Some(None),
        (None, false) => None,
    };
    Ok(Read { id: given.id.clone(), value: document(&given.value)?, vector })
}

fn searched(handles: &Handles, asked: SearchQuery) -> Answered {
    let index = handles.index(asked.handle)?;
    let vector = floats(asked.vector.as_deref())?;
    let mut call = index.search(&asked.text).offset(usize::try_from(asked.offset).unwrap_or(usize::MAX));
    if let Some(limit) = asked.limit {
        call = call.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if let Some(vector) = &vector {
        call = call.vector(vector);
    }
    if let Some(weight) = asked.vector_weight {
        call = call.vector_weight(weight);
    }
    if asked.rerank {
        call = call.rerank();
    }
    if let Some(conditions) = &asked.r#where {
        for condition in search::conditions(conditions)? {
            call = call.filter(condition);
        }
    }
    let found = call.run()?;
    let hits =
        found.hits.into_iter().map(|hit| SearchHit { id: hit.id, value: hit.value.to_string(), score: hit.score });
    Ok(SearchFound { hits: hits.collect(), corrected: found.corrected }.encode())
}

fn document(text: &str) -> Result<Value, Failure> {
    serde_json::from_str(text).map_err(|error| Failure::invalid(format!("a document that is not JSON: {error}")))
}

/// A vector's floats from its bytes, four a dimension, little-endian.
fn floats(bytes: Option<&[u8]>) -> Result<Option<Vec<f32>>, Failure> {
    let Some(bytes) = bytes else { return Ok(None) };
    let (floats, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(Failure::invalid(format!("a vector of {} bytes, not four a dimension", bytes.len())));
    }
    Ok(Some(floats.iter().map(|float| f32::from_le_bytes(*float)).collect()))
}

fn status(status: &Status) -> SearchStatus {
    match status {
        Status::Ready => SearchStatus { state: "ready".to_owned(), ..SearchStatus::default() },
        Status::Rebuilding { done } => {
            SearchStatus { state: "rebuilding".to_owned(), done: *done, error: String::new() }
        }
        Status::Failed { error } => SearchStatus { state: "failed".to_owned(), done: 0.0, error: error.clone() },
    }
}

fn listed(index: &search::Listed) -> SearchIndex {
    let status = status(&index.status);
    SearchIndex {
        name: index.name.clone(),
        documents: index.documents,
        vectors: index.vectors,
        opened: index.opened.map(unix_millis),
        state: status.state,
        done: status.done,
        error: status.error,
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
