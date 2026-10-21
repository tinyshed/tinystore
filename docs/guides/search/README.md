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

## In these guides

| Page                                    | Covers                                              |
|-----------------------------------------|-----------------------------------------------------|
| [Fields and weights](fields.md)         | which text is read, and how much each field counts  |
| [Typos](typos.md)                       | the nearest word, `corrected`, `typoTolerance`      |
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
