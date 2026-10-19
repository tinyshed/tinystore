# Fields and weights

An index reads the words of the fields it names, each with a weight: a word
found in a product's title counts three times a word in its description. The
document itself is kept whole, as JSON, and a hit gives it back.

```ts
interface Product {
	title: string
	body: string
	brand: string
	price: number
	tags: string[]
	author: { name: string }
}

const products = store.index<Product>('products', { fields: { title: 3, body: 1, 'author.name': 1 } })

await products.set('p-1', { title: 'Trail running shoes', body: 'Light shoes for wet trails', brand: 'north', price: 120, tags: ['sale'], author: { name: 'Ann' } })
const { hits } = await products.search({ text: 'running shoes' })
hits[0] // { id: 'p-1', value: { title: 'Trail running shoes', … }, score: … }
```

```python
products = store.index("products", Product, fields={"title": 3, "body": 1, "author.name": 1})

await products.set("p-1", Product(title="Trail running shoes", body="Light shoes for wet trails", brand="north", price=120, tags=["sale"], author=Author(name="Ann")))
result = await products.search("running shoes")
result.hits[0]  # Hit(id='p-1', value=Product(title='Trail running shoes', …), score=…)
```

```go
products, err := search.OpenIndex[Product](store, "products", search.Fields{"title": 3, "body": 1, "author.name": 1})

err = products.Set(ctx, "p-1", Product{Title: "Trail running shoes", Body: "Light shoes for wet trails", Brand: "north", Price: 120})
result, err := products.Search(ctx, "running shoes")
```

```rust
let products = store.index::<Product>("products").field("title", 3.0).field("body", 1.0).field("author.name", 1.0).open()?;

products.set("p-1", &trail_shoes)?;
let found = products.search("running shoes").run()?;
```

`fields: ['title', 'body']` (`FieldNames` in Go, `.fields([…])` in Rust)
weighs each 1. A path reaches inside: `author.name`, or a list of strings,
each read as text. A field the document lacks, or holds as null, adds no
words; a number, a boolean or an object there makes the set `invalid`, since
it is not text. Numbers belong in [filters](filters.md).

## Documents

| Call             | Does                                                            |
|------------------|-----------------------------------------------------------------|
| `set(id, value)` | sets the document, replacing it whole; the next search finds it |
| `setMany([…])`   | sets up to 1,000 documents in one commit, all or none           |
| `get(id)`        | the document as it was set, or none                             |
| `delete(id)`     | deletes it, and says whether it was there; never found again    |

```ts
await products.setMany([
	{ id: 'p-2', value: winterBoots },
	{ id: 'p-3', value: roadShoes },
])
await products.get('p-2') // the boots, as they were set
await products.delete('p-2') // true
```

```python
await products.set_many([("p-2", winter_boots), ("p-3", road_shoes)])
await products.get("p-2")  # the boots, as they were set
await products.delete("p-2")  # True
```

```go
err = products.SetMany(ctx, []search.Doc[Product]{{ID: "p-2", Value: winterBoots}, {ID: "p-3", Value: roadShoes}})
boots, found, err := products.Get(ctx, "p-2")
deleted, err := products.Delete(ctx, "p-2")
```

```rust
products.set_many([("p-2", &winter_boots), ("p-3", &road_shoes)])?;
let boots = products.get("p-2")?;
let deleted = products.delete("p-2")?;
```

A set is seen by the next search, and a deleted document never answers again
(contract SRCH-04). `setMany` commits every document or none (contract
SRCH-05), and costs one commit: an import of a million products is a
thousand calls, not a million.

## How the score is made

A search scores each document by BM25, the ranking search engines have used
for decades:

- a word that few documents hold counts more than a common one: `trail`
  more than `shoes`;
- a word a document holds several times counts more, with less for each
  repeat;
- a short field that holds the word counts more than a long one;
- a word's count is multiplied by its field's weight.

The answer is what scoring every document would give, never an estimate
(contract SRCH-01). Of equal scores, the document set first comes first. A
`score` orders the hits of one search; compare scores within a search, not
across two.

## Words in any language

The index needs no language setting. Every field's text is read the same way:

| Step                    | What it does                                                 |
|-------------------------|--------------------------------------------------------------|
| normalised              | NFKC, lower case, Latin accents off: `Café` is `cafe`        |
| cut into words          | by Unicode's word rules, for every script                    |
| Cyrillic                | each Russian word also by its stem: `хлеба` finds `хлеб`     |
| Chinese, Japanese, Thai | as overlapping pairs of characters, which need no dictionary |

A search's text is read the same way, so `CAFÉ`, `cafe` and `café` find the
same documents.

## Pages

```ts
const page2 = await products.search({ text: 'shoes', limit: 20, offset: 20 })
```

```python
page2 = await products.search("shoes", limit=20, offset=20)
```

```go
page2, err := products.Search(ctx, "shoes", search.Limit(20), search.Offset(20))
```

```rust
let page2 = products.search("shoes").limit(20).offset(20).run()?;
```

A search gives the best 10 unless `limit` says. `limit` and `offset`
together reach 1,000 at most: a person never reads further, and an exact top
1,000 stays cheap.

## Limits

| What               | Limit                           |
|--------------------|---------------------------------|
| a field's weight   | above 0, at most 1,000          |
| `setMany`          | 1,000 documents, 16 MiB         |
| a search's text    | 1 KiB, its first 32 words       |
| `limit` + `offset` | 1,000; `limit` is 10 unless set |
| an index's name    | `[a-z0-9][a-z0-9_-]{0,63}`      |

## See also

- [Search](README.md): the engine's overview.
- [Rebuilds](rebuilds.md): changing an index's fields and weights later.
- [How search finds](../../handbook/inside/how-search-finds.md) in the
  Handbook: postings, blocks and BM25 inside the index.
