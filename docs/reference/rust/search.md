# Search API for Rust

Every public type, trait and function of the Search engine in `tinystore::search` and `tinystore::aio::search`, generated from its source. The [Search guide](../../guides/search/README.md) explains how to use them, and the [Bun and Node](../bun/search.md), [Python](../python/search.md) and [Go](../go/search.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.index

```rust
pub fn index<T: Serialize + DeserializeOwned>(&self, name: &str) -> IndexBuilder<'_, T>
```

The index `name`, made the first time it opens: documents of type
`T` set by id and found again by their words, their vectors or both.

### Store.drop_index

```rust
pub fn drop_index(&self, name: &str) -> Result<bool>
```

Drops the index `name` and every document it holds, in the background,
and says whether there was one; a handle of it is refused from then
on, and opening the name again makes a new index. An index no program
opens any more is kept until it is dropped, since another version of
the program may open it again.

## aio::Store

```rust
impl aio::Store
```

### aio::Store.index

```rust
pub fn index<T: Serialize + DeserializeOwned>(&self, name: &str) -> IndexBuilder<'_, T>
```

The index `name`, made the first time it opens, as
`crate::Store::index` opens one: its builder and its `open` are plain
calls, and the index's calls are awaited.

### aio::Store.drop_index

```rust
pub fn drop_index(&self, name: &str) -> impl Future<Output = Result<bool>> + Send + 'static
```

Drops the index `name` and every document it holds, as
`crate::Store::drop_index` does, on the store's pool: the drop
waits for the sets in flight, and once polled it runs to its end.

## Maintenance

```rust
pub struct Maintenance {
    pub packed: u64,
    pub purged: u64,
}
```

What one pass of maintenance did.

## Condition

```rust
pub struct Condition(_)
```

A condition of a search, given to [`SearchCall::filter`](super::SearchCall::filter).

## and

```rust
pub fn and(conditions: impl IntoIterator<Item = Condition>) -> Condition
```

Every one of `conditions`, for a group inside an `or`; an empty `and`
passes everything.

## eq

```rust
pub fn eq<T: Serialize + ?Sized>(path: &str, value: &T) -> Condition
```

A filter equal to `value`: a document passes when it holds it, or one of
its list is it. Values match exactly, case and type: the text "50" is not
the number 50, and 42.0 is 42. `None` is `invalid`, so that an absent id
never asks for documents without one; `missing` asks for those.

## is_in

```rust
pub fn is_in<T: Serialize>(path: &str, values: &[T]) -> Condition
```

A filter holding one of `values`, as SQL's `in (…)`; an empty list passes
nothing.

## missing

```rust
pub fn missing(path: &str) -> Condition
```

A filter with no value: no such field, null or an empty list.

## or

```rust
pub fn or(conditions: impl IntoIterator<Item = Condition>) -> Condition
```

Any of `conditions`, in parentheses; an empty `or` passes nothing.

## range

```rust
pub fn range<T: Serialize>(path: &str, bounds: impl RangeBounds<T>) -> Condition
```

A filter holding a number within `bounds`, any of Rust's ranges:
`range("price", 10..100)` holds 10 and not 100, `range("at", since..)`.
A document with no number there never passes. Numbers compare as f64,
so whole numbers past 2⁵³ compare by the nearest f64, where `eq` is
exact.

## Document

```rust
pub trait Document<'v, T>
```

A document as `Index::set_many` takes it: `(id, &value)`, or on an index
with vectors `(id, &value, &vector[..])` and `(id, &value, None)`.

### Document.parts

```rust
fn parts(self) -> (String, &'v T, Option<Option<&'v [f32]>>)
```

Its id, its value, and its vector: `None` when it says nothing of one,
`Some(None)` when it has none.

## DocumentCall

```rust
pub struct DocumentCall<'a, T> { /* private fields */ }
```

A set of one document with its vector, run by `DocumentCall::set`.

### DocumentCall.vector

```rust
pub fn vector(self, vector: &'a [f32]) -> Self
```

The document's vector, which the program's own model gave it: of
the index's dimensions, every number finite and not all zeros.

### DocumentCall.without_vector

```rust
pub fn without_vector(self) -> Self
```

The document has no vector, and only its words find it.

### DocumentCall.set

```rust
pub fn set(self, value: &T) -> Result<()>
```

Sets the document to `value`, replacing it whole, its words and its
vector; the next search sees it.

## Found

```rust
pub struct Found<T> {
    pub hits: Vec<Hit<T>>,
    pub corrected: Option<String>,
}
```

What a search found, the best first, and the text it read in fact when
it read a misspelt word as the index's nearest.

## Hit

```rust
pub struct Hit<T> {
    pub id: String,
    pub value: T,
    pub score: f64,
}
```

A document a search found, and the score that ordered it: a score orders
one search's hits and means nothing beside another's.

## Index

```rust
pub struct Index<T> { /* private fields */ }
```

Documents of type `T` by id in a named index, found again by their words,
their vectors or both.

### Index.name

```rust
pub fn name(&self) -> &str
```

### Index.status

```rust
pub fn status(&self) -> Result<Status>
```

Where the index stands: serving the settings it was opened with, read
again beside them and how far, or a rebuild that failed and why. A
search never waits: it answers by the settings the index serves.

### Index.ready

```rust
pub fn ready(&self) -> Result<()>
```

Waits until the index serves the settings it was opened with: at once
when no rebuild is under way, and an error naming why when one failed.
A store opened without background work runs [`maintain`](super::maintain)
itself meanwhile.

### Index.search

```rust
pub fn search(&self, text: &str) -> SearchCall<'_, T>
```

A search by `text`'s words: a document needs any of them, not all,
and the best ten come first unless the search says otherwise. Of
equal scores the document set first comes first, so pages of one
search join without a hit twice. With a [`vector`](SearchCall::vector)
it searches by both.

### Index.search_vector

```rust
pub fn search_vector<'i>(&self, vector: &'i [f32]) -> SearchCall<'i, T>
```

A search by `vector` alone, which the program's model gave the
question: the exact best by their cosine with it, which is each
hit's score. A document without a vector is never found by one.

### Index.set

```rust
pub fn set(&self, id: impl Key, value: &T) -> Result<()>
```

Sets the document `id` to `value`, replacing it whole; the next
search sees it. On an index with vectors a set gives its vector, or
says it has none, through [`id`](Index::id).

### Index.id

```rust
pub fn id(&self, id: impl Key) -> DocumentCall<'_, T>
```

A set of the document `id` with its vector, run by its last step:
`products.id("p-3").vector(&embedding).set(&product)`.

### Index.set_many

```rust
pub fn set_many<'v>(&self, documents: impl IntoIterator<Item = impl Document<'v, T>>) -> Result<()>
where
    T: 'v,
```

Sets each document to its value, all of them in one commit or none:
`(id, &value)`, or on an index with vectors `(id, &value, &vector)`,
and in a list where some have none `(id, &value, Some(&vector[..]))`
and `(id, &value, None)`. A refused one names its place in the list
and its id. A call holds 1,000 documents and 16 MiB of them as JSON
at most; a larger import goes in calls of that size.

### Index.delete

```rust
pub fn delete(&self, id: impl Key) -> Result<bool>
```

Deletes the document `id`; says whether there was one.

### Index.get

```rust
pub fn get(&self, id: impl Key) -> Result<Option<T>>
```

The document `id`, or none.

## IndexBuilder

```rust
pub struct IndexBuilder<'s, T> { /* private fields */ }
```

What `Store::index` opens: an index's settings, given where it opens.

### IndexBuilder.field

```rust
pub fn field(self, path: &str, weight: f64) -> Self
```

A field whose text is read, by its path into the document, `author.name`
for a field of a field, and what a word found there weighs: a word in
a field of 2 scores twice what it would in a field of 1. An index of
no field reads a document that is a string as its text.

### IndexBuilder.fields

```rust
pub fn fields<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self
```

Fields that each weigh 1.

### IndexBuilder.filters

```rust
pub fn filters<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self
```

The fields a search may keep its hits to, each a path into the
document whose value is text, a number, a boolean or a list of them;
the index keeps their values and no other field's.

### IndexBuilder.typo_tolerance

```rust
pub fn typo_tolerance(self, on: bool) -> Self
```

Whether a misspelt word is read as the index's nearest: on unless
turned off.

### IndexBuilder.vectors

```rust
pub fn vectors(self, dimensions: usize) -> Self
```

Keeps a vector of `dimensions` numbers with each document, which the
program's own model gives it, and finds documents by their cosine
with a search's.

### IndexBuilder.recall

```rust
pub fn recall(self, recall: f64) -> Self
```

Searches by a vector read the clusters nearest their question rather
than every vector, and find `recall` of the exact ten on average,
from above 0 to below 1: the clusters are built in the background,
and searches stay exact until then. How many clusters a search reads
is the index's to choose, from questions drawn from its own vectors.

### IndexBuilder.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

What a vector weighs against the words in a search by both, from 0,
the words alone, to 1, the vector alone; 0.5 unless set, and a
search may give its own.

### IndexBuilder.rerank

```rust
pub fn rerank(self) -> Self
```

Makes a search by both order the hits of its words by their vectors,
finding nothing its words do not, unless it gives a weight.

### IndexBuilder.rebuild

```rust
pub fn rebuild(self, rebuild: Rebuild) -> Self
```

Whether a change of settings that needs the index read again begins
by itself, as it does unless said, or waits for [`reindex`](Self::reindex),
for a program that schedules its own load.

### IndexBuilder.open

```rust
pub fn open(self) -> Result<Index<T>>
```

Opens the index, made the first time. An index that holds other
settings is changed to these, each change said in the log: a weight,
a field or a filter removed, typo tolerance, vectors added, recall
and how a search by both combines them at once; a field or a filter
added to an index that holds documents by reading them again beside
it, in the background, the index answering as before until then.
Vectors of other dimensions, or none where it keeps some, are
refused. Opening never waits for the work a change needs.

### IndexBuilder.reindex

```rust
pub fn reindex(self) -> Result<Index<T>>
```

Opens the index as [`open`](Self::open) does, and reads it again
beside itself in the background: a change `Rebuild::Manual` left
waiting, or the documents numbered again.

## Rebuild

```rust
pub enum Rebuild {
    Automatic,
    Manual,
}
```

Whether a change of an index's settings that needs it read again begins
by itself or waits for `IndexBuilder::reindex`.

## SearchCall

```rust
pub struct SearchCall<'i, T> { /* private fields */ }
```

A search of an index, run by `SearchCall::run`.

### SearchCall.vector

```rust
pub fn vector(self, vector: &'i [f32]) -> Self
```

Searches by `vector` too, which the program's model gave the question:
each half's best hundred, or `offset + limit` if more, their scores
each scaled to 0–1 by its own lowest and highest, 0 where the other
half did not find a document, and summed at the index's vector
weight; a hit's score is that sum. Pages join within the first
hundred, where both halves stay the same; past it each page's halves
are longer, and their sums may order it otherwise.

### SearchCall.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

What the vector weighs in this search, in place of the index's: from
0, the search by its words alone, to 1, by its vector alone, each
with its own scores.

### SearchCall.rerank

```rust
pub fn rerank(self) -> Self
```

Orders the hundred best of the search's words by their vectors'
cosines with its vector, finding nothing the words do not; a hit
without a vector comes after those with one, scored −1.

### SearchCall.filter

```rust
pub fn filter(self, condition: Condition) -> Self
```

Keeps its hits to the documents `condition` passes, and to those
every earlier one passes: `.filter(search::eq("owner_id", &42))`,
`.filter(search::is_in("category", &["shoes", "boots"]))`. It answers
the best of those, scored by the words of the whole index, never the
best of all with the others taken out after.

### SearchCall.limit

```rust
pub fn limit(self, limit: usize) -> Self
```

The hits it answers, 10 unless it says.

### SearchCall.offset

```rust
pub fn offset(self, offset: usize) -> Self
```

The hits it passes before the first it answers.

### SearchCall.run

```rust
pub fn run(self) -> Result<Found<T>>
```

## Status

```rust
pub enum Status {
    Ready,
    Rebuilding { done: f64 },
    Failed { error: String },
}
```

Where an index stands: serving the settings it was opened with, read
again beside them, or a rebuild that stopped.

## Listed

```rust
pub struct Listed {
    pub name: String,
    pub documents: u64,
    pub vectors: u64,
    pub opened: Option<SystemTime>,
    pub status: Status,
}
```

An index as `search::list` says it.

## TableIndex

```rust
pub struct TableIndex<T> { /* private fields */ }
```

The rows of a table found again by their words, their vectors or both;
a hit's value is its row, read in the snapshot its search read.

### TableIndex.name

```rust
pub fn name(&self) -> &str
```

### TableIndex.search

```rust
pub fn search(&self, text: &str) -> SearchCall<'_, T>
```

A search by `text`'s words, as `Index::search` makes one; each
hit's id is its row's key and its value the row.

### TableIndex.search_vector

```rust
pub fn search_vector<'i>(&self, vector: &'i [f32]) -> SearchCall<'i, T>
```

A search by `vector` alone, as `Index::search_vector` makes one.

### TableIndex.status

```rust
pub fn status(&self) -> Result<Status>
```

Where the index stands: ready, reading the table's rows the first
time or again beside itself and how far, or a rebuild that failed.

### TableIndex.ready

```rust
pub fn ready(&self) -> Result<()>
```

Waits until the index has read its table's rows and serves the
settings it was opened with.

## TableIndexBuilder

```rust
pub struct TableIndexBuilder<T> { /* private fields */ }
```

What `db.search_index` opens: an index's table and its settings.

### TableIndexBuilder.table

```rust
pub fn table(self, table: &str) -> Self
```

The table whose rows the index holds and follows from then on.

### TableIndexBuilder.key

```rust
pub fn key(self, column: &str) -> Self
```

The column of unique values a row is found by, the table's primary
key unless said; a hit's id is its value as text.

### TableIndexBuilder.field

```rust
pub fn field(self, column: &str, weight: f64) -> Self
```

A column whose text is read, and what a word found there weighs: a
word in a column of 2 scores twice what it would in a column of 1.

### TableIndexBuilder.fields

```rust
pub fn fields<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self
```

Columns that each weigh 1.

### TableIndexBuilder.filters

```rust
pub fn filters<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self
```

The columns a search may keep its hits to, each holding text, a
number, or a boolean as SQLite keeps one, 1 or 0.

### TableIndexBuilder.typo_tolerance

```rust
pub fn typo_tolerance(self, on: bool) -> Self
```

Whether a misspelt word is read as the index's nearest: on unless
turned off.

### TableIndexBuilder.vectors

```rust
pub fn vectors(self, column: &str, dimensions: usize) -> Self
```

Reads each row's vector from `column`: a blob of `dimensions`
little-endian floats that the program's own model gave it, or null
for a row without one. The store runs no model: a changed text keeps
its row's vector until the program writes another.

### TableIndexBuilder.recall

```rust
pub fn recall(self, recall: f64) -> Self
```

As [`IndexBuilder::recall`](super::IndexBuilder::recall).

### TableIndexBuilder.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

As [`IndexBuilder::vector_weight`](super::IndexBuilder::vector_weight).

### TableIndexBuilder.rerank

```rust
pub fn rerank(self) -> Self
```

As [`IndexBuilder::rerank`](super::IndexBuilder::rerank).

### TableIndexBuilder.rebuild

```rust
pub fn rebuild(self, rebuild: Rebuild) -> Self
```

As [`IndexBuilder::rebuild`](super::IndexBuilder::rebuild).

### TableIndexBuilder.open

```rust
pub fn open(self) -> Result<TableIndex<T>>
```

Opens the index, made the first time: its triggers put on the table,
and its rows read in the background, which `TableIndex::ready`
waits for. Other settings change it as [`IndexBuilder::open`](super::IndexBuilder::open)
says; another table or key is refused.

### TableIndexBuilder.reindex

```rust
pub fn reindex(self) -> Result<TableIndex<T>>
```

Opens the index as [`open`](Self::open) does, and reads it again
beside itself in the background.

## TxTableIndex

```rust
pub struct TxTableIndex<'a, T> { /* private fields */ }
```

A table's index's searches inside a transaction of its database, from
`tx.with(&index)`: they see what the transaction wrote.

### TxTableIndex.search

```rust
pub fn search(&self, text: &str) -> SearchCall<'a, T>
```

### TxTableIndex.search_vector

```rust
pub fn search_vector(&self, vector: &'a [f32]) -> SearchCall<'a, T>
```

## list

```rust
pub fn list(store: &Store) -> Result<Vec<Listed>>
```

Every index the store's `search.db` holds, by name: its documents and
vectors, when a program last opened it, and whether a rebuild is under
way.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Runs the store's search maintenance now, as a store opened without
background work has to: every head packed and the documents gone purged,
in search.db and in each database a table's index was opened in, and a
table's rows read the first time.

## aio::search::list

```rust
pub fn list(store: &Store) -> impl Future<Output = Result<Vec<Listed>>> + Send + 'static
```

Every index the store's `search.db` holds, by name, as
`crate::search::list` reads them, on the store's pool.

## aio::search::IndexBuilder

```rust
pub struct IndexBuilder<'s, T> { /* private fields */ }
```

What `Store::index` opens: an index's settings, given where it opens.

### aio::search::IndexBuilder.field

```rust
pub fn field(self, path: &str, weight: f64) -> Self
```

As `crate::search::IndexBuilder::field`.

### aio::search::IndexBuilder.fields

```rust
pub fn fields<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self
```

As `crate::search::IndexBuilder::fields`.

### aio::search::IndexBuilder.filters

```rust
pub fn filters<S: AsRef<str>>(self, paths: impl IntoIterator<Item = S>) -> Self
```

As `crate::search::IndexBuilder::filters`.

### aio::search::IndexBuilder.typo_tolerance

```rust
pub fn typo_tolerance(self, on: bool) -> Self
```

As `crate::search::IndexBuilder::typo_tolerance`.

### aio::search::IndexBuilder.vectors

```rust
pub fn vectors(self, dimensions: usize) -> Self
```

As `crate::search::IndexBuilder::vectors`.

### aio::search::IndexBuilder.recall

```rust
pub fn recall(self, recall: f64) -> Self
```

As `crate::search::IndexBuilder::recall`.

### aio::search::IndexBuilder.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

As `crate::search::IndexBuilder::vector_weight`.

### aio::search::IndexBuilder.rerank

```rust
pub fn rerank(self) -> Self
```

As `crate::search::IndexBuilder::rerank`.

### aio::search::IndexBuilder.rebuild

```rust
pub fn rebuild(self, rebuild: Rebuild) -> Self
```

As `crate::search::IndexBuilder::rebuild`.

### aio::search::IndexBuilder.open

```rust
pub fn open(self) -> Result<Index<T>>
```

Opens the index as `crate::search::IndexBuilder::open` does: a plain
call, made once at start, which never waits for the work a change of
its settings needs.

### aio::search::IndexBuilder.reindex

```rust
pub fn reindex(self) -> Result<Index<T>>
```

Opens the index as [`open`](Self::open) does, and reads it again
beside itself in the background, as
`crate::search::IndexBuilder::reindex` does.

## aio::search::Index

```rust
pub struct Index<T> { /* private fields */ }
```

Documents of type `T` by id in a named index, found again by their words,
their vectors or both, its calls awaited: the same index as
`crate::search::Index`, and the same again through `From` and
[`blocking`](Self::blocking).

### aio::search::Index.blocking

```rust
pub fn blocking(&self) -> &Index<T>
```

The same index, for its blocking calls.

### aio::search::Index.name

```rust
pub fn name(&self) -> &str
```

### aio::search::Index.status

```rust
pub fn status(&self) -> impl Future<Output = Result<Status>> + Send + 'static
```

Where the index stands, as `crate::search::Index::status` reads it,
on the store's pool.

### aio::search::Index.ready

```rust
pub fn ready(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Waits until the index serves the settings it was opened with, as
`crate::search::Index::ready` does, holding no thread: woken as its
rebuild switches in or fails, and looking at its status again on the
store's pool every tenth of a second. Dropped, it stops waiting, and
the rebuild goes on. On a store opened without background work it
runs the maintenance itself, a pass at a time on the store's pool,
and a pass it began runs to its end.

### aio::search::Index.search

```rust
pub fn search(&self, text: &str) -> SearchCall<'_, T>
```

A search by `text`'s words, as `crate::search::Index::search` makes
one, run by its awaited [`run`](SearchCall::run).

### aio::search::Index.search_vector

```rust
pub fn search_vector<'i>(&self, vector: &'i [f32]) -> SearchCall<'i, T>
```

A search by `vector` alone, as `crate::search::Index::search_vector`
makes one.

### aio::search::Index.get

```rust
pub fn get(&self, id: impl Key) -> impl Future<Output = Result<Option<T>>> + Send + 'static
```

The document `id`, or none, as `crate::search::Index::get` reads it:
a point read, tried in the future's first poll.

### aio::search::Index.set

```rust
pub fn set(&self, id: impl Key, value: &T) -> impl Future<Output = Result<()>> + Send + 'static
```

Sets the document `id` to `value`, replacing it whole, as
`crate::search::Index::set` does; the next search sees it once the
future is ready. On an index with vectors a set gives its vector, or
says it has none, through [`id`](Self::id).

### aio::search::Index.id

```rust
pub fn id(&self, id: impl Key) -> DocumentCall<'_, T>
```

A set of the document `id` with its vector, run by its awaited last
step: `products.id("p-3").vector(&embedding).set(&product).await`.

### aio::search::Index.set_many

```rust
pub fn set_many<'v>(
    &self,
    documents: impl IntoIterator<Item = impl Document<'v, T>>,
) -> impl Future<Output = Result<()>> + Send + 'static
where
    T: 'v,
```

Sets each document to its value, all of them in one commit or none,
as `crate::search::Index::set_many` does.

### aio::search::Index.delete

```rust
pub fn delete(&self, id: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static
```

Deletes the document `id`, as `crate::search::Index::delete` does;
says whether there was one.

## aio::search::DocumentCall

```rust
pub struct DocumentCall<'a, T> { /* private fields */ }
```

A set of one document with its vector, run by `DocumentCall::set`.

### aio::search::DocumentCall.vector

```rust
pub fn vector(self, vector: &'a [f32]) -> Self
```

As `crate::search::DocumentCall::vector`.

### aio::search::DocumentCall.without_vector

```rust
pub fn without_vector(self) -> Self
```

As `crate::search::DocumentCall::without_vector`.

### aio::search::DocumentCall.set

```rust
pub fn set(self, value: &T) -> impl Future<Output = Result<()>> + Send + 'static
```

Sets the document to `value`, its words and its vector, as
`crate::search::DocumentCall::set` does.

## aio::search::SearchCall

```rust
pub struct SearchCall<'i, T> { /* private fields */ }
```

A search of an index, run by its awaited `SearchCall::run`.

### aio::search::SearchCall.vector

```rust
pub fn vector(self, vector: &'i [f32]) -> Self
```

As `crate::search::SearchCall::vector`.

### aio::search::SearchCall.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

As `crate::search::SearchCall::vector_weight`.

### aio::search::SearchCall.rerank

```rust
pub fn rerank(self) -> Self
```

As `crate::search::SearchCall::rerank`.

### aio::search::SearchCall.filter

```rust
pub fn filter(self, condition: Condition) -> Self
```

As `crate::search::SearchCall::filter`.

### aio::search::SearchCall.limit

```rust
pub fn limit(self, limit: usize) -> Self
```

As `crate::search::SearchCall::limit`.

### aio::search::SearchCall.offset

```rust
pub fn offset(self, offset: usize) -> Self
```

As `crate::search::SearchCall::offset`.

### aio::search::SearchCall.run

```rust
pub fn run(self) -> impl Future<Output = Result<Found<T>>> + Send + 'static
```

Runs the search on the store's pool, as
`crate::search::SearchCall::run` does, each hit's value decoded
there. Its text, its limits, its filters and its vector are checked
here, so that a search refused for them answers in its first poll and
never reaches the pool.

## aio::search::TableIndexBuilder

```rust
pub struct TableIndexBuilder<T> { /* private fields */ }
```

What `db.search_index` opens: a table's index and its settings, as
`crate::search::TableIndexBuilder` gives them, opened into an index
whose calls are awaited.

### aio::search::TableIndexBuilder.table

```rust
pub fn table(self, table: &str) -> Self
```

As `crate::search::TableIndexBuilder::table`.

### aio::search::TableIndexBuilder.key

```rust
pub fn key(self, column: &str) -> Self
```

As `crate::search::TableIndexBuilder::key`.

### aio::search::TableIndexBuilder.field

```rust
pub fn field(self, column: &str, weight: f64) -> Self
```

As `crate::search::TableIndexBuilder::field`.

### aio::search::TableIndexBuilder.fields

```rust
pub fn fields<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self
```

As `crate::search::TableIndexBuilder::fields`.

### aio::search::TableIndexBuilder.filters

```rust
pub fn filters<S: AsRef<str>>(self, columns: impl IntoIterator<Item = S>) -> Self
```

As `crate::search::TableIndexBuilder::filters`.

### aio::search::TableIndexBuilder.typo_tolerance

```rust
pub fn typo_tolerance(self, on: bool) -> Self
```

As `crate::search::TableIndexBuilder::typo_tolerance`.

### aio::search::TableIndexBuilder.vectors

```rust
pub fn vectors(self, column: &str, dimensions: usize) -> Self
```

As `crate::search::TableIndexBuilder::vectors`.

### aio::search::TableIndexBuilder.recall

```rust
pub fn recall(self, recall: f64) -> Self
```

As `crate::search::TableIndexBuilder::recall`.

### aio::search::TableIndexBuilder.vector_weight

```rust
pub fn vector_weight(self, weight: f64) -> Self
```

As `crate::search::TableIndexBuilder::vector_weight`.

### aio::search::TableIndexBuilder.rerank

```rust
pub fn rerank(self) -> Self
```

As `crate::search::TableIndexBuilder::rerank`.

### aio::search::TableIndexBuilder.rebuild

```rust
pub fn rebuild(self, rebuild: Rebuild) -> Self
```

As `crate::search::TableIndexBuilder::rebuild`.

### aio::search::TableIndexBuilder.open

```rust
pub fn open(self) -> Result<TableIndex<T>>
```

Opens the index as `crate::search::TableIndexBuilder::open` does: a
plain call, made once at start, its table's rows read in the
background, which `TableIndex::ready` waits for.

### aio::search::TableIndexBuilder.reindex

```rust
pub fn reindex(self) -> Result<TableIndex<T>>
```

Opens the index as [`open`](Self::open) does, and reads it again
beside itself in the background.

## aio::search::TableIndex

```rust
pub struct TableIndex<T> { /* private fields */ }
```

The rows of a table found again by their words, their vectors or both, its
calls awaited: the same index as `crate::search::TableIndex`, and the
same again through `From` and [`blocking`](Self::blocking). It has no
set or delete: its rows are the table's, and the index changes in their
commit.

### aio::search::TableIndex.blocking

```rust
pub fn blocking(&self) -> &TableIndex<T>
```

The same index, for its blocking calls.

### aio::search::TableIndex.name

```rust
pub fn name(&self) -> &str
```

### aio::search::TableIndex.search

```rust
pub fn search(&self, text: &str) -> SearchCall<'_, T>
```

A search by `text`'s words, as `crate::search::TableIndex::search`
makes one: each hit's id is its row's key and its value the row, read
on the store's pool in the snapshot its search read.

### aio::search::TableIndex.search_vector

```rust
pub fn search_vector<'i>(&self, vector: &'i [f32]) -> SearchCall<'i, T>
```

A search by `vector` alone, as
`crate::search::TableIndex::search_vector` makes one.

### aio::search::TableIndex.status

```rust
pub fn status(&self) -> impl Future<Output = Result<Status>> + Send + 'static
```

Where the index stands, as `crate::search::TableIndex::status` reads
it, on the store's pool.

### aio::search::TableIndex.ready

```rust
pub fn ready(&self) -> impl Future<Output = Result<()>> + Send + 'static
```

Waits until the index has read its table's rows and serves the
settings it was opened with, as `Index::ready` waits: dropped, it
stops waiting, and the table's rows are read on.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
