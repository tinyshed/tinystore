//! search's methods of protocol 2, protocol/search.wire: indexes a
//! connection opened by handle, documents of JSON set, read and deleted by
//! id, searches with their vector and `where`, an index's status, a drop and
//! the list. A vector travels as its floats' bytes, little-endian. A table's
//! index opens with its database's handle, and its hits carry their rows as
//! sql's rows carry them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use super::Lent;
use super::codec::Message;
use super::protocol::{
    Empty, Failure, Handle, SearchDeleted, SearchDocument, SearchDropped, SearchFound, SearchGot, SearchHit,
    SearchIndex, SearchKey, SearchListed, SearchName, SearchOpen, SearchQuery, SearchSet, SearchSetMany, SearchStatus,
    method,
};
use crate::engine::SharedFile;
use crate::search::{self, Document, Index, Rebuild, SearchCall, Status};
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

    /// An index of documents of its own, which a call sets, reads and deletes.
    fn documents(&self, handle: u64) -> Result<Index<Value>, Failure> {
        let index = self.index(handle)?;
        if index.follows_table() {
            return Err(Failure::invalid(format!(
                "index {}: a table's index follows its table's rows, which are written to the table",
                index.name()
            )));
        }
        Ok(index)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Index<Value>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) fn call(
    store: &Store,
    handles: &Handles,
    lent: Lent<'_>,
    called: u16,
    body: &[u8],
    read_only: bool,
) -> Answered {
    match called {
        method::SEARCH_OPEN => open((store, lent), handles, SearchOpen::decode(body)?, read_only),
        method::SEARCH_SET => set(handles, SearchSet::decode(body)?),
        method::SEARCH_SET_MANY => set_many(handles, SearchSetMany::decode(body)?),
        method::SEARCH_DELETE => {
            let key = SearchKey::decode(body)?;
            Ok(SearchDeleted { deleted: handles.documents(key.handle)?.delete(&key.id)? }.encode())
        }
        method::SEARCH_GET => {
            let key = SearchKey::decode(body)?;
            let value = handles.documents(key.handle)?.get(&key.id)?;
            Ok(SearchGot { found: value.is_some(), value: value.map(|value| value.to_string()) }.encode())
        }
        method::SEARCH_SEARCH => searched(handles, SearchQuery::decode(body)?),
        method::SEARCH_STATUS => Ok(status(&handles.index(Handle::decode(body)?.handle)?.status()?).encode()),
        method::SEARCH_DROP_INDEX => {
            Ok(SearchDropped { dropped: dropped(store, lent, SearchName::decode(body)?)? }.encode())
        }
        method::SEARCH_LIST => {
            Empty::decode(body)?;
            Ok(SearchListed { indexes: search::list(store)?.iter().map(listed).collect() }.encode())
        }
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

/// Opens an index by the settings asked, or a table's in the database the
/// open names; a read connection's only as it is.
fn open((store, lent): (&Store, Lent<'_>), handles: &Handles, asked: SearchOpen, read_only: bool) -> Answered {
    let index = match asked.database {
        Some(database) => table_index(lent(database)?, &asked, read_only)?,
        None if read_only => search::open_as_it_is(store, &asked.name)?,
        None => documents_index(store, &asked)?,
    };
    let handle = handles.last.fetch_add(1, Ordering::Relaxed) + 1;
    handles.lock().insert(handle, index);
    Ok(Handle { handle }.encode())
}

fn documents_index(store: &Store, asked: &SearchOpen) -> Result<Index<Value>, Failure> {
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
    Ok(if asked.reindex { builder.reindex()? } else { builder.open()? })
}

/// Opens a table's index in a database's file, whose rows the wire reads by
/// their cells.
#[cfg(feature = "sql")]
fn table_index(shared: Arc<SharedFile>, asked: &SearchOpen, read_only: bool) -> Result<Index<Value>, Failure> {
    if read_only {
        return Ok(search::table_as_it_is(&shared, &asked.name)?);
    }
    let table = asked.table.as_deref().unwrap_or_default();
    let mut builder = search::TableIndexBuilder::<Value>::in_file(shared, &asked.name, search::by_cells()).table(table);
    builder = builder.typo_tolerance(asked.typo_tolerance.unwrap_or(true)).filters(&asked.filters);
    if let Some(key) = &asked.key {
        builder = builder.key(key);
    }
    for field in &asked.fields {
        builder = builder.field(&field.path, field.weight);
    }
    match (&asked.vector_column, asked.dimensions) {
        (Some(column), Some(dimensions)) => {
            builder = builder.vectors(column, usize::try_from(dimensions).unwrap_or(usize::MAX));
        }
        (None, None) => {}
        _ => return Err(Failure::invalid("a table's index reads its vectors from a column of its dimensions")),
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
    Ok(if asked.reindex { builder.reindex()? } else { builder.open()? }.into_index())
}

#[cfg(not(feature = "sql"))]
fn table_index(_: Arc<SharedFile>, _: &SearchOpen, _: bool) -> Result<Index<Value>, Failure> {
    Err(Failure::invalid("this server keeps no databases"))
}

/// Drops an index of search.db, or a table's in the database the call names.
fn dropped(store: &Store, lent: Lent<'_>, asked: SearchName) -> Result<bool, Failure> {
    match asked.database {
        #[cfg(feature = "sql")]
        Some(database) => Ok(search::drop_in_file(&*lent(database)?, &asked.name)?),
        #[cfg(not(feature = "sql"))]
        Some(_) => Err(Failure::invalid("this server keeps no databases")),
        None => Ok(store.drop_index(&asked.name)?),
    }
}

fn set(handles: &Handles, asked: SearchSet) -> Answered {
    let index = handles.documents(asked.handle)?;
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
    let index = handles.documents(asked.handle)?;
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
    let call = query(&index, &asked, vector.as_deref())?;
    if index.follows_table() {
        return rows_found(call);
    }
    let found = call.run()?;
    let hits = found.hits.into_iter().map(|hit| SearchHit {
        id: hit.id,
        value: hit.value.to_string(),
        score: hit.score,
        row: None,
    });
    Ok(SearchFound { hits: hits.collect(), corrected: found.corrected, columns: None }.encode())
}

/// The search a query asks of `index`.
fn query<'i>(
    index: &'i Index<Value>,
    asked: &SearchQuery,
    vector: Option<&'i [f32]>,
) -> Result<SearchCall<'i, Value>, Failure> {
    let mut call = index.search(&asked.text).offset(usize::try_from(asked.offset).unwrap_or(usize::MAX));
    if let Some(limit) = asked.limit {
        call = call.limit(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if let Some(vector) = vector {
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
    Ok(call)
}

/// A table's index's hits, each with its row's cells, and the rows' columns
/// once: every row is read whole, so all have the same.
#[cfg(feature = "sql")]
fn rows_found(call: SearchCall<'_, Value>) -> Answered {
    let read_row = |connection: &rusqlite::Connection, select: &str, key: &rusqlite::types::Value| {
        let statement = crate::sql::Sql::new(select);
        let statement = match key {
            rusqlite::types::Value::Integer(number) => statement.bind(number),
            rusqlite::types::Value::Text(text) => statement.bind(text),
            _ => return Err(crate::Error::internal("a row's key of neither an integer nor text")),
        };
        crate::sql::cells_on(connection, &statement)
    };
    let found = search::found_by(call, &read_row)?;
    let columns = found.hits.first().map(|hit| hit.value.0.clone()).unwrap_or_default();
    let hits = found.hits.into_iter().map(|hit| SearchHit {
        id: hit.id,
        value: "null".to_owned(),
        score: hit.score,
        row: Some(hit.value.1.into_iter().map(super::sql::cell_of).collect()),
    });
    Ok(SearchFound { hits: hits.collect(), corrected: found.corrected, columns: Some(columns) }.encode())
}

#[cfg(not(feature = "sql"))]
fn rows_found(_: SearchCall<'_, Value>) -> Answered {
    Err(Failure::internal("a table's index on a server that keeps no databases"))
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
