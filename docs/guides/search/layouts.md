# Keyboard layouts

People who write in two alphabets switch keyboard layouts all day, and often
type a word with the wrong one on: `rhbgnj` for `крипто`, or `знерщт` for
`python`. Every letter is wrong, so no typo correction finds it. With
`layouts`, Search reads such a word as the same keys type in the other
layout, and `corrected` says what it read, so the page can show "Showing
results for крипто".

```ts
const courses = store.index<{ name: string }>('courses', { fields: { name: 1 }, layouts: ['ru', 'en'] })
await courses.setMany([
	{ id: 1, value: { name: 'Криптографические методы защиты информации' } },
	{ id: 2, value: { name: 'Python для анализа данных' } },
	{ id: 3, value: { name: 'Линейная алгебра' } },
])
const { hits, corrected } = await courses.search({ text: 'rhbgnj', prefix: true })
hits.map(hit => hit.id) // ['1']
corrected // 'крипто'
```

```python
courses = store.index("courses", Course, fields={"name": 1}, layouts=["ru", "en"])
await courses.set_many([
    ("1", Course("Криптографические методы защиты информации")),
    ("2", Course("Python для анализа данных")),
    ("3", Course("Линейная алгебра")),
])
result = await courses.search("rhbgnj", prefix=True)
result.corrected  # 'крипто'
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"name": 1}, search.Layouts("ru", "en"))
result, err := courses.Search(ctx, "rhbgnj", search.Prefix())
result.Corrected // "крипто"
```

```rust
let courses = store.index::<Course>("courses").field("name", 1.0).layouts(["ru", "en"]).open()?;
let found = courses.search("rhbgnj").prefix().run()?;
found.corrected // Some("крипто")
```

`layouts / layouts / Layouts / layouts` names the two layouts that the
index's readers type with, `ru` and `en` today. The search reads the text in
the other one.

## Which words are read in the other layout

Search splits the text at spaces and looks at each piece on its own:

| Typed        | Read as      | Why                                                           |
|--------------|--------------|---------------------------------------------------------------|
| `rhbgnj`     | `крипто`     | the QWERTY keys of `крипто`, which begins a course's word     |
| `знерщт`     | `python`     | the ЙЦУКЕН keys of `python`                                   |
| `Python lkz` | `python для` | `python` is found as typed, and `lkz` is read as `для`        |
| `ghbdtn`     | `ghbdtn`     | found as typed, in an index that holds `ghbdtn`               |
| `rhbgnj`     | `rhbgnj`     | without `prefix`, `крипто` is a whole word that no name holds |

- A piece is read in the other layout only when none of its words finds
  anything as typed. A word that finds something is never changed (contract
  SRCH-12), so a hit as typed ranks as it did.
- The reading is kept only when every word it gives finds something: a word
  that the index holds, or, for the last word of a [`prefix`](prefix.md)
  search, the start of one. Otherwise the piece stays as typed.
- A key keeps its place, letters and marks alike: `by;` is read as `инж`,
  because `ж` is on the `;` key, and `[kt,` as `хлеб`.
- A piece of two scripts, such as `pythonдля`, or one without a letter, such
  as `1.5`, is never read in another layout.

## Several words

Search checks each piece on its own. With `prefix`, only the last word is
read as the start of a word, so `kby fku`, the QWERTY keys of `лин алг`, reads
as `kby алг`. The index holds no word `лин`, so `kby` stays as typed, and
`алг` begins `алгебра`. With [`prefix: 'every'`](prefix.md#every-word), every
word is read as the start of a word, so `kby fku` reads as `лин алг`.

```ts
const found = await courses.search({ text: 'kby fku', prefix: true })
found.corrected // 'kby алг'
```

```python
result = await courses.search("kby fku", prefix=True)
result.corrected  # 'kby алг'
```

```go
result, err := courses.Search(ctx, "kby fku", search.Prefix())
result.Corrected // "kby алг"
```

```rust
let found = courses.search("kby fku").prefix().run()?;
found.corrected // Some("kby алг")
```

## Typos and layouts

Search reads the layouts first and corrects typos after. A word read in the
other layout finds something, so it is never corrected too. A piece whose
reading finds nothing stays as typed and is corrected as any word is, under
the [rules of typos](typos.md#the-rules). `corrected` shows both:
`informatoin rhbgnj` reads as `information крипто`.

Layouts work with `typoTolerance` turned off as well. Opening the index with
other layouts, or without them, applies at once, as
[What each change costs](rebuilds.md#what-each-change-costs) lists.

## Limits and defaults

| What                     | Value                                  |
|--------------------------|----------------------------------------|
| `layouts`                | none unless the index says             |
| the layouts known        | `ru` (ЙЦУКЕН) and `en` (QWERTY)        |
| how many an index names  | two, or none                           |
| a name that is not known | refused at open, with an invalid error |
| a word found as typed    | never read in another layout           |

Other pairs, such as Ukrainian or German beside English, are not built yet.

## See also

- [Typos](typos.md): the nearest word, and `corrected`.
- [As you type](prefix.md): the last word read as the start of others.
- [Search](README.md): the engine's overview.
