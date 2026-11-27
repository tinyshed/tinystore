# Abbreviations and synonyms

Students type «кмзи» for a course named «Криптографические методы защиты
информации», and readers type `ml` for a page about machine learning. An
index's `synonyms` pair each short form with the phrase that it stands for.
Search matches each pair in both directions. So the dictionary lives in the
index, and you don't need extra code for it.

```ts
const catalog = store.index<{ name: string }>('catalog', {
	fields: { name: 4 },
	synonyms: { кмзи: 'криптографические методы защиты информации', ml: 'machine learning' },
})
await catalog.set('c-1', { name: 'Криптографические методы защиты информации' })
await catalog.set('c-2', { name: 'Экзамен по КМЗИ' })
const { hits } = await catalog.search('кмзи') // c-1 by its phrase, c-2 by the word itself
```

```python
catalog = store.index(
    "catalog",
    Course,
    fields={"name": 4},
    synonyms={"кмзи": "криптографические методы защиты информации", "ml": "machine learning"},
)
await catalog.set("c-1", Course("Криптографические методы защиты информации"))
await catalog.set("c-2", Course("Экзамен по КМЗИ"))
result = await catalog.search("кмзи")  # c-1 by its phrase, c-2 by the word itself
```

```go
catalog, err := search.OpenIndex[Course](store, "catalog", search.Fields{"name": 4},
	search.Synonyms{"кмзи": "криптографические методы защиты информации", "ml": "machine learning"})
err = catalog.Set(ctx, "c-1", Course{Name: "Криптографические методы защиты информации"})
err = catalog.Set(ctx, "c-2", Course{Name: "Экзамен по КМЗИ"})
result, err := catalog.Search(ctx, "кмзи") // c-1 by its phrase, c-2 by the word itself
```

```rust
let catalog = store
    .index::<Course>("catalog")
    .field("name", 4.0)
    .synonyms([("кмзи", "криптографические методы защиты информации"), ("ml", "machine learning")])
    .open()?;
catalog.set("c-1", &Course { name: "Криптографические методы защиты информации".into() })?;
catalog.set("c-2", &Course { name: "Экзамен по КМЗИ".into() })?;
let found = catalog.search("кмзи").run()?; // c-1 by its phrase, c-2 by the word itself
```

## Both ways

- A search for the short form finds a document whose field contains the
  phrase. The phrase's words must be next to each other, in order, in one
  field. The words are compared by their stems, the same way the index
  compares all words. So «Криптографических методов защиты информации» in
  another grammatical case matches too.
- A search for the phrase's words finds a document that contains the short
  form. For example, `защиты информации` finds «Экзамен по КМЗИ».
- If the phrase's words are apart or out of order, they don't count as the
  phrase. For example, «методы и защиты» doesn't count as «кмзи».

`matches` and [snippets](matches.md#cut-a-snippet) mark the text that is
actually in the document. A search for the short form marks the whole
phrase, and a search for the phrase's words marks the short form (contract
SRCH-23).

## What it costs

The index applies synonyms when a document is set, not when a search runs.
It stores the short form next to the phrase, and the phrase's words next to
the short form, in the same postings as every other word. So synonyms make
searches no slower, and an index without synonyms pays nothing at all.

The words that a synonym adds don't count toward a field's length. A
field's length counts only the words written in it. So a long expansion
never lowers the score of the field's own words.

## Changing them

Synonyms are settings. To change them, open the index under the same name
with other synonyms. If the index contains documents, Search reads them
again in the background, as it does when you [add a field](rebuilds.md).
Until the rebuild switches in, searches use the old synonyms. If you open
the index without `synonyms`, the index has none, and its documents are
read again the same way. The same pairs in a different order are the same
synonyms, so nothing is read again. With `rebuild: 'manual'`, the change
waits for `store.reindex`.

## Limits

| What                 | Value                                     |
|----------------------|-------------------------------------------|
| synonyms per index   | 1,000 at most                             |
| a short form         | one word after analysis, 64 bytes at most |
| a phrase             | 1 to 8 words, none of them the short form |
| the cost to a search | none: applied when a document is set      |
| a field's length     | only the words written in it              |

Opening an index fails with an invalid error if it has a short form of two
words, an empty phrase, a phrase that contains its own short form, or more
than 1,000 pairs. The error identifies the index and the pair.

Once this release opens a store's search.db, the release before it can't
open that store. Otherwise the older release would set documents without
their synonyms' words.

## Not yet

- A typo inside a prefix: «прагр» does not yet find «программирование».
- Facets, meaning counts of hits per value of a filter.

## See also

- [Fields and weights](fields.md): which text is read.
- [Show what matched](matches.md): `matches` and snippets.
- [Rebuilds](rebuilds.md): a change of settings while the index answers.
