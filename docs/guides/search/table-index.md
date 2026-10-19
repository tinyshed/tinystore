# A table's index

An index opened from an application's database follows one of its tables.
The table owns the rows; the index finds them again by their words and
vectors, and changes in the same commit as each row. There is no second copy
to keep in sync, and no moment when the table and the index disagree.

```ts
interface Article {
	id: number
	slug: string
	title: string
	body: string
	author_id: number
	published: number
}

const db = await store.database('blog', { migrations: `${import.meta.dirname}/migrations` })
const articles = db.searchIndex<Article>('articles_search', {
	table: 'articles',
	key: 'slug',
	fields: { title: 2, body: 1 },
	filters: ['author_id', 'published'],
	vectors: { column: 'embedding', dimensions: 1024 },
})
await articles.ready() // the rows the table already holds, read once

const { hits } = await articles.search({ text: 'хлеба', where: { author_id: 7, published: true } })
hits[0] // { id: 'bread', value: { id: 1, slug: 'bread', title: 'Как испечь хлеб', … }, score: … }
```

```python
db = await store.database("blog", migrations=Path(__file__).parent / "migrations")
articles = db.search_index(
    "articles_search", Article,
    table="articles",
    key="slug",
    fields={"title": 2, "body": 1},
    filters=["author_id", "published"],
    vectors=tinystore.Vectors(column="embedding", dimensions=1024),
)
await articles.ready()  # the rows the table already holds, read once

result = await articles.search("хлеба", where={"author_id": 7, "published": True})
```

```go
db, err := sqldb.Open(ctx, store, "blog", sqldb.Migrations(os.DirFS("migrations")))
articles, err := search.OpenIndex[Article](db, "articles_search",
	search.Table("articles"),
	search.Key("slug"),
	search.Fields{"title": 2, "body": 1},
	search.Filters("author_id", "published"),
	search.Vectors{Column: "embedding", Dimensions: 1024})
err = articles.Ready(ctx) // the rows the table already holds, read once

result, err := articles.Search(ctx, "хлеба", search.Where{"author_id": 7, "published": true})
```

```rust
let db = store.database("blog").migrations("migrations").open()?;
let articles = db
    .search_index::<Article>("articles_search")
    .table("articles")
    .key("slug")
    .field("title", 2.0)
    .field("body", 1.0)
    .filters(["author_id", "published"])
    .vectors("embedding", 1024)
    .open()?;
articles.ready()?; // the rows the table already holds, read once

let found = articles.search("хлеба").filter(eq("author_id", &7)).filter(eq("published", &true)).run()?;
```

| Option    | What it says                                                                         |
|-----------|--------------------------------------------------------------------------------------|
| `table`   | the table whose rows the index holds                                                 |
| `key`     | a column of unique values that names a row, a hit's `id`; the primary key unless set |
| `fields`  | the columns whose words are read, with their weights                                 |
| `filters` | the columns a search may keep to; a boolean matches the 1 or 0 SQLite keeps          |
| `vectors` | the column of vectors your program fills, and their dimensions                       |

A hit's `value` is its row as `db.all` reads it, every column. The index
lives in the database's file, `sql/blog.db`, beside the table.

## Every change reaches it

Opening the index puts triggers on the table that note each changed row's
key. Every transaction of the file reads those rows into the index before it
commits:

- an insert, an update or a delete by any statement, a trigger, a cascade or
  a migration reaches the index in the commit that makes it; a rollback
  changes neither (contract SRCH-07);
- a write by another program beside the store, `tinystore sql --write` or a
  SQLite shell, reaches it at the store's next write to the file, or within a
  second (contract SRCH-08);
- a migration that rebuilds the table, a new table copied and renamed, is
  read again whole.

```ts
await db.exec`update articles set title = ${'Хлеб без дрожжей'} where slug = ${'bread'}`
await articles.search({ text: 'дрожжей' }) // the new title, already
```

```python
await db.exec("update articles set title = ? where slug = ?", "Хлеб без дрожжей", "bread")
await articles.search("дрожжей")  # the new title, already
```

```go
_, err = db.Exec(ctx, `update articles set title = ? where slug = ?`, "Хлеб без дрожжей", "bread")
result, err = articles.Search(ctx, "дрожжей") // the new title, already
```

```rust
db.exec(sql!("update articles set title = ? where slug = ?", "Хлеб без дрожжей", "bread"))?;
let found = articles.search("дрожжей").run()?; // the new title, already
```

A write the index cannot take fails alone: a row whose `embedding` is not a
vector of the index's dimensions fails its own insert, and the writes beside
it in the shared commit go on.

## Vectors in a column

The vectors column is a `blob` of little-endian 32-bit floats, one vector a
row, or `null` for a row found by its words only:

```sql title="migrations/0002_embeddings.sql"
alter table articles add column embedding blob;
```

```ts
const bytes = new Uint8Array(new Float32Array(vector).buffer)
await db.exec`update articles set embedding = ${bytes} where slug = ${slug}`
```

```python
await db.exec("update articles set embedding = ? where slug = ?", struct.pack(f"<{len(vector)}f", *vector), slug)
```

```go
bytes := make([]byte, 4*len(vector))
for i, f := range vector {
	binary.LittleEndian.PutUint32(bytes[4*i:], math.Float32bits(f))
}
_, err = db.Exec(ctx, `update articles set embedding = ? where slug = ?`, bytes, slug)
```

```rust
let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
db.exec(sql!("update articles set embedding = ? where slug = ?", bytes, slug))?;
```

The program fills the column when it has the vector, often from a job that
asks the model after the row is saved. Until then the row is found by its
words.

## Searching inside a transaction

In Rust, a transaction searches the index through `tx.with(&index)`, and sees
what the transaction wrote, before it commits:

```rust
let found = db.tx(|tx| -> tinystore::Result<usize> {
    tx.exec(sql!("update articles set title = ? where slug = ?", "Хлеб без дрожжей", "bread"))?;
    Ok(tx.with(&articles).search("дрожжей").run()?.hits.len()) // 1: the transaction's own write
})?;
```

The SDKs search outside transactions.

## Settings and dropping

The index's fields, filters, weights and vectors change as a store's index's
do: open it with other settings, and it reads the table again beside itself
while it answers. See [Rebuilds](rebuilds.md).

```ts
await db.dropSearchIndex('articles_search') // true: its triggers off the table, its rows dropped
```

```python
await db.drop_search_index("articles_search")
```

```go
dropped, err := search.Drop(ctx, db, "articles_search")
```

```rust
db.drop_search_index("articles_search")?;
```

## Why this design?

**Why the index follows the table.** With a separate search service, the
program writes the row and then the document, and every bug, crash or retry
between the two leaves them apart. Teams then write jobs that compare and
repair. Triggers note each change inside SQLite, and every commit brings the
index up to date before it ends, so the two cannot drift.

**Why triggers note keys, not documents.** A trigger runs inside SQLite's
statement, where the index's own work, cutting text into words, cannot run.
So a trigger writes only the changed row's key, and the store reads those rows
once, at the commit: a row updated ten times in a transaction is indexed
once.

## See also

- [Search](README.md): the engine's overview.
- [Buckets and queues in a database](../sql/buckets-and-queues.md): what
  else can live in a database's file.
- [Finding old messages](../../handbook/chat/finding-old-messages.md) in the
  Handbook indexes a chat's messages this way.
