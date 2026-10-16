# search: the API book

Accepted by the owner on 16 October 2026, after a newcomer check and the
owner's and an outside reviewer's readings (below). Documents a program sets
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

| Word            | What it is                                                                                      |
|-----------------|-------------------------------------------------------------------------------------------------|
| an index        | a name documents of one type are kept and found under, `store.index('articles')`                |
| a document      | a value set by its id, as a bucket's is; the index keeps it and answers with it                 |
| fields          | which of a document's text its words are read from, and how much each weighs                    |
| a vector        | numbers the program's own model gave a document or a question; never made here                  |
| a filter        | a field of the document a search can keep its hits to, declared at open: `{ userId: 42 }`       |
| a hit           | `{ id, value, score }`: a document found, and what ordered it                                   |
| a table's index | `db.searchIndex`: an SQL table's rows, found as an index's documents, kept in step by the store |

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
search articles: description added: reading 812,345 documents beside the index, which answers without it until then
search articles: description ready in 41 s; switched
```

Opening an index with other settings than it holds changes it, and opening
never waits for the work a change needs.

| What changed                                               | What happens                                                                                         |
|------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| a field's weight, `typoTolerance`, `hybrid`                | at once: a search reads the weights and its blocks' bounds when it runs, so nothing is written again |
| a field or a filter removed                                | at once from searches; its postings dropped in steps in the background                               |
| a field or a filter added                                  | read from the kept documents in the background; searches use it once it is ready                     |
| `recall` added or removed                                  | the clusters built or dropped in the background; searches stay exact meanwhile                       |
| how text is cut, by a release of TinyStore that changes it | the index rebuilt in the background                                                                  |
| the vectors' `dimensions`                                  | `invalid`: the vectors are the program's model's, and the store has nothing to make new ones from    |

- Background work runs beside the index, which answers and takes writes
  meanwhile, every write reaching both, in commits of a few keys as packing
  runs; once the new one has caught up it is switched in one commit and the
  old one dropped (phase 6's gate). A restart continues where it stopped.
- Every change is said in the log when it starts and when it is done.

```ts
const articles = store.index<Article>('articles', { fields, rebuild: 'manual' })
await store.reindex('articles', { fields: { title: 2, body: 1, description: 1 } })

await articles.ready()        // once the index serves the settings it was opened with
articles.status()             // { state: 'rebuilding', done: 0.42 } | { state: 'ready' } | { state: 'failed', error }
```

- `rebuild: 'manual'` is for a program that schedules its own load: a
  change that needs background work is `invalid`, naming it, until
  `store.reindex` asks for it. A change that needs none applies all the same.
- A search never waits for `ready()`: it answers by the settings the index
  serves, which `status()` says.

## Drop an index

```ts
await store.dropIndex('posts')
```

```bash
tinystore search list ./data
```

```bash
tinystore search drop ./data posts
```

- An index the program stopped opening stays: another version of it may
  open the index again, so the store never guesses. `tinystore search
  list` shows each index with its documents, its size and when it was last
  opened.

## Set and delete

```ts
await articles.set('a-17', { title: 'Хлеб на закваске', body: '…' })
await products.set('p-3', product, { vector: embedding })          // number[] or Float32Array
await products.set('p-4', draft, { vector: null })                 // found by its words alone
await articles.delete('a-17')
const article = await articles.get('a-17')                          // the value, or undefined
```

```rust
articles.set("a-17", &article)?;
products.set("p-3", &product).vector(&embedding)?;
articles.delete("a-17")?;
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

## Search

```ts
const { hits } = await articles.search('как испечь хлеб')                        // by words
const { hits } = await products.search({ text: 'red shoes', vector: q })        // by both
const { hits } = await products.search({ vector: q })                           // by the vector alone
const { hits } = await articles.search({ text: 'хлеб', limit: 20, offset: 20 })
const { hits, corrected } = await articles.search('helo wrld')                  // corrected: 'hello world'
const { hits } = await products.search({ text: 'red shoes', filter: { ownerId: 42 } })

for (const hit of hits) show(hit.id, hit.value.title)
```

```python
hits = articles.search("как испечь хлеб").hits
hits = products.search(text="red shoes", vector=q, filter={"owner_id": 42}).hits
```

```rust
let found = articles.search("как испечь хлеб").run()?;
let found = products.search("red shoes").vector(&q).filter("owner_id", 42).limit(20).run()?;
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
  otherwise, from 0, the words alone, to 1, the vector alone.
- The weight is the only setting a search may give in place of its index's:
  one index serves searches that weigh differently, a catalog's search box
  and a support page's, and a weight changes no stored byte, so neither
  needs an index of its own.
- `rerank: true` orders the words' hundred by their vectors' cosine with
  the question's and finds nothing the words did not: no clusters and no
  scan of every vector, a hundred vectors read by their number. It is for a
  program whose questions always have words and whose vectors only order
  them. At open it makes every search by both rerank; beside a
  `vectorWeight` in the same `hybrid` it is `invalid`.
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

- Without `recall`, a search by a vector answers the exact best: the same
  as scoring every vector, scores bit for bit. search-vectors measured 245
  ms a million vectors on two cores, by reading a byte a dimension and
  scoring again on the vectors as given the 25 a question that bytes could
  not rule out; it grows with the index, about 25 ms a hundred thousand.
- With `recall`, the index answers from the clusters nearest the question:
  about 3 ms a million at 0.94 of the exact ten, holding 96 MiB, against
  HNSW's 0.4 ms and 2.2 GiB. The program asked for it at open, so no answer
  says it again.
- How many clusters a search reads is the index's to choose from `recall`,
  measured on a sample of its own vectors and again as it grows: the same
  3 % of the clusters found 0.94 on MIRACL and 0.90 on Amazon's products
  and StackExchange (search-hybrid), so a fixed number would promise
  nothing.
- Words are always exact. In a search by both, `recall` speaks for the
  vector half alone.

## Filters

```ts
const products = store.index<Product>('products', { fields, filters: ['category', 'ownerId', 'inStock'] })

await products.search({ text: 'red shoes', filter: { ownerId: 42 } })
await products.search({ text: 'red shoes', filter: { category: ['shoes', 'boots'], inStock: true } })
await products.search({ vector: q, filter: { category: 'shoes' } })
```

```rust
let found = products
    .search("red shoes")
    .filter("category", ["shoes", "boots"])
    .filter("in_stock", true)
    .run()?;
```

- `filters` names the fields a search may filter by, each a path into the
  document whose value is a string, a whole number, a boolean, or a list of
  them: a product in two categories passes a filter on either. A filter on
  a field the index was not opened with is `invalid`, since the index keeps
  only what it was told to.
- A search keeps to the documents its filter passes: a value must match
  exactly, one of a list given, and every field named must pass. It answers
  the best ten of those documents, never the best ten of all with the
  others taken out after, which could leave a user nothing where their own
  documents were there to find.
- Searches by words and by an exact vector answer what scoring every
  document that passes would; with `recall`, the vector half finds that
  share of the exact ten among them.
- A tenant is a filter: `filter: { tenantId }` keeps each tenant to their
  own documents, ranked by the words of the whole index.
- Ranges of numbers and dates are not in the first version.

## An index of a table

```ts
const articles = db.searchIndex<Article>('articles_search', {
  table: 'articles',
  fields: { title: 2, body: 1 },
})

await db.execute('update articles set title = ? where id = ?', ['Новый заголовок', 42])
const { hits } = await articles.search('новый заголовок')     // sees the update; hit.value is the row

const products = db.searchIndex<Product>('products_search', {
  table: 'products',
  fields: { name: 3, description: 1 },
  vectors: { column: 'embedding', dimensions: 1024 },
})
```

```rust
let articles = db
    .search_index::<Article>("articles_search")
    .table("articles")
    .field("title", 2.0)
    .field("body", 1.0)
    .open()?;
```

- `db.searchIndex` follows a table of the database: the table owns the
  rows, and the index has no `set` or `delete`. Its key is the table's
  primary key unless `key` names another unique column, `fields` names its
  text columns and `filters` the columns a search may filter by.
- Every way a row changes reaches it: `execute`, `batch`, a transaction, a
  trigger of the program's own, a migration. A change and the index's change
  commit together or not at all: the index's triggers on the table write
  each changed key into a list of pending keys in the same transaction, and
  the store's writer indexes them before that commit. A rollback, a failed
  savepoint or a crash leaves neither, and a search in the transaction that
  wrote sees its writes.
- A write from outside the store, a process opened beside it or the sqlite3
  shell, fires the same triggers, so its keys wait in the list; the store
  indexes them at its next write to that file, or within a second. The
  index may lag such a write; it never loses one.
- Vectors come from a column the program fills, a blob of little-endian
  floats, never from a model the store runs. A write whose vector has
  another length, NaN or an infinity fails with its row. A changed text does
  not change its row's vector: keeping the two in step is the program's.
- `hit.value` is the row, read in the same snapshot as the index.
- On a database `index` would read as SQL's `create index`, so it is spelled
  out there; `store.index` stays short where nothing else is an index. An
  index in `search.db` owns its documents and joins no store's transaction,
  since no write is atomic across two files.

## Errors

| Kind      | When                                                                                                                                                                                                                                                                                                                                                                                  |
|-----------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `invalid` | a bad name or id; vectors of other dimensions than the index holds; a change that needs a rebuild under `rebuild: 'manual'`; a field's weight not above zero; a vector of another length, NaN or an infinity, missing, or on an index without `vectors`; a filter on a field not in `filters`, or a value of no type a filter keeps; a `vectorWeight` outside 0–1, or beside `rerank` |
| `limit`   | a document, a field's text or a search over its bound                                                                                                                                                                                                                                                                                                                                 |
| `corrupt` | a block, a list or a document that no longer reads, named                                                                                                                                                                                                                                                                                                                             |
| `closed`  | the store closed                                                                                                                                                                                                                                                                                                                                                                      |

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
| a filter's value   | 256 bytes                 |
| a search's filter  | 8 fields, 100 values      |

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
  block whose bound cannot reach the tenth best. search-compact: `or` at
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
- **Filters** are keys of the postings without frequencies, `category` and
  `shoes` beside a field's words, packed and purged with them, and the
  index knows each one's count of documents. A filter that passes few
  documents is scored alone: its documents and nothing else. One that
  passes many runs the usual search, crossing out of each window the
  documents it does not pass, whose blocks' bounds then skip less. Exact
  vectors score only what passes; clusters skip what does not and read
  further clusters until enough pass, or go exact over a filter's few. Where
  each path begins is the filters slice's measurement; graphs such as HNSW
  are what filters are hard for, and clusters are not one.
- **Two halves at once** are asked one after the other and summed as the
  search section says; search-hybrid's fusion took 7.4 and 14.8 ms at the
  median with clusters, the words' share of it 2.2 and 11.8, and `rerank`
  2.9 and 12.4.
- **Typos** come from `tinystore::typos`: one FST of the index's words a
  script, through a Levenshtein automaton; search-plan measured a mixed
  question's candidates at a median 18.6 ms from one FST for every script,
  2.6 from one a script.
- **Vectors** are kept as given, a row each, 4.5 KiB of pages for 1,024
  dimensions. An exact search reads a byte a dimension of every vector, kept
  with the length of what the byte leaves out, which bounds each score.
  `recall` adds clusters by spherical k-means, each member kept as RaBitQ's
  bit a dimension, 146 bytes a vector: a bit a dimension reached the same
  recall as a byte three times sooner. SQLite's own vec1 answered 1.2–1.5
  times sooner at equal recall.
- **Writes into the clusters** go to the nearest cluster's head, a commit
  each, 3.1–6.1 ms at the median; a cluster is packed in a commit of its
  own and split past twice the mean. Through 130,000 writes, a language the
  clusters were never trained on among them, that kept a search at 4.5 ms
  at the median where an unpacked one reached 14.2, recall at 0.942, and
  every exact answer exact (search-vchurn).

## Slices

1. The analyzer, words and postings in the Rust core: `set`, `delete`,
   `get`, `search` by words, the head packed in steps, the gone purged,
   and `corrected`.
2. `tinystore::typos`, which the command line and config use too: built
   16 October, `40672f2`.
3. Vectors, exact; a search by both, its `vectorWeight` and `rerank`.
4. Filters, for words and exact vectors, with the round that finds where
   a filter's own documents are scored apart.
5. Vectors with `recall`: the clusters, their heads, packing and splits,
   the clusters read chosen from `recall`, and filters through them.
6. Changes of an index in the background, `ready()`, `status()`,
   `dropIndex` and `tinystore search`.
7. The wire and the Bun SDK.
8. `db.searchIndex`: its triggers, the pending keys, and their indexing
   before the commit.

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

- Ranges of numbers and dates in a filter, `filter: { price: { below: 100
  } }`, from a sorted key of their own; `not`.
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
- Questions that are an identifier alone, a part number or an error code,
  which no round sampled apart.

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
