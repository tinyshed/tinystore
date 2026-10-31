# Search

The Search engine finds documents by their words in any language, forgiving
typos, by the vectors your embedding model makes, or by both. An index keeps
its own documents in `search.db`, or follows a table of an application's
database and changes in the same commit as its rows. Searches are exact by
default.

```ts
const articles = store.index<Article>('articles', { fields: { title: 2, body: 1 }, filters: ['authorId'] })

await articles.set('a-17', { title: 'Как испечь хлеб', body: '…', authorId: 42 })
const { hits, corrected } = await articles.search({ text: 'хлеба', where: { authorId: 42 } })
```

```python
articles = store.index("articles", Article, fields={"title": 2, "body": 1}, filters=["author_id"])

await articles.set("a-17", Article(title="Как испечь хлеб", body="…", author_id=42))
result = await articles.search(text="хлеба", where={"author_id": 42})
```

```go
articles, err := search.OpenIndex[Article](store, "articles",
	search.Fields{"title": 2, "body": 1}, search.Filters("author_id"))

err = articles.Set(ctx, "a-17", Article{Title: "Как испечь хлеб", Body: "…", AuthorID: 42})
result, err := articles.Search(ctx, "хлеба", search.Where{"author_id": 42})
```

```rust
let articles = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).filters(["author_id"]).open()?;

articles.set("a-17", &Article { title: "Как испечь хлеб".into(), body: "…".into(), author_id: 42 })?;
let result = articles.search("хлеба").filter(eq("author_id", &42)).run()?;
```

## What it guarantees

- A search by words answers what scoring every document would, by BM25,
  never an estimate (contract SRCH-01).
- A search by a vector answers the exact best by cosine, unless the index
  asked for `recall` (contract SRCH-02).
- A filter keeps to the documents it passes before ranking (contract
  SRCH-03).
- A document set is found by the next search; a deleted one never again
  (contract SRCH-04).
- A table's index changes in the commit that changes the row (contract
  SRCH-07).
- A misspelt word is read as its nearest, and `corrected` says so; a word
  that matches is never changed (contract SRCH-09).

## Listing what it holds

An index finds; it does not list. A search with no words and no vector finds
nothing, an empty page rather than every document, and no call walks an
index's documents. An album that shows its photos newest first and finds them
by their words keeps them in a table, and the table's index follows it:

```ts
const photos = db.table<Photo>('photos')
const found = db.searchIndex<Photo>('photos_search', { table: 'photos', fields: { title: 2, caption: 1 } })

const page = await photos.orderBy('taken_at', 'desc').limit(50).all() // the album, listed
const { hits } = await found.search({ text: 'море' }) // the same rows, found by their words
```

```python
photos = db.table("photos", Photo)
found = db.search_index("photos_search", Photo, table="photos", fields={"title": 2, "caption": 1})

page = await photos.order_by("taken_at", "desc").limit(50).all()
result = await found.search("море")
```

```go
photos := sqldb.OpenTable[Photo](db, "photos")
found, err := search.OpenIndex[Photo](db, "photos_search", search.Table("photos"), search.Fields{"title": 2, "caption": 1})

page, err := photos.OrderBy("taken_at", sqldb.Desc).Limit(50).All(ctx)
result, err := found.Search(ctx, "море")
```

```rust
let photos = db.table::<Photo>("photos");
let found = db.search_index::<Photo>("photos_search").table("photos").field("title", 2.0).field("caption", 1.0).open()?;

let page = photos.order_by("taken_at", Direction::Desc).limit(50).all()?;
let result = found.search("море").run()?;
```

A row and its words change in one commit, so the list and the search never
disagree. An index of its own, `store.index`, suits a program that lists its
documents from somewhere else, a bucket by the same ids, or never lists them;
a bucket and an index commit apart, as [two
files](../../handbook/start/directory.md#two-engines-in-one-file) do. See [A
table's index](table-index.md).

## Async Rust

This section is only about Rust. In Bun, Node and Python every call is
already awaited, and in Go a call blocks its goroutine, which costs little.

With the `async` feature, `tinystore::aio::search` has the same indexes, and
each call that reads, writes or waits returns a future. Open the index from
an `aio::Store`, and add `.await`:

```rust
use tinystore::Options;
use tinystore::aio::Store;
use tinystore::aio::search::eq;

let store = Store::open("data", Options::default())?;
let articles = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).filters(["author_id"]).open()?;

articles.set("a-17", &bread).await?;
articles.set_many([("a-18", &rye), ("a-19", &spelt)]).await?;
let result = articles.search("хлеба").filter(eq("author_id", &42)).run().await?;
let one = articles.get("a-17").await?; // Some(bread)
let gone = articles.delete("a-17").await?; // true: there was one
articles.ready().await?; // returns once a rebuild has switched in
```

Opening an index is still a plain call, because a program opens its handles
once at start. The answers are the same types as in the blocking API:
`Found`, `Hit`, `Status` and `Listed`, and the filters `eq`, `is_in`,
`range`, `missing`, `or` and `and`.

| Blocking                                   | Async                                                      |
|--------------------------------------------|------------------------------------------------------------|
| `articles.set("a-17", &bread)?`            | `articles.set("a-17", &bread).await?`                      |
| `articles.id("d-1").vector(&v).set(&doc)?` | `articles.id("d-1").vector(&v).set(&doc).await?`           |
| `articles.set_many(documents)?`            | `articles.set_many(documents).await?`                      |
| `articles.get("a-17")?`                    | `articles.get("a-17").await?`                              |
| `articles.delete("a-17")?`                 | `articles.delete("a-17").await?`                           |
| `articles.search("хлеба").run()?`          | `articles.search("хлеба").run().await?`                    |
| `articles.status()?`, `articles.ready()?`  | `articles.status().await?`, `articles.ready().await?`      |
| `store.drop_index("articles")?`            | `store.drop_index("articles").await?`                      |
| `tinystore::search::list(&store)?`         | `tinystore::aio::search::list(&store).await?`              |
| `tinystore::search::maintain(&store)?`     | blocking: `tinystore::search::maintain(store.blocking())?` |

### What a dropped future does

- A `set`, `set_many` or `delete` that was polled once commits, even if you
  drop its future (contract DUR-11). Only its answer is lost. A write whose
  future is never polled writes nothing.
- A search that you drop stops: its SQLite statement is interrupted and its
  reader is freed at once (contract STORE-09). So a timeout around a search
  stops it:

  ```rust
  let found = tokio::time::timeout(Duration::from_secs(2), articles.search(&text).run()).await;
  ```

- If you drop `ready`, it stops waiting. The rebuild goes on in the
  background.

### Where the work runs

None of these calls commits, syncs the disk or waits on the thread that polls
its future (contract STORE-10):

- A set makes the document's JSON when you call it, and its words in the
  future's first poll. When its documents add up to more than 4 KiB of JSON,
  for example in a `set_many` of many documents, it makes the words on the
  store's own threads instead. It commits on the store's own threads, and the next search sees it
  once the future is ready.
- `get` answers in the first poll when one of the file's readers is free,
  and otherwise on the store's threads.
- A search, `status` and `list` run on the store's threads, and each hit is
  decoded there, so the document type must be `Send + 'static`. A search
  refused for its text, its limit or its vector fails in the first poll.
- `ready` holds no thread while it waits: the index wakes it when the rebuild
  switches in or fails. It also checks the index's status on the store's
  threads every 100 ms. On a store opened without background work, `ready`
  runs the maintenance itself, on the store's threads.

`Index::from(index)` and `index.blocking()` cross between the two APIs. Both
give the same index, never a second one. A table's index has the same awaited
search, `status` and `ready`: see [A table's index](table-index.md#async-rust).

## In these guides

| Page                                    | Covers                                              |
|-----------------------------------------|-----------------------------------------------------|
| [Fields and weights](fields.md)         | which text is read, and how much each field counts  |
| [Typos](typos.md)                       | the nearest word, `corrected`, `typoTolerance`      |
| [As you type](prefix.md)                | `prefix`: the last word as the start of others      |
| [Filters](filters.md)                   | `where`, lists, ranges, `search.or`                 |
| [Vectors](vectors.md)                   | dimensions, exact search, `recall` and clusters     |
| [Words and vectors together](hybrid.md) | `vectorWeight`, `rerank`                            |
| [A table's index](table-index.md)       | `db.searchIndex`, triggers, `ready`                 |
| [Rebuilds](rebuilds.md)                 | changing fields and filters while the index answers |

New to Search? [Finding old messages](../../handbook/chat/finding-old-messages.md)
in the Handbook indexes a chat's messages, and [How search
finds](../../handbook/inside/how-search-finds.md) explains the index.

## Reference

[Bun and Node](../../reference/bun/search.md) ·
[Python](../../reference/python/search.md) ·
[Go](../../reference/go/search.md) ·
[Rust](../../reference/rust/search.md)
