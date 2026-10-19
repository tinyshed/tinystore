# Rebuilds

An index is changed by opening it with other settings. A weight changes at
once; a new field or filter is read from the documents beside the index,
which answers and takes writes meanwhile, and switches in when ready. No
search ever sees a half-built index.

```ts
// yesterday: { fields: { title: 2, body: 1 }, filters: ['brand'] }
const products = store.index<Product>('products', {
	fields: { title: 3, body: 1, brand: 1 },
	filters: ['brand', 'price'],
})
await products.status() // { state: 'rebuilding', done: 0.42 }
await products.ready() // once it serves brand's words and the price filter
```

```python
products = store.index("products", Product, fields={"title": 3, "body": 1, "brand": 1}, filters=["brand", "price"])
await products.status()  # IndexStatus(state='rebuilding', done=0.42)
await products.ready()
```

```go
products, err := search.OpenIndex[Product](store, "products",
	search.Fields{"title": 3, "body": 1, "brand": 1}, search.Filters("brand", "price"))
status, err := products.Status(ctx) // search.Status{State: search.Rebuilding, Done: 0.42}
err = products.Ready(ctx)
```

```rust
let products = store
    .index::<Product>("products")
    .field("title", 3.0)
    .field("body", 1.0)
    .field("brand", 1.0)
    .filters(["brand", "price"])
    .open()?;
products.status()?; // Status::Rebuilding { done: 0.42 }
products.ready()?;
```

Opening never waits. A search never waits either: it answers by the settings
the index serves now, which `status()` says.

## What each change costs

| What changed                                | What happens                                                     |
|---------------------------------------------|------------------------------------------------------------------|
| a field's weight, `typoTolerance`, `hybrid` | at once: nothing is written again                                |
| a field or a filter removed                 | at once from searches; its keys dropped in the background        |
| a field or a filter added                   | read from the kept documents in the background, then switched in |
| `recall` added or removed                   | clusters built or dropped in the background; searches stay exact |
| `vectors` added                             | at once: documents have none until they are set again            |
| the vectors' dimensions, `vectors` left out | `invalid`: the store cannot make your model's vectors            |

A weight is read by each search as it runs, which is why it is free. A field
added needs every document's words, which is why it is read again.

## While it rebuilds

- The new rows read the old ones' documents in order, 256 a commit, and the
  writer is held only to write each step.
- A set meanwhile reaches the new rows too, a delete reaches both at once, so
  nothing set during a rebuild is lost.
- Once the new rows hold every document, they switch in, in one commit, and
  the old ones are dropped a step at a time.
- A restart goes on where it stopped (contract SRCH-06).
- Every change is said in the store's log when it starts and when it is done:

```text
search products: brand added: reading 812345 documents beside the index, which answers as before until then
search products: brand added ready in …; switched
```

- A rebuild that cannot read a document, a number where a new field reads
  text, stops: `status()` is `failed` with why, `ready()` throws it, and the
  index answers by its old settings until the next change.

## Rebuilding by hand

A program that schedules its own load opens its index with `rebuild:
'manual'`. A change that needs background work is then `invalid`, naming
itself, until `store.reindex` asks for it:

```ts
const products = store.index<Product>('products', { fields, filters, rebuild: 'manual' })
// at night, from a job:
const rebuilt = await store.reindex<Product>('products', { fields: { title: 3, body: 1, brand: 1 }, filters })
await rebuilt.ready()
```

```python
products = store.index("products", Product, fields=fields, filters=filters, rebuild="manual")
# at night, from a job:
rebuilt = await store.reindex("products", Product, fields={"title": 3, "body": 1, "brand": 1}, filters=filters)
await rebuilt.ready()
```

```go
products, err := search.OpenIndex[Product](store, "products", fields, filters, search.Rebuild(search.Manual))
// at night, from a job: returns once the new settings serve
err = search.Reindex(ctx, store, "products", search.Fields{"title": 3, "body": 1, "brand": 1}, filters)
```

```rust
let products = store.index::<Product>("products").fields(["title", "body"]).rebuild(Rebuild::Manual).open()?;
// at night, from a job:
let rebuilt = store
    .index::<Product>("products")
    .field("title", 3.0)
    .field("body", 1.0)
    .field("brand", 1.0)
    .rebuild(Rebuild::Manual)
    .reindex()?;
rebuilt.ready()?;
```

A change that needs no background work, a weight, applies at once even under
`manual`. `reindex` with the same settings reads the index again as it is,
which numbers its documents afresh.

## Numbering

Each document has a number inside the index, and a deleted one leaves a gap.
Once the numbers given pass twice the documents held, and 4,096, the index
reads itself again by its own settings, as a rebuild, to close the gaps. A
`manual` index waits for `reindex`, except past 2³¹, where it begins anyway.

## Listing and dropping

```ts
await store.indexes() // [{ name: 'products', documents: 3000, vectors: 0, opened: Date, status: { state: 'ready' } }]
await store.dropIndex('products') // true: dropped, its rows removed in the background
```

```python
await store.indexes()
await store.drop_index("products")
```

```go
indexes, err := search.List(ctx, store)
dropped, err := search.Drop(ctx, store, "products")
```

```rust
let indexes = tinystore::search::list(&store)?;
store.drop_index("products")?;
```

An index no program opens any more is kept until it is dropped, since another
version of the program may open it again. A handle of a dropped index is
refused from then on, and opening the name again makes a new, empty index.

Beside a running server, the command line lists and drops them too:

```sh
tinystore search list ./data
tinystore search drop ./data products
```

## See also

- [Search](README.md): the engine's overview.
- [Fields and weights](fields.md) and [Filters](filters.md): the settings a
  rebuild reads.
- [How search finds](../../handbook/inside/how-search-finds.md) in the
  Handbook.
