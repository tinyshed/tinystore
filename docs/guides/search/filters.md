# Filters

A filter keeps a search to the documents it passes: one user's notes, a shop's
products in stock under 100 euros. The index ranks only those, so a filter
never empties a page that other documents would have won.

```ts
const products = store.index<Product>('products', {
	fields: { title: 3, body: 1 },
	filters: ['brand', 'price', 'tags', 'inStock'],
})

const { hits } = await products.search({
	text: 'running shoes',
	where: { brand: ['north', 'swift'], price: { gte: 50, lt: 150 }, inStock: true },
})
```

```python
products = store.index("products", Product, fields={"title": 3, "body": 1}, filters=["brand", "price", "tags", "in_stock"])

result = await products.search(
    "running shoes",
    where={"brand": ["north", "swift"], "price": {"gte": 50, "lt": 150}, "in_stock": True},
)
```

```go
products, err := search.OpenIndex[Product](store, "products",
	search.Fields{"title": 3, "body": 1}, search.Filters("brand", "price", "tags", "in_stock"))

result, err := products.Search(ctx, "running shoes", search.Where{
	"brand":    []string{"north", "swift"},
	"price":    search.Range{Gte: 50, Lt: 150},
	"in_stock": true,
})
```

```rust
let products = store
    .index::<Product>("products")
    .field("title", 3.0)
    .field("body", 1.0)
    .filters(["brand", "price", "tags", "in_stock"])
    .open()?;

let found = products
    .search("running shoes")
    .filter(is_in("brand", &["north", "swift"]))
    .filter(range("price", 50..150))
    .filter(eq("in_stock", &true))
    .run()?;
```

An index names its filters when it opens. A `where` on a field it does not
name is `invalid`, so a misspelt filter fails instead of passing everything.

## Conditions

`where` reads the way a table's `where` does in [SQL](../sql/tables.md):

| Condition     | TypeScript and Python             | Go                                        | Rust                      |
|---------------|-----------------------------------|-------------------------------------------|---------------------------|
| equal         | `{ brand: 'north' }`              | `Where{"brand": "north"}`                 | `eq("brand", "north")`    |
| one of a list | `{ brand: ['north', 'swift'] }`   | `Where{"brand": []string{…}}`             | `is_in("brand", &[…])`    |
| a range       | `{ price: { gte: 50, lt: 150 } }` | `Where{"price": Range{Gte: 50, Lt: 150}}` | `range("price", 50..150)` |
| no value      | `{ deletedAt: null }`             | `Where{"deleted_at": nil}`                | `missing("deleted_at")`   |
| any of        | `search.or({…}, {…})`             | `search.Or(Where{…}, Where{…})`           | `or([…])`                 |
| every one of  | `search.and({…}, {…})`            | `search.And(…)`                           | `and([…])`                |

Every field named in one `where` must pass; `or` and `and` group them:

```ts
await products.search({
	text: 'boots',
	where: search.or({ brand: 'north', inStock: true }, { tags: 'sale' }),
}) // north's boots in stock, or any boots on sale
```

```python
await products.search("boots", where=search.or_({"brand": "north", "in_stock": True}, {"tags": "sale"}))
```

```go
result, err = products.Search(ctx, "boots",
	search.Or(search.Where{"brand": "north", "in_stock": true}, search.Where{"tags": "sale"}))
```

```rust
let found = products
    .search("boots")
    .filter(or([and([eq("brand", "north"), eq("in_stock", &true)]), eq("tags", "sale")]))
    .run()?;
```

## What matches

| Document holds          | `where`              | Passes?                                        |
|-------------------------|----------------------|------------------------------------------------|
| `brand: 'North'`        | `{ brand: 'north' }` | no: values match exactly, case included        |
| `price: 50`             | `{ price: 50.0 }`    | yes: `50.0` is `50`                            |
| `sku: '50'`             | `{ sku: 50 }`        | no: the text `"50"` is not the number 50       |
| `tags: ['sale', 'new']` | `{ tags: 'new' }`    | yes: a list passes when one of its values does |
| no `brand`, or `null`   | `{ brand: null }`    | yes                                            |
| `tags: []`              | `{ tags: null }`     | yes: an empty list holds no value              |
| `price: 50`             | `{ brand: [] }`      | no: an empty list passes nothing               |

A range is of numbers only: `gt`, `gte`, `lt`, `lte`, any of them. Numbers
compare as 64-bit floats in a range and exactly in `eq`. Text is matched
whole: a filter is a key, not a search, so `brand: 'nor'` finds nothing.

<div data-for="rust">

In Rust, `eq(field, &None)` is `invalid`: an id that went missing must not
quietly ask for every document without one. `missing(field)` asks for those.

</div>

## Filters and words

A filter alone ranks nothing: a search needs words or a vector, and
`search({ where })` with neither finds no hits. To list documents by a field,
a shop's products of one brand in name order, keep them in a table and query
it with [SQL](../sql/tables.md), or index the table and search it with
[a table's index](table-index.md).

## How a filter runs

A filtered search answers what scoring every document the filter passes would
answer (contract SRCH-03). The index chooses how:

- a filter that passes few documents, a user's own notes among millions, is
  read first, and only those documents are scored;
- a filter that passes many, products in stock, is checked as the search
  walks the best candidates, skipping the rest.

Either way the answer is the same exact best ten. A search engine that takes
the best ten of everything and drops the ones the filter refuses returns an
empty page whenever other documents win; this one does not.

## Limits

| What                    | Limit                                      |
|-------------------------|--------------------------------------------|
| conditions in a `where` | 8                                          |
| values in a `where`     | 100, every list counted                    |
| a filter's value        | 256 bytes                                  |
| what a filter keeps     | text, numbers, booleans, and lists of them |
| `not`                   | none yet: write the values you keep        |

## See also

- [Search](README.md): the engine's overview.
- [Rebuilds](rebuilds.md): adding a filter to an index that holds documents.
- [A filter keeps to the user's messages first](../../handbook/chat/finding-old-messages.md#a-filter-keeps-to-the-users-messages-first)
  in the Handbook, with a scene.
