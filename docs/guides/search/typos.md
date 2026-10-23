# Typos

A word that matches nothing in the index is read as the nearest word the
index holds, and `corrected` says what the search read in fact, so the page
can show "Showing results for running shoes".

```ts
const { hits, corrected } = await products.search({ text: 'runing shoos' })
corrected // 'running shoes'
```

```python
result = await products.search("runing shoos")
result.corrected  # 'running shoes'
```

```go
result, err := products.Search(ctx, "runing shoos")
result.Corrected // "running shoes"
```

```rust
let found = products.search("runing shoos").run()?;
found.corrected // Some("running shoes")
```

`corrected` is absent (`""` in Go) when the search read its words as given.

## The rules

| A word of         | Is read with                    |
|-------------------|---------------------------------|
| up to 4 letters   | no change: `cat` is never `car` |
| 5 to 8 letters    | one edit                        |
| 9 letters or more | two edits                       |

An edit is a letter added, dropped or changed, or two neighbours swapped:
`shose` is one edit from `shoes`.

- A word that matches something is never changed (contract SRCH-09). `bots`
  finds documents with `bots`, even when `boots` is far more common.
- Each word is corrected on its own: `runing shoes` keeps `shoes` and reads
  `running`.
- A word is near only words of its own script: a Latin typo is never read as
  a Cyrillic word.
- Chinese, Japanese and Thai text has no spaces, so the index reads it as
  overlapping pairs of characters. When two neighbouring pairs are both
  corrected, the search reads both, and `corrected` shows the first.
- A search's words are any, not all: a document with one of them is a hit,
  ranked below those with more.

## Turn it off

For codes, ids and names where a near word is a wrong word, an SKU or a
ticket number, open the index without it:

```ts
const parts = store.index<Part>('parts', { fields: ['sku', 'name'], typoTolerance: false })
```

```python
parts = store.index("parts", Part, fields=["sku", "name"], typo_tolerance=False)
```

```go
parts, err := search.OpenIndex[Part](store, "parts", search.FieldNames("sku", "name"), search.TypoTolerance(false))
```

```rust
let parts = store.index::<Part>("parts").fields(["sku", "name"]).typo_tolerance(false).open()?;
```

Turning it on or off applies at once: nothing in the index is written again.

## Where the words come from

The index keeps a dictionary of its own words, one per script, built the
first time a search needs it. After writes, searches go on with the
dictionary they have, reading no word their snapshot no longer holds, and
background work builds it again within a minute. A word added a few seconds
ago is found exactly at once, and as a correction a minute later.

## Why this design?

**Why only when a word matches nothing.** A search engine that corrects
words that exist second-guesses the user: a search for a rare brand turns
into a search for a common word. Correcting only what matches nothing never
loses a hit the user typed for.

**Why five and nine letters.** Short words sit one edit from many others:
`cat`, `car`, `cut`, `cot`. Correcting them reads noise. The thresholds are
the ones Meilisearch uses, so a search box behaves as people already expect.

**Why say `corrected`.** A search that silently searches for something else
leaves the user wondering why the hits do not hold their word. Saying what
was searched lets the page say so, and offer the original.

## See also

- [Search](README.md): the engine's overview.
- [Fields and weights](fields.md): how words are read.
- [Words in any language, and typos](../../handbook/chat/finding-old-messages.md#words-in-any-language-and-typos)
  in the Handbook.
