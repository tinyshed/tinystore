# Words and vectors together

A search with both `text` and `vector` asks the words and the vectors at once.
Words find the exact term, a model number or a name; vectors find the
meaning, a question asked in other words. Together they find both.

```ts
const question = 'how do I keep a starter alive'
const { hits } = await docs.search({ text: question, vector: await embed(question) })
```

```python
question = "how do I keep a starter alive"
result = await docs.search(question, vector=await embed(question))
```

```go
question := "how do I keep a starter alive"
result, err := docs.Search(ctx, question, search.Vector(embed(question)))
```

```rust
let question = "how do I keep a starter alive";
let found = docs.search(question).vector(&embed(question)).run()?;
```

## How the two are combined

By default the scores are added:

1. the words give their best hundred by BM25, the vectors their best hundred
   by cosine, or `offset + limit` each when that is more;
2. each list's scores are scaled to 0–1, its lowest becoming 0 and its
   highest 1;
3. a document's score is `(1 − w) × words + w × vectors`, with `w`, the
   vector weight, 0.5 unless set; a document one half did not find scores 0
   there.

So a document both halves found near the top comes first, and one that only
the vectors found, the recipe that never says "starter", still makes the
page.

```ts
await docs.search({ text: question, vector: q, hybrid: { vectorWeight: 0.8 } }) // lean on meaning
```

```python
await docs.search(question, vector=q, hybrid=tinystore.Hybrid(vector_weight=0.8))  # lean on meaning
```

```go
result, err = docs.Search(ctx, question, search.Vector(q), search.Hybrid{VectorWeight: 0.8}) // lean on meaning
```

```rust
let found = docs.search(question).vector(&q).vector_weight(0.8).run()?; // lean on meaning
```

| `vectorWeight` | Ranks by                              |
|----------------|---------------------------------------|
| 0              | words alone; vectors only fill places |
| 0.5            | both equally, the default             |
| 1              | vectors alone; words only fill places |

The weight can be set where the index opens, `hybrid: { vectorWeight }`, as
the index's default, and per search, which wins. Changing the index's default
applies at once.

## Rerank

`rerank` takes the words' best hundred and orders them by their vectors
alone. Every hit holds a word of the question; the vectors decide which of
them is meant.

```ts
await docs.search({ text: 'jaguar', vector: await embed('jaguar the car'), hybrid: { rerank: true } })
```

```python
await docs.search("jaguar", vector=await embed("jaguar the car"), hybrid=tinystore.Hybrid(rerank=True))
```

```go
result, err = docs.Search(ctx, "jaguar", search.Vector(embed("jaguar the car")), search.Hybrid{Rerank: true})
```

```rust
let found = docs.search("jaguar").vector(&embed("jaguar the car")).rerank().run()?;
```

A hit's score is then its cosine, and a document without a vector comes last,
scored −1.
`rerank` needs both `text` and `vector`, and is `invalid` beside a
`vectorWeight`.

## Which to use

| You want                                                       | Use                      |
|----------------------------------------------------------------|--------------------------|
| a search box that understands both terms and questions         | both, the default sum    |
| exact terms first, meaning to break ties: codes, names, jargon | sum, `vectorWeight` low  |
| questions in the user's own words: support, docs, notes        | sum, `vectorWeight` high |
| only hits that hold a word, ordered by what was meant          | `rerank`                 |

## Why this design?

**Why add scores rather than fuse ranks.** Reciprocal rank fusion, a common
way to combine two lists, reads only each hit's position, so a hit that one
half scored far above the rest counts the same as one barely ahead. In the
`search-hybrid` research round (the bge-m3 model on Amazon's products and
twelve StackExchange forums, 600 judged questions each), the scaled sum
ranked better than fusion by rank in all four comparisons, so it is the
default. The [research reports](https://github.com/tinyshed/research/tree/main/tinystore/reports)
have every number and the command that reproduces it.

**Why each half's best hundred.** Scaling needs a best and a worst. A hundred
is more than a page ever shows, and few enough that both halves stay exact
and fast. A document past the hundredth of both halves would score near zero
in any case.

## See also

- [Search](README.md): the engine's overview.
- [Vectors](vectors.md): the vectors half on its own.
- [Fields and weights](fields.md): the words half on its own.
