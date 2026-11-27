# Abbreviations and synonyms

Students type «кмзи» for a course named «Криптографические методы защиты
информации», and readers type `ml` for a page about machine learning. An
index's `synonyms` pair each short form with the phrase it stands for, and
Search reads the pair both ways, so the dictionary lives in the index and
not in code beside it.

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

- A search for the short form finds a document whose field holds the
  phrase: its words next to each other, in order, in one field. The words
  meet by the same stems the index reads, so «Криптографических методов
  защиты информации» in another case counts too.
- A search for the phrase's words finds a document that holds the short
  form: `защиты информации` finds «Экзамен по КМЗИ».
- The phrase's words apart, or out of order, are no phrase: «методы и
  защиты» does not stand for «кмзи».

`matches` and [snippets](matches.md#cut-a-snippet) mark the text that is
there: the whole phrase for a search of the short form, and the short form
for a search of the phrase's words (contract SRCH-23).

## What it costs

Synonyms are read when a document is set, not when a search runs: the index
keeps the short form beside the phrase, and the phrase's words beside the
short form, in the same postings as every other word. A search pays nothing
for them. An index without synonyms pays nothing at all.

The words a synonym adds count in no field's length. A field's length is
the words written in it, so a long expansion never lowers what the field's
own words score.

## Changing them

Synonyms are settings. Open the index with other synonyms under the same
name and, if it holds documents, Search reads them again beside it, as it
does for a [field added](rebuilds.md); until the rebuild switches in, a
search answers by the synonyms the index had. An open without `synonyms`
leaves the index none, read again the same way. The same pairs in another
order are the same synonyms and read nothing again. With
`rebuild: 'manual'`, the change waits for `store.reindex`.

## Limits

| What              | Value                                     |
|-------------------|-------------------------------------------|
| synonyms an index | 1,000 at most                             |
| a short form      | one word after analysis, 64 bytes at most |
| a phrase          | 1 to 8 words, the short form none of them |
| a search's cost   | none: read when a document is set         |
| a field's length  | its written words only                    |

A short form of two words, an empty phrase, a phrase that holds its own short
form or more than 1,000 pairs is refused as invalid, naming the index and
the pair.

A store whose search.db this release opened cannot be opened by the release
before it, which would set documents without their synonyms' words.

## Not yet

- A typo inside a prefix: «прагр» does not yet find «программирование».
- Counts of hits by a filter's values, as facets.

## See also

- [Fields and weights](fields.md): which text is read.
- [Show what matched](matches.md): `matches` and snippets.
- [Rebuilds](rebuilds.md): a change of settings while the index answers.
