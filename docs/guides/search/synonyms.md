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

## A word for one document: aliases

Some short forms mean one thing in one document and another thing in the
next: «тп» is a dozen different courses, and a catalog computes a code for
each new course every week. Give such a word to the document itself, as an
alias. An alias names a field, and stands for that field's whole text in
this document only.

```ts
await catalog.set('c-1', { name: 'Криптографические методы защиты информации' }, { aliases: { кмзи: 'name' } })
await catalog.setMany([
	{ id: 'c-7', value: { name: 'Теория принятия решений' }, aliases: { тп: 'name' } },
	{ id: 'c-8', value: { name: 'Технологии программирования' }, aliases: { тп: 'name' } },
])
const { hits } = await catalog.search({ text: 'тп', matches: true }) // c-7 and c-8, each name marked whole
```

```python
from tinystore.search import Doc

await catalog.set("c-1", Course("Криптографические методы защиты информации"), aliases={"кмзи": "name"})
await catalog.set_many([
    Doc("c-7", Course("Теория принятия решений"), aliases={"тп": "name"}),
    Doc("c-8", Course("Технологии программирования"), aliases={"тп": "name"}),
])
result = await catalog.search("тп", matches=True)  # c-7 and c-8, each name marked whole
```

```go
err = catalog.Set(ctx, "c-1", Course{Name: "Криптографические методы защиты информации"},
	search.Aliases{"кмзи": "name"})
err = catalog.SetMany(ctx, []search.Doc[Course]{
	{ID: "c-7", Value: Course{Name: "Теория принятия решений"}, Aliases: search.Aliases{"тп": "name"}},
	{ID: "c-8", Value: Course{Name: "Технологии программирования"}, Aliases: search.Aliases{"тп": "name"}},
})
result, err := catalog.Search(ctx, "тп", search.Matches()) // c-7 and c-8, each name marked whole
```

```rust
use tinystore::search::Doc;

let kmzi = Course { name: "Криптографические методы защиты информации".into() };
catalog.id("c-1").aliases([("кмзи", "name")]).set(&kmzi)?;
let (decisions, programming) =
    (Course { name: "Теория принятия решений".into() }, Course { name: "Технологии программирования".into() });
catalog.set_many([
    Doc::new("c-7", &decisions).aliases([("тп", "name")]),
    Doc::new("c-8", &programming).aliases([("тп", "name")]),
])?;
let found = catalog.search("тп").matches().run()?; // c-7 and c-8, each name marked whole
```

- A search for the alias finds the document as if it matched the field the
  alias names, and scores it with that field's weight. The alias is read
  like any word: case, keyboard [layouts](layouts.md), [prefixes](prefix.md)
  and [typos](typos.md) apply.
- `matches` and snippets mark the field's text, from its first word to its
  last, not the alias, which the document doesn't show.
- No other document is found by the alias, and the same alias can stand for
  a different field or text in each document (contract SRCH-30).
- A set replaces the document's aliases with the ones it gives, so a set
  without `aliases` leaves the document none, and a delete removes them.
  Nothing else is read again: no rebuild, whatever the change (contract
  SRCH-31).
- An alias counts toward no field's length. If a [rebuild](rebuilds.md)
  drops the field an alias names, the alias finds nothing until the field
  comes back.

**Synonyms or aliases?** Use `synonyms` for a short form that means the same
phrase in every document, such as «бд» for «базы данных»: one pair finds
every document that contains the phrase, but a change reads the index again.
Use `aliases` for a word that belongs to one document, such as a course code
or a product's codename: a change is one set, and no other document notices.

## Limits

| What                 | Value                                         |
|----------------------|-----------------------------------------------|
| synonyms per index   | 1,000 at most                                 |
| a short form         | one word after analysis, 64 bytes at most     |
| a phrase             | 1 to 8 words, none of them the short form     |
| aliases per document | 64 at most                                    |
| an alias             | one word after analysis, 64 bytes at most     |
| an alias's field     | one of the index's fields                     |
| the cost to a search | none: both are applied when a document is set |
| a field's length     | only the words written in it                  |

Opening an index fails with an invalid error if it has a short form of two
words, an empty phrase, a phrase that contains its own short form, or more
than 1,000 pairs. The error identifies the index and the pair. A set fails
with an invalid error if an alias is two words, names a field the index
doesn't read, appears twice, or if there are more than 64. The error
identifies the index, the document and the alias.

Once this release opens a store's search.db, the release before it can't
open that store. Otherwise the older release would set documents without
their synonyms' words. The release that adds aliases does the same to
0.13.0, which would read documents again without their aliases.

## Not yet

- A typo inside a prefix: «прагр» does not yet find «программирование».
- Facets, meaning counts of hits per value of a filter.

## See also

- [Fields and weights](fields.md): which text is read.
- [Show what matched](matches.md): `matches` and snippets.
- [Rebuilds](rebuilds.md): a change of settings while the index answers.
