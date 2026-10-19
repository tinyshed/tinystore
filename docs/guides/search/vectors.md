# Vectors

An index with `vectors` keeps a vector beside each document, as your
embedding model made it, and finds the documents nearest a question's vector
by cosine: "sourdough" finds the bread recipe that never says the word.
TinyStore never runs a model; it keeps and searches what yours returns.

```ts
const docs = store.index<Doc>('docs', { fields: ['title', 'text'], vectors: { dimensions: 1024 } })

await docs.set('d-1', { title: 'Bread at home', text: '…' }, { vector: await embed('Bread at home …') })
const { hits } = await docs.search({ vector: await embed('sourdough') })
```

```python
docs = store.index("docs", Doc, fields=["title", "text"], vectors=tinystore.Vectors(dimensions=1024))

await docs.set("d-1", Doc(title="Bread at home", text="…"), vector=await embed("Bread at home …"))
result = await docs.search(vector=await embed("sourdough"))
```

```go
docs, err := search.OpenIndex[Doc](store, "docs", search.FieldNames("title", "text"), search.Vectors{Dimensions: 1024})

err = docs.Set(ctx, "d-1", Doc{Title: "Bread at home", Text: "…"}, search.Vector(embed("Bread at home …")))
result, err := docs.SearchVector(ctx, embed("sourdough"))
```

```rust
let docs = store.index::<Doc>("docs").fields(["title", "text"]).vectors(1024).open()?;

docs.id("d-1").vector(&embed("Bread at home …")).set(&bread)?;
let found = docs.search_vector(&embed("sourdough")).run()?;
```

A hit's `score` is the cosine of the two vectors, from −1 to 1.

## Setting a document's vector

On an index with vectors, every set says what the document's vector is:

| Set                                                        | Means                                             |
|------------------------------------------------------------|---------------------------------------------------|
| with a vector                                              | found by its words and by its vector              |
| `vector: null` (`None`, `NoVector()`, `.without_vector()`) | found by its words only: a draft not embedded yet |
| saying nothing                                             | `invalid`: a forgotten vector fails loudly        |

```ts
await docs.set('d-2', draft, { vector: null }) // embedded later, by a job
await docs.setMany([
	{ id: 'd-3', value: a, vector: va },
	{ id: 'd-4', value: b, vector: vb },
])
```

```python
await docs.set("d-2", draft, vector=None)  # embedded later, by a job
await docs.set_many([("d-3", a, va), ("d-4", b, vb)])
```

```go
err = docs.Set(ctx, "d-2", draft, search.NoVector()) // embedded later, by a job
err = docs.SetMany(ctx, []search.Doc[Doc]{{ID: "d-3", Value: a, Vector: va}, {ID: "d-4", Value: b, Vector: vb}})
```

```rust
docs.id("d-2").without_vector().set(&draft)?; // embedded later, by a job
docs.set_many([("d-3", &a, &va[..]), ("d-4", &b, &vb[..])])?;
```

A vector has the index's dimensions, every number finite, not all zeros;
anything else is `invalid` and names the document. Its length does not
matter: cosine reads its direction.

## Exact by default

A search by a vector answers the exact best by cosine (contract SRCH-02). It
reads every vector's compact code, a byte per dimension, which bounds how
near each can be, and scores again on the full floats only the ones the
bound cannot rule out. The answer is the one comparing every vector would
give, at a fraction of the reading.

An exact search's cost grows with the number of vectors, since it reads
every code. Measure it on your own data: while it fits the time a page has,
there is no reason to give up exactness.

## Faster with `recall`

When it no longer fits, `recall` trades exactness for speed: the index groups its vectors
into clusters and a search reads the nearest clusters only.

```ts
const docs = store.index<Doc>('docs', { fields: ['title', 'text'], vectors: { dimensions: 1024, recall: 0.95 } })
```

```python
docs = store.index("docs", Doc, fields=["title", "text"], vectors=tinystore.Vectors(dimensions=1024, recall=0.95))
```

```go
docs, err := search.OpenIndex[Doc](store, "docs", search.FieldNames("title", "text"),
	search.Vectors{Dimensions: 1024, Recall: 0.95})
```

```rust
let docs = store.index::<Doc>("docs").fields(["title", "text"]).vectors(1024).recall(0.95).open()?;
```

- `recall: 0.95` asks that about 95 % of the exact best hits be found. The
  index chooses how many clusters a search reads, and how many members it
  scores again, by measuring that on its own vectors.
- Each hit's score is still its exact cosine: what is approximate is which
  documents are found, never their scores.
- The clusters are built in the background. Until they serve, and whenever
  they cannot reach the recall asked, searches stay exact.
- Adding or removing `recall` later is a change of settings: see
  [Rebuilds](rebuilds.md).

## Dimensions are the model's

The dimensions are fixed when the index is made. Opening it with others is
`invalid`: the store holds the vectors your model made and has nothing to
make new ones from. Changing models is a new index, filled as the old one
answers:

```ts
const next = store.index<Doc>('docs-v2', { fields: ['title', 'text'], vectors: { dimensions: 1536 } })
// a job sets every document in next with the new model's vectors, then the program switches, then
await store.dropIndex('docs')
```

```python
nxt = store.index("docs-v2", Doc, fields=["title", "text"], vectors=tinystore.Vectors(dimensions=1536))
await store.drop_index("docs")
```

```go
next, err := search.OpenIndex[Doc](store, "docs-v2", search.FieldNames("title", "text"), search.Vectors{Dimensions: 1536})
_, err = search.Drop(ctx, store, "docs")
```

```rust
let next = store.index::<Doc>("docs-v2").fields(["title", "text"]).vectors(1536).open()?;
store.drop_index("docs")?;
```

`vectors` added to an index of words applies at once: its documents have no
vector until they are set again.

## Limits

| What       | Limit                                       |
|------------|---------------------------------------------|
| dimensions | 4,096                                       |
| `recall`   | above 0 and below 1                         |
| a vector   | finite numbers, not all zeros               |
| stored     | about 4.5 KiB of pages for 1,024 dimensions |

## See also

- [Search](README.md): the engine's overview.
- [Words and vectors together](hybrid.md): one search by both.
- [Search by meaning](../../handbook/chat/finding-old-messages.md#search-by-meaning)
  in the Handbook, and [How search finds](../../handbook/inside/how-search-finds.md).
