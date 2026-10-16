# search: the API book

Accepted by the owner on 16 October 2026, after a newcomer check and the
owner's and an outside reviewer's readings (below), and closed by the owner
on 17 October, its eight slices built. Documents a program sets
by id in a named index and finds again by their words, by vectors it gives
with them, or by both, kept to the documents a filter passes.
[dx.md](../dx.md) holds the rules it follows. The evidence is research's
rounds of 16 October:
[search-plan](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-plan-2026-10-16.md),
[search-1m](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-1m-2026-10-16.md),
[search-blocks](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-blocks-2026-10-16.md),
[search-compact](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-compact-2026-10-16.md),
[search-vectors](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-vectors-2026-10-16.md),
[search-vchurn](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-vchurn-2026-10-16.md),
[search-wchurn](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-wchurn-2026-10-16.md)
and
[search-hybrid](https://github.com/tinyshed/research/blob/main/tinystore/reports/search-hybrid-2026-10-16.md).
The Go engine had no search.

## What a newcomer learns

| Word            | What it is                                                                                       |
|-----------------|--------------------------------------------------------------------------------------------------|
| an index        | a name documents of one type are kept and found under, `store.index('articles')`                 |
| a document      | a value set by its id, as a bucket's is; the index keeps it and answers with it                  |
| fields          | which of a document's text its words are read from, and how much each weighs                     |
| a vector        | numbers the program's own model gave a document or a question; never made here                   |
| a filter        | a field of the document a search can keep its hits to, declared at open: `where: { userId: 42 }` |
| a hit           | `{ id, value, score }`: a document found, and what ordered it                                    |
| a table's index | `db.searchIndex`: an SQL table's rows, found as an index's documents, kept in step by the store  |

The engine is `search`, its file `search.db`; what a program opens in it is
an index, and what it does with one is `search`. An index answers the best
ten, or `limit`, of what it holds, every time the same for the same
documents. Words need no setup: any language, any script, a typo forgiven.
Vectors are optional, and the program brings them.

## Open an index

```ts
const articles = store.index<Article>('articles', { fields: { title: 2, body: 1 } })
const posts = store.index<Post>('posts', { fields: ['title', 'body'] })   // every field weighs 1
const notes = store.index<string>('notes')                                // a string is its own field
const products = store.index<Product>('products', {
  fields: { name: 3, description: 1 },
  filters: ['category', 'ownerId'],
  vectors: { dimensions: 1024 },
  hybrid: { vectorWeight: 0.5 },
})
```

```python
articles = store.index("articles", Article, fields={"title": 2, "body": 1})
posts = store.index("posts", Post, fields=["title", "body"])
products = store.index("products", Product, fields={"name": 3, "description": 1},
                       filters=["category", "owner_id"], vectors=Vectors(dimensions=1024),
                       hybrid=Hybrid(vector_weight=0.5))
```

```rust
let articles = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open()?;
let posts = store.index::<Post>("posts").fields(["title", "body"]).open()?;
let products = store
    .index::<Product>("products")
    .field("name", 3.0)
    .field("description", 1.0)
    .filters(["category", "owner_id"])
    .vectors(1024)
    .vector_weight(0.5)
    .open()?;
```

- A field is a path into the document whose text is read, a string or a
  list of strings; a field missing from a document is empty. Its number is
  how much a word found there weighs against the others: a word in a field
  of 2 scores twice what it would in a field of 1. A list weighs every field
  1, and is the same index as the object that says so; a weight must be
  above zero.
- `filters` names the fields a search may keep its hits to, below.
  `hybrid` says how a search by words and a vector at once combines the
  two, below; it is the default every such search starts from, and a
  search may say otherwise.
- Every index `store.index` opens lives in `search.db`: an index's
  documents, words and vectors commit together, and no other engine waits on
  its writer. One of an SQL table lives in that database's file, below.

## Change an index

```ts
store.index('articles', { fields: { title: 2, body: 1 } })                    // before a deploy
store.index('articles', { fields: { title: 5, body: 1, description: 1 } })    // after it
```

```text
search articles: title weighs 5, was 2; at once
search articles: description added: reading 812345 documents beside the index, which answers as before until then
search articles: description added ready in 41 s; switched
```

```rust
let articles = store.index::<Article>("articles").field("title", 5.0).field("body", 1.0).field("description", 1.0).open()?;
```

Opening an index with other settings than it holds changes it, and opening
never waits for the work a change needs.

| What changed                                               | What happens                                                                                         |
|------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| a field's weight, `typoTolerance`, `hybrid`                | at once: a search reads the weights and its blocks' bounds when it runs, so nothing is written again |
| a field or a filter removed                                | at once from searches; its keys dropped by reading the index again in the background                 |
| a field or a filter added                                  | read from the kept documents in the background; searches use it once it is ready                     |
| `recall` added or removed                                  | the clusters built or dropped in the background; searches stay exact meanwhile                       |
| how text is cut, by a release of TinyStore that changes it | the index rebuilt in the background                                                                  |
| `vectors` added                                            | at once: the documents the index holds have none until they are set again                            |
| the vectors' `dimensions`, or `vectors` left out           | `invalid`: the vectors are the program's model's, and the store has nothing to make new ones from    |

- Background work runs beside the index, which answers and takes writes
  meanwhile: the new rows read the old ones' documents in the order of
  their numbers, 256 a commit, worked out in a snapshot so that the
  writer is held only to write them; a set meanwhile has a later number,
  which they reach in turn, and a delete reaches both at once. Once a step
  finds fewer than 256, the last are read and the new rows switched in, in
  one commit no write runs beside, and the old ones dropped a step a
  commit. A restart continues where it stopped.
- The new rows number the documents again from 1, in the order the old
  ones did, so that of equal scores the document set first still comes
  first. Reading an index again by its own settings is how its numbers are
  kept near its documents: once the numbers it gave pass twice the
  documents it holds, and 4,096, it begins by itself, as it does in any
  case past 2³¹.
- A rebuild that cannot read a document, a number where the new settings
  read text, fails, and `status()` says why: the index answers by the old
  settings, and the next change or `reindex` begins again.
- Every change is said in the log when it starts and when it is done.

```ts
const articles = store.index<Article>('articles', { fields, rebuild: 'manual' })
await store.reindex('articles', { fields: { title: 2, body: 1, description: 1 } })

await articles.ready()        // once the index serves the settings it was opened with
await articles.status()       // { state: 'rebuilding', done: 0.42 } | { state: 'ready' } | { state: 'failed', error }
```

```rust
let articles = store.index::<Article>("articles").fields(["title", "body"]).rebuild(Rebuild::Manual).open()?;
let articles = store.index::<Article>("articles").fields(["title", "body", "description"]).rebuild(Rebuild::Manual).reindex()?;

articles.ready()?;
articles.status()?;           // Status::Rebuilding { done: 0.42 } | Status::Ready | Status::Failed { error }
```

- `rebuild: 'manual'` is for a program that schedules its own load: a
  change that needs background work is `invalid`, naming it, until
  `store.reindex` asks for it. A change that needs none applies all the same,
  and a field removed leaves its keys for the next rebuild. `reindex` with
  nothing to change numbers the index again; a manual index is numbered
  again only so, or past 2³¹.
- A search never waits for `ready()`: it answers by the settings the index
  serves, which `status()` says.

## Drop an index

```ts
await store.dropIndex('posts')            // true when there was one
await store.indexes()                     // [{ name, documents, vectors, opened, status }]
```

```rust
store.drop_index("posts")?;                       // true when there was one
let listed = tinystore::search::list(&store)?;     // name, documents, vectors, opened, status
```

```bash
tinystore search list ./data
```

```bash
tinystore search drop ./data posts
```

- An index the program stopped opening stays: another version of it may
  open the index again, so the store never guesses. `tinystore search
  list` shows each index with its documents, its vectors, when it was last
  opened and a rebuild under way; its size is not counted apart, since its
  rows share `search.db` with every other index's.
- Dropping marks the index's rows in one commit and drops them a step a
  commit in the background; its open handles are refused from then on, and
  opening the name again makes a new, empty index.

## Set and delete

```ts
await articles.set('a-17', { title: 'Хлеб на закваске', body: '…' })
await products.set('p-3', product, { vector: embedding })          // number[] or Float32Array
await products.set('p-4', draft, { vector: null })                 // found by its words alone
await articles.delete('a-17')
const article = await articles.get('a-17')                          // the value, or undefined
await articles.setMany([{ id: 'a-1', value: first }, { id: 'a-2', value: second }])
```

```rust
articles.set("a-17", &article)?;
products.id("p-3").vector(&embedding).set(&product)?;
products.id("p-4").without_vector().set(&draft)?;
articles.delete("a-17")?;
articles.set_many([("a-1", &first), ("a-2", &second)])?;
products.set_many([("p-5", &first, Some(&va[..])), ("p-6", &second, None)])?;
```

- A set replaces the document whole, its words and its vector; the next
  search sees it. On an index with `vectors`, `vector` is required: a set
  that leaves it out is `invalid`, so that editing a title cannot silently
  drop a document's vector; `vector: null` keeps a document only its words
  find.
- A vector of another length, or one holding NaN or an infinity, is
  `invalid`. A vector is kept as given, every bit, and compared by its
  cosine with the question's, from the lengths kept beside it.
- Deleting an id that is not there does nothing.
- `setMany` sets a list of documents in one commit, all of them or none: a
  refused one names its place in the list and its id. A call holds 1,000
  documents and 16 MiB at most, so that no import makes one commit and a
  write-ahead log of its size; a larger import goes in calls of that size.
- A set replaces a document whole, and there is no `patch` of some of its
  fields: what a part of a document's nested values, lists and `null`
  would mean, and whether a changed text leaves a vector that no longer
  fits it, wait for a program that needs one (the owner's call below).

## Search

```ts
const { hits } = await articles.search('как испечь хлеб')                        // by words
const { hits } = await products.search({ text: 'red shoes', vector: q })        // by both
const { hits } = await products.search({ vector: q })                           // by the vector alone
const { hits } = await articles.search({ text: 'хлеб', limit: 20, offset: 20 })
const { hits, corrected } = await articles.search('helo wrld')                  // corrected: 'hello world'
const { hits } = await products.search({ text: 'red shoes', where: { ownerId: 42 } })

for (const hit of hits) show(hit.id, hit.value.title)
```

```python
hits = articles.search("как испечь хлеб").hits
hits = products.search(text="red shoes", vector=q, where={"owner_id": 42}).hits
```

```rust
let found = articles.search("как испечь хлеб").run()?;
let found = products.search_vector(&q).run()?;
let found = products.search("red shoes").vector(&q).filter(eq("owner_id", &42)).limit(20).run()?;
```

- A string is a search by its words: `search('хлеб')` is
  `search({ text: 'хлеб' })`. A document needs any of a search's words, not
  all; a word most documents hold weighs little.
- Words are matched as written and by their stem, in the language each
  script and word is written in.
- A search by words answers the exact best by BM25 over the fields and their
  weights: the same as scoring every document, never an estimate.
- A word of five to eight letters that matches nothing is read with one
  letter changed, one of nine or more with two, as Meilisearch does; a word
  that matches something is never changed. `corrected` holds the words the
  search read in fact, the ones it found its hits by, and is absent when it
  read them as given. `typoTolerance: false` at open turns it off.
- `score` orders one search's hits and means nothing beside another
  search's.

## Corrections outside an index

```rust
let commands = tinystore::typos::Dictionary::new(["serve", "backup", "restore"]);
let near = commands.nearest("bakup", 1);         // Some(Near { word: "backup", edits: 1 })
```

- What reads a search's misspelled words is a module of the core's own,
  `tinystore::typos`, which knows nothing of search or of a store: a set of
  words, and the nearest of them to a word given. The search engine builds
  one from an index's words; the command line suggests a command or a flag
  with it, `did you mean backup?`, and config a key it does not know. Like
  `compress`, it is shared by engines that never import each other.
- It answers the nearest word with how many edits it took, and applies no
  rule of its own: search reads a word with one edit from five letters and
  two from nine, as Meilisearch does; the command line, whose words are
  short, takes one from three and chooses among several. How much of it is
  public is the slice's question.
- A suggestion changes nothing. Config names the key it does not know and
  the one it may have meant in its error, `config app: app.yaml: log.levle:
  no such setting; did you mean log.level?`, and never reads one as the
  other; the command line says `tinystore: no command bakup; did you mean
  backup?`. Built in the core, 16 October.
- `typos` is a feature of its own, which `search` and `config` turn on, the
  command line through config: fst and levenshtein_automata, about 0.13 MiB
  measured in search-plan, and a build without them links neither.

## Vectors and words together

```ts
await products.search({ text: 'red shoes', vector: q })                                  // the index's vectorWeight
await products.search({ text: 'red shoes', vector: q, hybrid: { vectorWeight: 0.7 } })   // this search's
await products.search({ text: 'red shoes', vector: q, hybrid: { rerank: true } })        // the words' hits, ordered by the vector
```

```rust
let found = products.search("red shoes").vector(&q).vector_weight(0.7).run()?;
let found = products.search("red shoes").vector(&q).rerank().run()?;
```

- A search by both asks each half for its best hundred, or `offset +
  limit` if more; scales each half's scores to 0–1 by its own lowest and
  highest, a document the other half did not find scoring 0 there; and
  orders the documents by (1 − `vectorWeight`) × words + `vectorWeight` ×
  vector. `vectorWeight` is 0.5 unless the index or the search says
  otherwise, from 0, the search by its words alone, to 1, by its vector
  alone, each with its own scores rather than a sum the other half adds
  documents to at 0.
- Pages of a search by both join within the first hundred, where both
  halves stay the same; past it each page asks the halves for more, and
  their sums may order it otherwise.
- The weight is the only setting a search may give in place of its index's:
  one index serves searches that weigh differently, a catalog's search box
  and a support page's, and a weight changes no stored byte, so neither
  needs an index of its own.
- `rerank: true` orders the words' hundred by their vectors' cosine with
  the question's and finds nothing the words did not: no clusters and no
  scan of every vector, a hundred vectors read by their number. It is for a
  program whose questions always have words and whose vectors only order
  them. At open it makes every search by both rerank; beside a
  `vectorWeight` in the same `hybrid` it is `invalid`. A hit without a
  vector comes after those with one, scored −1, the least a cosine can be.
- search-hybrid, bge-m3 on Amazon's products in three languages and twelve
  StackExchange forums, 600 judged questions each, nDCG@10:

  | mode               | products | forums |
  |--------------------|----------|--------|
  | words              | 0.306    | 0.240  |
  | vectors, exact     | 0.307    | 0.378  |
  | fusion by rank     | 0.359    | 0.348  |
  | `vectorWeight` 0.5 | 0.361    | 0.366  |
  | `vectorWeight` 0.7 | 0.348    | 0.391  |
  | `rerank`           | 0.361    | 0.353  |

  Scores summed beat fusion by rank at equal weight in all four pairs of
  the round, outside the 95 % interval in three; no weight won both
  collections, so the program chooses it, from 0.5. `rerank` cost 0.6–0.8
  ms on top of the words and lost nothing to clusters, which cost every
  fusion 0.014–0.039. On MIRACL, the model's own domain, the vectors alone
  led the words 0.736 to 0.432 and no fusion by rank passed them
  (search-vectors).

## Exact and approximate vectors

```ts
const products = store.index<Product>('products', { fields, vectors: { dimensions: 1024 } })
const catalog = store.index<Product>('catalog', { fields, vectors: { dimensions: 1024, recall: 0.95 } })
```

```rust
let catalog = store.index::<Product>("catalog").field("name", 1.0).vectors(1024).recall(0.95).open()?;
```

- Without `recall`, a search by a vector answers the exact best: the same
  as scoring every vector, scores bit for bit. search-vectors measured 245
  ms a million vectors on two cores, by reading a byte a dimension and
  scoring again on the vectors as given the 25 a question that bytes could
  not rule out; it grows with the index, about 25 ms a hundred thousand.
- Slice 3 keeps a vector's code in a row of its own and reads them a row
  at a time, holding no vector in memory but the ones it scores again: 83
  ms at the median over 100,000 vectors of 1,024 dimensions, 20 searches
  after a first, the file in the system's cache, on a Ryzen 7 7700 under
  Windows 11, nearly all of it reading the rows, since the same scan
  without a bound computed took as long; search-vectors read codes packed
  into its clusters' lists, which a later release may pack so for the
  exact search too. A vector of 1,024 dimensions keeps 4 KiB and its code
  1 KiB. `a_search_by_a_vector_over_a_hundred_thousand_takes` in
  `search::index`, by hand with `--release -- --ignored`.
- With `recall`, the index answers from the clusters nearest the question:
  about 3 ms a million at 0.94 of the exact ten, holding 96 MiB, against
  HNSW's 0.4 ms and 2.2 GiB. The program asked for it at open, so no answer
  says it again.
- How many clusters a search reads is the index's to choose from `recall`,
  measured on a sample of its own vectors and again as it grows: the same
  3 % of the clusters found 0.94 on MIRACL and 0.90 on Amazon's products
  and StackExchange (search-hybrid), so a fixed number would promise
  nothing. So is how many of the best by RaBitQ's estimate it scores
  again, from 64 up to 2,048: vectors much alike in a crowd need more,
  since a bit a dimension cannot tell their cosines apart. When even every
  cluster cannot reach `recall`, searches stay exact, and the log says so.
- The clusters are built once the index holds 2,048 vectors, in the
  background; searches stay exact until they serve. They live in
  `search.db` and serve again after a restart, and a build stopped by one
  goes on. A vector set while they serve is found at once, from the head,
  which every search reads beside its clusters. `recall` is above 0 and
  below 1, the exact search being the default, and is an average over
  questions drawn from the index's own vectors, not a floor for each
  search; every score a search answers is still its vector's exact cosine.
- Built in slice 5, over 100,000 vectors of 256 dimensions drawn about 500
  centres, `recall` 0.95, on a Ryzen 7 7700 under Windows 11: built in 5.5
  s, one cluster of 316 read and 256 members scored again a search, 1.57
  ms at the median against the exact search's 16, every one of the exact
  ten found (`a_search_from_clusters_over_a_hundred_thousand_takes` in
  `search::clusters`, by hand). Vectors drawn about centres are easy for
  clusters; search-vectors' real embeddings are the measure above.
- Words are always exact. In a search by both, `recall` speaks for the
  vector half alone.

## Filters

```ts
const products = store.index<Product>('products', { fields, filters: ['category', 'ownerId', 'price', 'tags'] })

await products.search({ text: 'red shoes', where: { ownerId: 42 } })
await products.search({ text: 'red shoes', where: { category: ['shoes', 'boots'], price: { gte: 10, lt: 100 } } })
await products.search({ vector: q, where: search.or({ category: 'boots' }, { tags: 'sale' }) })
await products.search({ text: 'red shoes', where: { category: null } })      // documents with no category
```

```rust
use tinystore::search::{eq, is_in, missing, or, range};

let found = products
    .search("red shoes")
    .filter(eq("owner_id", &42))
    .filter(is_in("category", &["shoes", "boots"]))
    .filter(range("price", 10..100))
    .run()?;
let found = products.search_vector(&q).filter(or([eq("category", "boots"), eq("tags", "sale")])).run()?;
let found = products.search("red shoes").filter(missing("category")).run()?;
```

- A filter is written as sqldb's conditions are, the owner's call: in
  TypeScript the object `where` takes, equal values with a list for one of
  them and `null` for none, grouped by `search.or` and `search.and` as
  sqldb's by `sql.or`; in Rust the pieces `.filter` takes, `eq`,
  `is_in`, `or` and `and` as `sql::eq` and the rest, and `range` for
  numbers between two bounds, any Rust range. Each `.filter` adds a
  condition with and. A piece of SQL is no filter: a search's filter runs
  over its postings, not a table.
- `filters` names the fields a search may filter by, each a path into the
  document whose value is text, a number, a boolean, or a list of them: a
  product in two categories passes a filter on either. A filter on a field
  the index was not opened with is `invalid`, since the index keeps only
  what it was told to.
- A value must match exactly, its case and its type: the text "50" never
  passes for the number 50, and 42.0 is 42; `eq` is exact for whole
  numbers past 2⁵³ too, so an id of 64 bits filters as it is. A field
  that holds a list passes `eq`, `is_in` or `range` on any of its values.
- A document has no value where it has no such field, null or an empty
  list, and `missing(path)` asks for it, as SQL's `is null`; in Rust `eq`
  refuses `None`, so that an id a session lacks never asks for documents
  without one. In TypeScript `null` in `where` asks for none, as sqldb's
  does, and `undefined` is `invalid`.
- A range takes any of Rust's ranges, `10..100` holding 10 and not 100, and
  compares numbers as f64, so whole numbers past 2⁵³ compare by the
  nearest one; a document with no number there never passes.
- A search keeps to the documents its filter passes and answers the best
  ten of those, never the best ten of all with the others taken out after,
  which could leave a user nothing where their own documents were there to
  find.
- Searches by words and by an exact vector answer what scoring every
  document that passes would; with `recall`, the vector half finds that
  share of the exact ten among them.
- A tenant is a filter: `where: { tenantId }` keeps each tenant to their
  own documents, ranked by the words of the whole index.
- A filter on a field the index was not opened with names the nearest it
  was, `did you mean price?`, through `tinystore::typos`.
- `not`, ranges of text and counts of hits by value are not in the first
  version.

## An index of a table

```ts
const db = await store.database('app', { migrations: './migrations' })
const articles = db.searchIndex<Article>('articles_search', {
  table: 'articles',
  fields: { title: 2, body: 1 },
  filters: ['category', 'done'],
})
await articles.ready()                                           // the rows the table held, read once

await db.exec`update articles set title = ${'Новый заголовок'} where id = ${42}`
const { hits } = await articles.search('новый заголовок')       // sees the update; hit.value is the row
await articles.search({ text: 'bread', where: { done: true } })  // the 1 SQLite keeps

const products = db.searchIndex<Product>('products_search', {
  table: 'products',
  fields: { name: 3, description: 1 },
  vectors: { column: 'embedding', dimensions: 1024 },
})
await db.dropSearchIndex('products_search')
```

```rust
let articles = db
    .search_index::<Article>("articles_search")
    .table("articles")
    .field("title", 2.0)
    .field("body", 1.0)
    .filters(["category", "done"])
    .open()?;
articles.ready()?;
let found = articles.search("новый заголовок").filter(search::eq("done", &true)).run()?;
db.tx(|tx| {
    tx.exec(sql!("insert into articles (id, title) values (?, ?)", 7, "inside"))?;
    let found = tx.with(&articles).search("inside").run()?;     // sees the insert
    Ok(())
})?;
let products = db.search_index::<Product>("products_search").table("products").key("sku")
    .field("name", 3.0).vectors("embedding", 1024).open()?;
db.drop_search_index("products_search")?;
```

- `db.searchIndex` follows a table of the database: the table owns the
  rows, and the index has no `set` or `delete`. Its key is the table's
  primary key unless `key` names another column of unique values, by a
  primary key or a unique index, integers or text; a hit's id is the key as
  text. `fields` and `filters` name columns, never paths into one.
- Every way a row changes reaches it: `exec`, `batch`, a transaction, a
  trigger of the program's own, a migration. A change and the index's change
  commit together or not at all: the index's triggers on the table write
  each changed key into a list of pending keys in the same transaction, and
  every transaction of the file reads those keys' rows into the index
  before it commits, a grouped write inside its own savepoint. A rollback, a
  failed savepoint or a crash leaves neither, and a search in the
  transaction that wrote, `tx.with(&index)`, sees its writes.
- A write from outside the store, a process opened beside it or the sqlite3
  shell, fires the same triggers, which only write a key, so its keys wait in
  the list; the store reads them at its next write to that file, or within
  a second. The index may lag such a write; it never loses one. A row of
  theirs the index cannot read is said in the log and left out of it, since
  no caller of the store's made it.
- The triggers name only the key, so every change of a row lists it and a
  migration may drop any other column. The index reads the row as it is
  and sets it again only when what it reads of it changed, which a digest
  of those columns and the vector says: an update of a column it does not
  read costs a read. A column a migration dropped reads as empty until the
  index is opened without it.
- A migration that rebuilds the table by SQLite's twelve steps drops its
  triggers with the old table. The index's next open puts them back and,
  finding them gone, reads the table again: its rows set where they
  changed, then its documents whose rows are gone deleted. A program that
  rebuilds a table at run time opens its index again after.
- The first open reads the rows the table holds in the background, a step
  of 256 keys a commit in the order of the keys; the triggers keep the rows
  behind the step, and each step reads its rows as they are. `status()` is
  `rebuilding` meanwhile and `ready()` waits for it.
- Vectors come from a column the program fills, a blob of little-endian
  floats or null for none, never from a model the store runs. A write whose
  vector has another length, NaN or an infinity fails with its row. A
  changed text does not change its row's vector: keeping the two in step
  is the program's.
- `hit.value` is the row, read whole in the same snapshot as the index, as
  `db.all` reads one: an integer column the program keeps a boolean in
  comes back the number SQLite keeps. A filter's `true` and `false` pass
  that 1 and 0.
- Over the wire a table's index opens with its database's handle, and a
  hit carries its row's cells as `sql.Rows` does (docs/wire.md).
- On a database `index` would read as SQL's `create index`, so it is spelled
  out there; `store.index` stays short where nothing else is an index. An
  index in `search.db` owns its documents and joins no store's transaction,
  since no write is atomic across two files.
- Measured on nothing yet: what a row costs a commit, and a table's first
  read. A round with the owner's word.

## Errors

| Kind      | When                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
|-----------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `invalid` | a bad name or id; vectors of other dimensions than the index holds, or none where it keeps them; a change that needs a rebuild under `rebuild: 'manual'`, until `reindex`; a handle of an index dropped; a field's weight not above zero or past 1,000; a vector of another length, NaN, an infinity or zeros alone, missing, or on an index without `vectors`; a filter on a field not in `filters`, a value of no type a filter keeps, `eq` given a list, or a range of other than numbers; a `vectorWeight` outside 0–1, or beside `rerank`; a `recall` not above 0 and below 1, or without `vectors`; either in a search without a vector, or `rerank` in one without text; for a table's index, a table, key or column the database lacks, a key of values not unique, another table or key than it follows, and a row whose vector does not read, which fails its write |
| `limit`   | a document, a field's text, a filter's text, a search, its filter or a `setMany` over its bound                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `corrupt` | a block, a list or a document that no longer reads, named                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `closed`  | the store closed                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |

Every error names the index and the document: `index articles: id "a-17": invalid`.

## Bounds

Proposed, to be measured before they are promised.

| What               | Bound                     |
|--------------------|---------------------------|
| a document         | 1 MiB                     |
| a search's text    | 1 KiB, its first 32 words |
| a vector           | 4,096 dimensions          |
| `limit` + `offset` | 1,000                     |
| an index's name    | 64 bytes                  |
| a field's weight   | above 0, at most 1,000    |
| a filter's value   | 256 bytes                 |
| a search's filter  | 8 conditions, 100 values  |
| a `setMany`        | 1,000 documents, 16 MiB   |

## Under the surface

- **Words.** Text is NFKC-normalized and lowercased, Latin accents and
  Russian stress marks dropped; words are cut by Unicode's rules, Chinese,
  Japanese and Thai first cut out by script and read by pairs of letters;
  each word is kept as written and, for languages Snowball stems, as its
  stem. search-plan measured stems +7–12 % for Russian and a loss for
  English, so a stem is kept only where it helps.
- **Postings** are blocks of 128 documents and frequencies in SQLite rows,
  each block with the bound of what its documents can score; a field's
  lengths are kept apart, a row a 65,536 documents; a word in no more than
  16 documents keeps its postings in its own row. A search reads blocks by
  windows of 4,096 documents, eight blocks a statement, and skips every
  block whose bound cannot reach the tenth best. A posting's score is kept
  in whole points of 2⁻³², which add exactly in any order, so packing,
  which changes the order a search adds a document's postings in, changes
  no score. search-compact: `or` at
  1.5–2.5 times Tantivy's p99 in one session, 22–90 times faster than FTS5,
  a reader of 71 MiB against Tantivy's 309–323, a set in 3.5 ms, every
  answer equal to scoring every posting.
- **Writes** go to a head that searches read beside the blocks; a set gives
  its document a new number and marks the old one gone. Background work
  packs the head into the blocks every thousand writes, which kept `or`'s
  p99 at 14–16 ms through 15,000 writes where an unpacked head reached 37
  (search-compact). It packs 2,048 of the head's rows a commit, holding the
  writer 92 ms at most where one commit held it a second, at 3.4 times the
  work in all; once the gone pass 0.5 % of the documents it purges them,
  rewriting only the blocks whose range holds one, in commits as short
  (search-wchurn). Every answer stayed exact throughout.
- **Numbers** are given by every set, so they grow with the sets an index
  has had, not the documents it holds, and a part's lengths are kept a row
  a 65,536 numbers: a slice-1 mix that set 400 documents 20 times each left
  a file 1.7 times a fresh build's. Slice 6's rebuild in the background
  numbers the documents again from 1, the program's ids kept, and starts by
  itself once the numbers given pass twice the documents held and 4,096,
  and in any case past 2³¹, long before 2³², the most a number can be.
- **Filters** are keys of the postings beside a field's words, a value's
  term by its type, `t:shoes`, `i:42`, `b:true`, and `n:` for a document
  with none, a frequency of 1 each, packed and purged with the words, each
  knowing its count of documents. A number is also kept at eight
  precisions, its place among all numbers with its last 8, 16, … 56 bits
  dropped, as Lucene's numeric fields were, so that a range is a few
  hundred keys at most. A search reads every document of the keys its
  conditions name and joins them, `or` by their union and `and` by their
  intersection. A filter that passes few documents has them scored alone,
  each sought in every list, or each vector's code read by its number; one
  that passes many runs the usual search, crossing out of each window the
  documents it does not pass and passing a window that holds none, or
  scans every code crossing them out. Over 100,000 articles with vectors
  of 256 dimensions, on a Ryzen 7 7700 under Windows 11, medians in ms
  (`where_a_filter_is_scored_alone_takes` in `search::index`, by hand):

  | passes | words alone | words crossed | vector alone | vector crossed |
  |--------|-------------|---------------|--------------|----------------|
  | 1/2    | 7.62        | 1.70          | 28.15        | 25.11          |
  | 1/8    | 2.36        | 1.28          | 16.54        | 23.66          |
  | 1/32   | 1.15        | 1.30          | 6.54         | 22.89          |
  | 1/128  | 0.73        | 1.34          | 2.82         | 22.86          |
  | 1/512  | 0.64        | 1.21          | 0.85         | 22.08          |
  | 1/2048 | 0.31        | 0.88          | 0.08         | 22.91          |
  | 1/8192 | 0.15        | 0.37          | 0.03         | 21.46          |

  So a search by words scores a filter's documents alone from 1 in 32 of
  the index's, and one by a vector from 1 in 4. Clusters will skip what
  does not pass and read further clusters until enough pass, or go exact
  over a filter's few (slice 5); graphs such as HNSW are what filters are
  hard for, and clusters are not one.
- **Two halves at once** are asked one after the other and summed as the
  search section says; search-hybrid's fusion took 7.4 and 14.8 ms at the
  median with clusters, the words' share of it 2.2 and 11.8, and `rerank`
  2.9 and 12.4.
- **Typos** come from `tinystore::typos`: one FST of the index's words a
  script, through a Levenshtein automaton; search-plan measured a mixed
  question's candidates at a median 18.6 ms from one FST for every script,
  2.6 from one a script. The first search that needs them builds them;
  once the index's words change, a search goes on with the ones kept,
  reading no word its own snapshot no longer holds, and background work
  builds them again within a minute.
- **Vectors** are kept as given, a row each, 4.5 KiB of pages for 1,024
  dimensions. An exact search reads a byte a dimension of every vector's
  direction, kept with the length of what the bytes leave out, and puts the
  question's direction in levels to ±4,095, whose product with a code is
  exact in an i32: a code bounds its vector's cosine rather than estimating
  it, and only a vector whose bound reaches the tenth best is scored again,
  on its floats as given. Of 5,000 random vectors of 256 dimensions, 13 a
  search were (`codes_rule_out_most_vectors`).
  `recall` adds clusters by spherical k-means from k-means++'s seeds over
  a sample of the byte codes, 32 a cluster, √n clusters for n vectors, so
  that a cluster holds about as many members as there are clusters; each
  member kept as RaBitQ's bit a dimension of its offset from the centroid,
  turned by a fixed rotation, 136 bytes a vector of 1,024 dimensions: a bit
  a dimension reached the same recall as a byte three times sooner.
  SQLite's own vec1 answered 1.2–1.5 times sooner at equal recall.
- **Members are rows ordered by their cluster**, so that a search reads a
  cluster as one range and nothing needs packing; search-vchurn packed
  because its head was a table apart, read through an index.
- **Writes into the clusters** go to the head, cluster −1, in the set's own
  commit. Maintenance assigns the head to the cluster nearest each once it
  holds 256, 1,024 a commit, as a build assigns every vector: each step is
  worked out in a snapshot, so that the writer is held only to write it,
  and a member whose vector a write dropped meanwhile is left out. A
  cluster past twice √n is split by spherical 2-means in a commit of its
  own, so that clusters grow in number with the index, as search-vchurn's
  splits held them under drift. Through 130,000 writes there, a language
  the clusters were never trained on among them, recall held at 0.942 and
  every exact answer stayed exact (search-vchurn).

## Slices

1. The analyzer, words and postings in the Rust core: `set`, `setMany`,
   `delete`, `get`, `search` by words, the head packed in steps, the gone
   purged, and `corrected`: built 16 October, a field added to an index that
   holds documents refused until slice 6. Its tables are named
   `_tinystore_search_*`, as kv's are, so that slice 8 keeps the same ones
   in a database's file beside the program's.
2. `tinystore::typos`, which the command line and config use too: built
   16 October, `40672f2`.
3. Vectors, exact; a search by both, its `vectorWeight` and `rerank`:
   built 16 October, a vector's code and the vector as given a row each.
4. Filters, for words and exact vectors, with the round that finds where
   a filter's own documents are scored apart: built 16 October, with `or`,
   `and` and ranges of numbers, in the shape of sqldb's conditions (the
   owner's calls below).
5. Vectors with `recall`: the clusters, their heads, packing and splits,
   the clusters read chosen from `recall`, and filters through them: built
   16 October, members ordered by their cluster in place of packing, and
   the members scored again chosen with the clusters read.
6. Changes of an index in the background, `ready()`, `status()`,
   `dropIndex` and `tinystore search`, and the documents numbered again by
   a rebuild that starts by itself: built 16 October, a set reaching the
   new rows through their copy, in the order of its number.
7. The wire and the Bun SDK: built 16 October, the engine's methods
   `0x0b01` to `0x0b09`, documents as JSON and vectors as their floats'
   bytes, a `where` as a JSON list of conditions (docs/wire.md), a read
   connection opening an index only as it is; `store.index`, `reindex`,
   `dropIndex` and `indexes`, and `search.or` and `search.and`, as sqldb's
   `sql.or`.
8. `db.searchIndex`: its triggers, the pending keys, and their indexing
   before the commit: built 16 October, every transaction of a file
   settling what an engine kept in it left, a digest of what the index
   reads of a row, the table read the first time and again after a
   migration rebuilt it, and the wire and the Bun SDK; a search in the
   transaction that wrote in Rust alone.

Python and Go come with their bindings. Each slice ends with its gates and
a newcomer check of what it made public.

## Gates

- A search by words answers what scoring every document answers, under
  sets, deletes, packing and purging.
- An exact vector search answers what scoring every vector answers, bit for
  bit.
- A filtered search answers what scoring every document the filter passes
  answers, and finds a document that passes whenever one holds a word of
  the search.
- A search by both orders its hits by the sum the search section gives,
  computed again from both halves' scores.
- A set is seen by the next search; a deleted document never answers again.
- A packing or a purge changes no answer.
- Searches and writes from many threads at once answer what one at a time
  would, in some order of the writes.
- A crash at any moment of a write, a packing, a purge or a rebuild leaves
  the index as its last commit, and the next open answers as it did.
- Through a long mix of sets and deletes, the file stays within a bound of
  a fresh build of the same documents and the write-ahead log within its
  checkpoint's.
- A search-only build links no jobs; a build without `typos` links no fst.
- A changed weight or `vectorWeight` rebuilds nothing, and the next search
  reads it.
- A change that needs background work answers by the old settings until
  its switch, every write meanwhile reaching both, and a restart continues
  it; `rebuild: 'manual'` refuses it until `reindex`.
- A write to a table that rolls back changes neither the table nor its
  index; one from outside the store reaches the index at the store's next
  write or within a second, and a search always answers what indexing the
  table's rows afresh would.

## Later

- `not` in a filter, which needs every document the index holds; ranges
  of text; how many hits each value of a filter holds (facets), each
  leaving out its own field's condition, as Algolia's do.
- A search with a filter and neither words nor a vector, listing what it
  passes in an order a field gives, and `total` for a pager.
- Tenants ranked by their own words, `under(orgId)`: whether a branch
  counts its words apart needs a round with thousands of small tenants
  first. A tenant as a filter is in the first version.
- `total` for a pager, which an exact top ten that skips blocks does not
  count.
- Phrases, from positions kept beside the blocks (+283 MiB a million
  documents, search-compact); highlighted matches.
- A vector's model named at open, so that one swapped for another of the
  same dimensions is refused.
- A model in the binary, so that a program without one gets vectors.
- Each half's own score on a hit of a search by both, and a least cosine
  a search by a vector keeps to, as Qdrant's `score_threshold` (slice 3's
  check).
- How a search by a vector found its hits, exactly or from how many
  clusters, on its answer, and the recall measured on `status()` (slice 5's
  check).
- A rebuild's cancel and dry run, `ready()` with a timeout, and the target
  settings and why on `status()` (slice 6's check).
- The paths of `fields`, `filters` and `where` checked against the
  document's type in TypeScript (slice 7's check).
- A table's index searched inside a transaction over the wire and in the
  Bun SDK, `tx.with(index)`, as Rust's is.
- The columns a hit's row reads, leaving out a vector's blob; an index of
  a view or of several tables (slice 8's check).
- A table renamed by a migration: SQLite renames its triggers' table, and
  the index's settings still name the old one.
- A database's tables' indexes in `search::list`, `store.indexes()` and
  `tinystore search list`, which list search.db's.
- How many documents a search by a vector passed for having none.
- Questions that are an identifier alone, a part number or an error code,
  which no round sampled apart.
- `patch`, a document's fields set apart, once a program needs one and its
  meaning for nested values, lists, `null` and vectors is decided.

## Names, 16 October

The owner and an outside reviewer read the first draft.

| It read                                                   | Change                                                              |
|-----------------------------------------------------------|---------------------------------------------------------------------|
| `store.search('articles')` as a search for "articles"     | `store.index('articles')`, `.search()`; the engine stays `search`   |
| `fields: { title: 2, body: 1 }`, numbers whose role hides | `fields: ['title', 'body'], boost: { title: 2 }`, for a blind check |
| `exact` on an answer: exact what, and which half          | `recall` chosen at open; the answer says nothing                    |
| `under(orgId)` promising each tenant its own ranking      | later, after a round with thousands of tenants                      |
| `query` beside SQL's queries                              | `search`; `query` stays SQL's                                       |

## Newcomer check and review, 16 October

Three Haiku agents that had read nothing about TinyStore read the second
draft at the highest effort: a newcomer's reading of fifteen bare calls as
one program, a free review by a developer who knows Meilisearch and Algolia,
and a blind choice between whole sequences of use.

| It read                                                         | Change                                                           |
|-----------------------------------------------------------------|------------------------------------------------------------------|
| `suggest` as autocomplete, all three                            | gone; `corrected` on the answer                                  |
| `typos: false` as "do not keep typos"                           | `typoTolerance: false`, Meilisearch's word                       |
| `search('red shoes', { vector: q })`: the vector as a mere hint | `search({ text, vector })`; a string is a search by words        |
| a set without `vector` silently dropping a document's vector    | `vector` required on an index with vectors, `null` to leave none |
| a fusion of two hundreds that cannot page past them             | each half asked for at least `offset + limit`                    |
| a product that ranks long vectors first                         | the cosine, from lengths kept beside the vectors                 |
| whether a search needs every word                               | any word, said where `search` is                                 |
| `boost` naming a field twice                                    | the weight beside its field, the owner's call below              |

Kept against a reviewer: `hit.value` (the blind check chose it over
`document`, and a bucket's value is `value`), `tx.with` (every engine's
word), and `exact` on the answer, which two asked for and the outside
reviewer found ambiguous.

## Slice 1's check, 16 October

Three Haiku agents that had read nothing about TinyStore read the Rust calls
slice 1 made public: a newcomer's reading of thirteen bare lines, a free
review by a developer who knows Meilisearch, Algolia and Tantivy, and a blind
choice between three shapes of five calls.

| It read                                                                                  | Change                                                                                        |
|------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| `.run()`, `found.hits`, `delete` answering a `bool`, `typo_tolerance`, `index::<String>` | none: the blind choice took each over its two others                                          |
| a second `open` with other settings as a handle of its own, both readers                 | none: an open changes the index, the owner's call; `open`'s comment says so                   |
| whether a search needs every word                                                        | none: any word, said in `search`'s comment                                                    |
| pages of equal scores repeating or skipping a hit                                        | the document set first comes first, said in `search`'s comment                                |
| `total`, highlights, `set_many`, `patch`                                                 | `total` and highlights are in Later; `set_many` built, `patch` later, the owner's calls below |

## Slice 3's check, 16 October

Three Haiku agents that had read nothing about TinyStore read the Rust calls
slice 3 made public, at the highest effort: a newcomer's reading of thirteen
bare lines, a free review by a developer who knows Meilisearch, Algolia,
Qdrant, LanceDB and Tantivy, and a blind choice between three shapes of four
calls.

| It read                                                                             | Change                                                                                          |
|-------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|
| `id(…).vector(…).set(…)`, `.without_vector()`, `.vector(&q).rerank()`               | none: the blind choice took each over its two others                                            |
| `search("").vector(&q)` as a hidden flag, two of three                              | `search_vector(&q)`; an empty text with a vector still searches by the vector                   |
| a weight of 0 that still answers what the vector alone found, at 0                  | a weight of 0 is the search by words alone, of 1 by the vector alone, each with its own scores  |
| `set_many` mixing `&va[..]` and `None` in one list, which does not compile          | the list where some have none says `Some(&va[..])`, as `set_many`'s comment and a test do       |
| pages past the first hundred of a search by both ordered otherwise                  | said in `vector`'s comment and above                                                            |
| `set(id, &doc)` on an index with vectors failing only when it runs                  | none: an index's settings change as it opens, so its handle's type cannot know them             |
| `.vectors(1024)` read as a count of vectors by one; `Combine` for weight and rerank | left to the owner: the newcomer read `vectors` right, and `hybrid` is the book's accepted shape |
| each half's score on a hit, a least cosine, documents without a vector counted      | Later                                                                                           |

## Slice 4's check, 16 October

Three Haiku agents that had read nothing about TinyStore read slice 4's
filters at the highest effort: a newcomer's reading of twelve bare lines, a
free review by a developer who knows Meilisearch, Algolia, Qdrant,
Elasticsearch, LanceDB, Prisma, Drizzle and Diesel, and a blind choice
between three shapes of four calls.

| It read                                                                                              | Change                                                                                 |
|------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| `search::eq` with `.filter`, `range` with a Rust range, `or([…])`                                    | none: the blind choice took each over its two others, for being sqldb's                |
| `eq(path, &None)`, two of three, and an `Option` id whose `None` would ask for documents without one | `missing(path)`; `eq` and `is_in` refuse `None`                                        |
| what has no value: no field, null, an empty list, an empty text                                      | no field, null and an empty list, said in `missing`'s comment and above; "" is a value |
| a field misspelt, found only at `run`                                                                | the error names the nearest filter the index has                                       |
| the case of a value, a document without a number in a range, a list field                            | said in `eq`'s and `range`'s comments and above                                        |
| typed field references from a derive, `.filterable` for `.filters`                                   | `.filters` kept by the owner, 17 October; sqldb names columns by text too              |
| `&42` beside `"sale"`                                                                                | none: `sql::eq` takes its value so, and a `&str` is already one                        |
| a listing by a filter alone, its order and `total`; facets; `not`                                    | Later                                                                                  |

## Slice 5's check, 16 October

A Haiku agent that had read nothing about TinyStore, told it had used
Qdrant, LanceDB, pgvector and FAISS, read `recall` at the highest effort.

| It read                                                                      | Change                                                                                  |
|------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------|
| `recall(0.95)` as a promise for each search                                  | said: an average over questions drawn from the index's own vectors, not a floor         |
| whether the clusters survive a restart, and when a set is found              | said: they live in `search.db`; a set is found at once, from the head                   |
| the answer silent on whether clusters or the exact search found it           | Later: how a search found its hits on its answer, and the recall measured on `status()` |
| `recall_target` for `recall`, the measured value keeping the name            | left to the owner: `recall` is the book's accepted word                                 |
| a ceiling on a filtered search that reads further clusters until enough pass | none: it reads at most every cluster, which the exact search over a filter's few bounds |

## Slice 6's check, 16 October

A Haiku agent that had read nothing about TinyStore, told it had used
Meilisearch, Elasticsearch's reindex and aliases, Typesense and SQL
migrations, read the calls slice 6 made public at the highest effort.

| It read                                                                                     | Change                                                                                                      |
|---------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------|
| an open that changes the schema: a stale build that lists fewer fields removes one          | left to the owner: open changes the index, the owner's call above; the reviewer would split `migrate()` off |
| a rebuild that fails seen only by `status()`                                                | none: it is said in the log as it fails, and `ready()` is the error                                         |
| renumbering as a rebuild nobody asked for, and `Rebuild::Manual` still renumbering near 2³¹ | said in "Change an index": past 2³¹ a manual index is numbered again regardless                             |
| a cancel, a dry run, `ready()` with a timeout, the target settings and why on `status()`    | Later                                                                                                       |
| a typo in a name opening a new, empty index                                                 | none: as a bucket's name does                                                                               |

## Slice 7's check, 16 October

A Haiku agent that had read nothing about TinyStore, told it had used
Meilisearch's and Typesense's JavaScript clients, Prisma and Drizzle, read
the Bun SDK's search at the highest effort.

| It read                                                                               | Change                                                                                                    |
|---------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| `undefined` in `where` an error, where Prisma and Drizzle leave the condition out     | kept, since a silent `ownerId` would hand a user another's documents; the error says to leave the key out |
| `tags: 'sale'` on a field that holds a list: contains or equals                       | none: "Filters" says a list passes on any of its values                                                   |
| `search.or` read as a function beside `products.search`                               | left to the owner: `or` and `and` exported by name would change sqldb's `sql.or` too                      |
| the paths of `fields`, `filters` and `where` unchecked against `Product`              | Later                                                                                                     |
| what a handle opened before `reindex` reads after it, and a search while `rebuilding` | none: "Change an index" says the old settings answer until the switch, which every handle follows         |

## Slice 8's check, 16 October

A Haiku agent that had read nothing about TinyStore, told it had used
Postgres' full-text search, Meilisearch's and Typesense's syncing from a
database, Prisma, Drizzle and SQLite's FTS5 with triggers, read
`db.searchIndex` at the highest effort.

| It read                                                                                                                                                   | Change                                                                           |
|-----------------------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|
| a filter on a column a rebuild adds, answered by the old settings, passing every row                                                                      | none: it is `invalid` until the switch, as any filter on a field the index lacks |
| a write beside the store reaching the index within a second as a journal would; a trigger calling a function only the store has failing the sqlite3 shell | said: the triggers only write a key into a table, which any process can          |
| a migration that drops a column the index reads failing when it runs before the index opens without it                                                    | built: the triggers name only the key, and a dropped column reads as empty       |
| a table rebuilt by SQLite's twelve steps losing its triggers                                                                                              | built: the next open puts them back and reads the table again                    |
| `open()` in Rust answering a partial first read                                                                                                           | said: `ready()` waits for it, in Rust as in TypeScript                           |
| `hit.value` the whole row, a vector column's 4 KiB a hit with it                                                                                          | Later: the columns a hit's row reads                                             |
| `done: boolean` in TypeScript reading the 1 SQLite keeps                                                                                                  | said: a row as `db.all` reads it; a filter's `true` and `false` pass 1 and 0     |
| one table an index, where Meilisearch's documents are any JSON                                                                                            | Later: an index of a view or of several tables                                   |
| `filters` beside `.filter`, one letter apart                                                                                                              | kept by the owner, 17 October: `filterable` reads badly                          |
| what a row costs a commit, and a first read's time and size                                                                                               | Later: a round, with the owner's word                                            |

## The owner's calls, 16 October

The owner and the outside reviewer read the check, then search-hybrid, and
accepted the book.

| Question                                  | Decided                                                                                                                                                                                       |
|-------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `boost` beside `fields`, or the weight in | `fields: { title: 2, body: 1 }`, a list for equal weights; `boost` gone                                                                                                                       |
| `corrected` or `didYouMean`               | `corrected`: the hits already come from the words it names, which `didYouMean` would deny                                                                                                     |
| the corrector inside search               | `tinystore::typos`, a module and a feature of its own, for the command line and config too                                                                                                    |
| a correction of a config key              | said in the error, never applied                                                                                                                                                              |
| opening again with other settings         | it changes the index: weights at once, other changes in the background beside the old one, said in the log; `rebuild: 'manual'` refuses them until `store.reindex`; `ready()` and `status()`  |
| an index no code opens any more           | kept until `store.dropIndex` or `tinystore search drop`, since another version may open it                                                                                                    |
| `db.index`                                | `db.searchIndex`; `store.index` stays                                                                                                                                                         |
| how words and vectors fuse                | their scores, each scaled to its own hundred, summed at `hybrid: { vectorWeight }`, 0.5 unless the index or a search says otherwise; `rerank: true` for the words' hits ordered by the vector |
| the weight at open or in a search         | at open as the default, and a search may give its own, since a weight changes no stored byte (the outside reviewer's)                                                                         |
| filters                                   | in the first version, equal values and lists of them on fields declared at open, scored while the search runs; ranges later                                                                   |
| what an implementation must show          | searches and writes at once, a crash at any moment, and a file and a log that stay bounded through a long mix (the outside reviewer's)                                                        |
| what the corrector answers                | the word and its edits, each caller its own rules                                                                                                                                             |
| `db.searchIndex`'s shape                  | it follows a table, which owns the rows; no `set`; one commit with every change                                                                                                               |
| vectors in a table's index                | a column the program fills; the store never runs a model in a write                                                                                                                           |
| Russian's stems                           | rust-stemmers kept: 11.5 KiB of a binary, measured, against a stemmer of our own to keep (the outside reviewer's)                                                                             |
| numbers that grow with every set          | numbered again by slice 6's rebuild, which starts by itself at a threshold that slice measures, long before 2³² (the outside reviewer's)                                                      |
| a list of documents at once               | `setMany`: one commit, all or none, 1,000 documents and 16 MiB a call (the outside reviewer's)                                                                                                |
| `patch`                                   | later: `set` replaces a document whole until a program needs a part set and what it means is decided (the outside reviewer's)                                                                 |
| how a search's filter is written          | as sqldb's conditions: the object `where` takes in TypeScript, and in Rust `.filter` with `eq`, `is_in`, `or` and `and` as `sql::eq` and the rest                                             |
| a filter beyond equal values              | `or`, `and` and ranges of numbers in the first version: they are unions and intersections of the documents' keys, and a number kept at eight precisions makes a range a few hundred keys      |
