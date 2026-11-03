# As you type

A search box that answers while its reader types sends a word typed halfway:
`crypt` on its way to `cryptography`. With `prefix`, Search reads the last
word of the text as the start of a word too, so the box finds every document
with a word that starts with it, as the reader types each letter.

```ts
const courses = store.index<{ name: string }>('courses', { fields: { name: 1 } })
await courses.setMany([
	{ id: 1, value: { name: 'Cryptographic methods of information security' } },
	{ id: 2, value: { name: 'Cryptanalysis' } },
	{ id: 3, value: { name: 'Cryptography' } },
	{ id: 4, value: { name: 'Foundations of cryptography' } },
])
const { hits } = await courses.search({ text: 'crypt', prefix: true })
hits.map(hit => hit.id) // ['2', '3', '4', '1']
```

```python
courses = store.index("courses", Course, fields={"name": 1})
await courses.set_many([("1", Course("Cryptographic methods of information security")), ("2", Course("Cryptanalysis"))])
result = await courses.search("crypt", prefix=True)
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"name": 1})
result, err := courses.Search(ctx, "crypt", search.Prefix())
```

```rust
let courses = store.index::<Course>("courses").field("name", 1.0).open()?;
let found = courses.search("crypt").prefix().run()?;
```

Without `prefix`, `crypt` is a word of its own: it finds a course named
`Crypt`, and none of the four above.

## Which word is a prefix

With `prefix: true`, only the last word is a prefix, and only while the reader
may still be typing it:

| Text               | Read as                                                    |
|--------------------|------------------------------------------------------------|
| `crypt`            | `crypt`, and every word that starts with it                |
| `methods crypt`    | `methods` whole, then `crypt` and the words it begins      |
| `crypt ` (a space) | `crypt` whole: the reader finished the word                |
| `crypt,`           | `crypt` whole: a mark ends a word as a space does          |
| `c`                | `c` whole: one letter would begin half the words of a page |

A prefix is read as [every word](fields.md#words-in-any-language) is, so
`Crypt` and `crypt` are the same prefix, in any script.

## Every word

People shorten every word of a name, not only the last: `лин алг` for
`Линейная алгебра`, or `теор вер` for `Теория вероятностей`. With
`prefix: 'every'`, Search reads each word of the text as the start of a word
too, and puts first the documents that hold a word for each word of the text.

```ts
await courses.setMany([
	{ id: 5, value: { name: 'Линейная алгебра' } },
	{ id: 6, value: { name: 'Алгоритмизация и программирование' } },
	{ id: 7, value: { name: 'Проектирование технологических линий' } },
])
const { hits } = await courses.search({ text: 'лин алг', prefix: 'every' })
hits.map(hit => hit.id) // ['5', '7', '6']
```

```python
await courses.set_many([
    ("5", Course("Линейная алгебра")),
    ("6", Course("Алгоритмизация и программирование")),
    ("7", Course("Проектирование технологических линий")),
])
result = await courses.search("лин алг", prefix="every")
```

```go
err = courses.SetMany(ctx, []search.Doc[Course]{
	{ID: "5", Value: Course{"Линейная алгебра"}},
	{ID: "6", Value: Course{"Алгоритмизация и программирование"}},
	{ID: "7", Value: Course{"Проектирование технологических линий"}},
})
result, err := courses.Search(ctx, "лин алг", search.PrefixEvery())
```

```rust
let found = courses.search("лин алг").prefix_every().run()?;
```

`prefix: 'every' / prefix="every" / PrefixEvery / prefix_every` turns it on
for one search, in place of the last word alone. `Линейная алгебра` comes
first because it holds a word that starts with `лин` and one that starts with
`алг`. The other two courses each hold only one of them. With
`prefix: true`, `лин` is a whole word, so it finds only `линий` by its stem,
and `Проектирование технологических линий` comes first.

- Each word of two letters or more is read as the start of a word, whatever
  follows it. A space does not make it whole. Only the first eight are read
  this way, and the rest whole.
- A document holds a word of the text when it holds that word, its stem, or a
  longer word that starts with it.
- The documents that hold a word for each word of the text come first, and
  the rest come after them. BM25 orders the documents within each group.
- Scores still go down the page. Search adds to the score of each document in
  the first group the most that any document could score. As always, a score
  orders the hits of one search and means nothing beside another's.

## How a prefix ranks

The words a prefix begins count as one word in each field: a document holding
two of them counts both, as it would one word written twice. They weigh half
their field's weight, less than the stem of a Cyrillic word, which weighs
three quarters, so a document holding the word typed comes before one holding
only a longer word, unless that word is far rarer or its field far shorter.

Among the words a prefix begins, the shortest field ranks first, as BM25 ranks
any word: `cryptograph` finds `Cryptography`, then `Foundations of
cryptography`, then `Cryptographic methods of information security`.

## Typos and a prefix

A word typed halfway is no typo while it begins a word the index holds:
`cardi` is one letter from `cards`, and still finds `cardigan` rather than
being read as `cards`. A last word that begins nothing the index holds is
corrected as any word is, and [`corrected`](typos.md) says so:
`cryptograhpy` reads as `cryptography`.

A typo inside a prefix is not forgiven as a prefix: a last word that begins
nothing is read only as the whole word nearest it, under the [rules of
typos](typos.md#the-rules), and its prefix is then that word.

## Limits and defaults

| What                         | Value                                    |
|------------------------------|------------------------------------------|
| `prefix`                     | off unless the search says               |
| the shortest prefix          | two letters                              |
| words a prefix is read as    | the 64 most held in each field           |
| what a word it begins weighs | half its field's weight                  |
| words before the last        | whole, unless `prefix` is `'every'`      |
| prefixes with `'every'`      | the first 8 words of two letters or more |

A prefix reads the postings of every word it begins before the search scores,
up to the 64 above, so a short prefix on an index of many documents reads
more than a whole word would.

## See also

- [Search](README.md): the engine's overview.
- [Typos](typos.md) and [Keyboard layouts](layouts.md): the other ways a
  search reads what was typed.
- [Fields and weights](fields.md): how words are read.
- [How search finds](../../handbook/inside/how-search-finds.md) in the
  Handbook.
