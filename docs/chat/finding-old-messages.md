# Finding old messages

A user remembers "something about Lisbon" from a month ago, or asks for "that
bread recipe" in words the message never used. This chapter adds a search
box: by words in any language, forgiving typos, and by meaning through the
vectors your embedding model makes. It uses the Search engine, with an index
that follows the messages table, so you never copy rows into a search
service.

## Index the messages table

The program fills a column with each message's vector, as the embedding model
returns it. A third migration adds that column:

```sql title="migrations/0003_embeddings.sql"
alter table messages add column embedding blob;
```

Then the chat opens an index of the table:

```ts
interface Message {
	id: number
	chat_id: number
	role: string
	text: string
}

const db = await store.database('chat', { migrations: `${import.meta.dirname}/migrations` })
const found = db.searchIndex<Message>('messages_search', {
	table: 'messages',
	fields: ['text'],
	filters: ['chat_id'],
	vectors: { column: 'embedding', dimensions: 1024 },
})
await found.ready() // the rows the table already holds, read once
```

```python
@dataclass
class Message:
    id: int
    chat_id: int
    role: str
    text: str


db = await store.database("chat", migrations=Path(__file__).parent / "migrations")
found = db.search_index(
    "messages_search", Message,
    table="messages",
    fields=["text"],
    filters=["chat_id"],
    vectors=tinystore.Vectors(column="embedding", dimensions=1024),
)
await found.ready()  # the rows the table already holds, read once
```

```go
type Message struct {
	ID     int64
	ChatID int64
	Role   string
	Text   string
}

db, err := sqldb.Open(ctx, store, "chat", sqldb.Migrations(os.DirFS("migrations")))
found, err := search.OpenIndex[Message](db, "messages_search",
	search.Table("messages"),
	search.FieldNames("text"),
	search.Filters("chat_id"),
	search.Vectors{Column: "embedding", Dimensions: 1024})
err = found.Ready(ctx) // the rows the table already holds, read once
```

```rust
#[derive(Deserialize)]
struct Message {
    id: i64,
    chat_id: i64,
    text: String,
}

let db = store.database("chat").migrations("migrations").open()?;
let found = db
    .search_index::<Message>("messages_search")
    .table("messages")
    .fields(["text"])
    .filters(["chat_id"])
    .vectors("embedding", 1024)
    .open()?;
found.ready()?; // the rows the table already holds, read once
```

The index lives inside `sql/chat.db`, next to the table it follows.

| Option    | What it says                                                                 |
|-----------|------------------------------------------------------------------------------|
| `table`   | the table whose rows the index holds; its primary key is a hit's id          |
| `fields`  | the columns whose words are read; `{ text: 2, title: 1 }` gives them weights |
| `filters` | the columns a search may keep its hits to                                    |
| `vectors` | the column of vectors your program fills, and their number of dimensions     |

From now on, every insert, update and delete of a message reaches the index
in the same commit as the row (contract SRCH-07). There is no sync job and
no moment when the table and the index disagree. An edited message is found
by its new words at once, and a deleted one is never found again.

## Search by words

```ts
const { hits } = await found.search({ text: 'lisbon', where: { chat_id: chatIds } })
for (const hit of hits) show(hit.value.chat_id, hit.value.text)
```

```python
result = await found.search(text="lisbon", where={"chat_id": chat_ids})
for hit in result.hits:
    show(hit.value.chat_id, hit.value.text)
```

```go
result, err := found.Search(ctx, "lisbon", search.Where{"chat_id": chatIDs})
for _, hit := range result.Hits {
	show(hit.Value.ChatID, hit.Value.Text)
}
```

```rust
let result = found.search("lisbon").filter(is_in("chat_id", &chat_ids)).run()?;
for hit in &result.hits {
    show(hit.value.chat_id, &hit.value.text);
}
```

A hit has the row's `id`, the row itself as `value`, and a `score` that orders
this search's hits. The search returns the best 10, or `limit`, by BM25: the
same ranking that scoring every message would give, never an estimate
(contract SRCH-01).

`chatIds` are the ids of the user's own chats, so a user only ever finds
their own messages. A list in `where` means "any of these", and a single
value means "this one":

```ts
await found.search({ text: 'lisbon', where: { chat_id: 7 } }) // in this chat only
```

```python
await found.search(text="lisbon", where={"chat_id": 7})  # in this chat only
```

```go
result, err = found.Search(ctx, "lisbon", search.Where{"chat_id": 7}) // in this chat only
```

```rust
let result = found.search("lisbon").filter(eq("chat_id", &7)).run()?; // in this chat only
```

## Words in any language, and typos

The index needs no language settings. It reads Latin, Cyrillic, Greek, Arabic,
Chinese and Japanese text as it comes, and reduces Russian words to their
stem:

```ts
await found.search('хлеба') // finds "Как испечь хлеб на закваске?"
const { hits, corrected } = await found.search({ text: 'alfamma', where: { chat_id: 7 } })
corrected // 'alfama': the word the search read instead
```

```python
await found.search("хлеба")  # finds "Как испечь хлеб на закваске?"
result = await found.search(text="alfamma", where={"chat_id": 7})
result.corrected  # 'alfama': the word the search read instead
```

```go
result, err = found.Search(ctx, "хлеба") // finds "Как испечь хлеб на закваске?"
result, err = found.Search(ctx, "alfamma", search.Where{"chat_id": 7})
result.Corrected // "alfama": the word the search read instead
```

```rust
let result = found.search("хлеба").run()?; // finds "Как испечь хлеб на закваске?"
let result = found.search("alfamma").filter(eq("chat_id", &7)).run()?;
result.corrected // Some("alfama"): the word the search read instead
```

A word that matches nothing is read as the nearest word the index holds: one
letter off for a word of five to eight letters, two for a longer one. A word
that matches something is never changed (contract SRCH-09). `corrected` lets
the page say "Showing results for alfama".

## Search by meaning

"Sourdough" appears in no message, but a recipe for bread is what the user
wants. Your embedding model turns the question into a vector, and the index
finds the messages whose vectors are nearest, together with the words:

```ts
const question = await embedder.embed('sourdough') // your model: an array of 1024 numbers
const { hits } = await found.search({ text: 'sourdough', vector: question, where: { chat_id: chatIds } })
```

```python
question = await embedder.embed("sourdough")  # your model: a list of 1024 floats
result = await found.search(text="sourdough", vector=question, where={"chat_id": chat_ids})
```

```go
question, err := embedder.Embed(ctx, "sourdough") // your model: a []float32 of 1024
result, err = found.Search(ctx, "sourdough", search.Vector(question), search.Where{"chat_id": chatIDs})
```

```rust
let question: Vec<f32> = embedder.embed("sourdough")?; // your model: 1024 floats
let result = found.search("sourdough").vector(&question).filter(is_in("chat_id", &chat_ids)).run()?;
```

The search asks the words and the vectors for their best hundred each,
scales both sets of scores to 0–1, and adds them, half and half unless
`hybrid: { vectorWeight }` says otherwise. A message that only the vectors
find, like the bread recipe here, still comes first.

TinyStore never runs a model. The reply worker from [Replies in the
background](replies.md) is a good place to fill the column: one more step
that asks the embedding model and writes the vector as little-endian floats:

```ts
const vector = await run.step('embedding', () => embedder.embed(answer))
await db.exec`update messages set embedding = ${new Uint8Array(new Float32Array(vector).buffer)} where id = ${replyId}`
```

```python
vector = await run.step("embedding", lambda: embedder.embed(answer))
await db.exec("update messages set embedding = ? where id = ?", struct.pack(f"<{len(vector)}f", *vector), reply_id)
```

```go
vector, err := jobs.Step(ctx, run, "embedding", func(ctx context.Context) ([]float32, error) {
	return embedder.Embed(ctx, answer)
})
_, err = db.Exec(ctx, `update messages set embedding = ? where id = ?`, littleEndian(vector), replyID)
```

```rust
let vector: Vec<f32> = run.step("embedding", || embedder.embed(&answer))?;
let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
db.exec(sql!("update messages set embedding = ? where id = ?", bytes, reply_id))?;
```

## A filter keeps to the user's messages first

Another user wrote about Lisbon a thousand times. Ana wrote about it once:

```scene
code ts
  const { hits } = await found.search({ text: 'lisbon', where: { chat_id: chatIds } })
end
actor ana   "Ana"            at 10,50 icon=user
actor index "messages_search" at 50,50 icon=search
actor hers  "Ana's chats"    at 90,22 icon=message-square
actor other "Other chats"    at 90,78 icon=users
link ana index
link index hers
link index other
set hers "1 about Lisbon"
set other "1,000 about Lisbon"

step "Ana searches for lisbon in her own chats: the filter holds the ids of her chats." line=1
  send ana -> index "lisbon, chat_id in [7, 8]"

step "The index keeps to the messages the filter passes before it ranks, so the thousand other messages never compete." line=1
  pulse hers
  strike other "1,000 about Lisbon"

step "Ana gets her one message. Taking the best ten of all and filtering after would have left her nothing." line=1
  send index -> ana "her message" color=ok
```

A filtered search answers what scoring every message the filter passes would
answer (contract SRCH-03). A search engine that takes the best ten of
everything and then removes the ones the user may not see returns an empty
page whenever other users' messages win. The filter here runs first.

## Why this design?

**Why the index follows the table instead of a copy.** With a separate search
service, the program writes the row and then the document, and every bug,
crash or retry between the two leaves them out of sync. Teams then write
nightly jobs that compare and repair. Here the table's triggers note each
changed row, and every commit of the file brings the index up to date
before it ends. A rollback undoes both.

**Why BM25, exact.** BM25 ranks a word by how rare it is and how often a
message uses it, the standard that search engines use. Many engines
approximate it to be fast. TinyStore scores the exact best ten, using block
bounds to skip documents that cannot win, so a search gives the same answer
every time for the same messages, after any packing or cleanup.

**Why vectors come from your model.** Embedding models change every few
months, and each has its own price, languages and size. The store keeps the
vectors you give it and searches them exactly, by cosine (contract SRCH-02).
For millions of vectors, `recall: 0.95` switches to clusters, which answer in
a few milliseconds at the cost of missing about one in twenty of the exact
best. You choose; the index never does it silently.

**Why words and vectors are added, not merged by rank.** One common way to
combine them, reciprocal rank fusion, uses only the positions of hits in each
list. In the `search-hybrid` research round (the bge-m3 model on Amazon's
products and twelve StackExchange forums, 600 judged questions each), adding
the two scaled scores ranked better than fusion by rank in all four
comparisons. So it is the default, and `vectorWeight` lets you lean either
way. The [research reports](https://github.com/tinyshed/research/tree/main/tinystore/reports)
have every number.

## Limits and defaults

| What               | Limit                             |
|--------------------|-----------------------------------|
| a search's text    | 1 KiB, its first 32 words         |
| `limit` + `offset` | 1,000; `limit` is 10 unless set   |
| a vector           | 4,096 dimensions                  |
| a field's weight   | above 0, at most 1,000            |
| a filter           | 8 conditions, 100 values          |
| a filter's value   | 256 bytes                         |
| `vectorWeight`     | 0.5 unless set, from 0 to 1       |
| typos              | 1 letter from 5 letters, 2 from 9 |

## See also

- [Messages](messages.md): the table this index follows.
- [Replies in the background](replies.md): the worker that can fill the
  vectors.
- [Search API reference](../reference/bun/search.md): every call of indexes
  and searches.
