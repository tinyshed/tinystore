# Show what matched

A search box shows each result with the words that matched in bold. With
`matches`, Search says where its words matched each hit: the field, the range
of its text, and the query's word that found it. The ranges come from the
index itself, so a word found by its stem, by a prefix or after a typo fix is
marked too.

```ts
const courses = store.index<{ name: string; short: string }>('courses', { fields: { name: 2, short: 1 } })
await courses.setMany([
	{ id: 1, value: { name: 'Линейная алгебра и аналитическая геометрия', short: 'ЛААГ' } },
	{ id: 2, value: { name: 'Криптографические методы защиты информации', short: 'КМЗИ' } },
])
const { hits } = await courses.search({ text: 'алгебре', matches: true })
hits[0].matches // [{ field: 'name', start: 9, end: 16, word: 'алгебре' }]
hits[0].value.name.slice(9, 16) // 'алгебра': found by its stem
```

```python
courses = store.index("courses", Course, fields={"name": 2, "short": 1})
await courses.set_many([
    ("1", Course("Линейная алгебра и аналитическая геометрия", "ЛААГ")),
    ("2", Course("Криптографические методы защиты информации", "КМЗИ")),
])
result = await courses.search("алгебре", matches=True)
result.hits[0].matches  # [Match(field='name', start=9, end=16, word='алгебре', item=None)]
result.hits[0].value.name[9:16]  # 'алгебра': found by its stem
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"name": 2, "short": 1})
err = courses.SetMany(ctx, []search.Doc[Course]{
	{ID: "1", Value: Course{Name: "Линейная алгебра и аналитическая геометрия", Short: "ЛААГ"}},
	{ID: "2", Value: Course{Name: "Криптографические методы защиты информации", Short: "КМЗИ"}},
})
result, err := courses.Search(ctx, "алгебре", search.Matches())
result.Hits[0].Matches // [{Field: "name", Start: 17, End: 31, Word: "алгебре"}]
result.Hits[0].Value.Name[17:31] // "алгебра": found by its stem
```

```rust
let courses = store.index::<Course>("courses").field("name", 2.0).field("short", 1.0).open()?;
let algebra = Course { name: "Линейная алгебра и аналитическая геометрия".into(), short: "ЛААГ".into() };
let crypto = Course { name: "Криптографические методы защиты информации".into(), short: "КМЗИ".into() };
courses.set_many([(1, &algebra), (2, &crypto)])?;
let found = courses.search("алгебре").matches().run()?;
let hit = &found.hits[0];
hit.matches[0].at // 17..31, bytes of hit.value.name
&hit.value.name[hit.matches[0].at.clone()] // "алгебра": found by its stem
```

The query asked for `алгебре`, and the course holds `алгебра`. Both have the
stem `алгебр`, so Search found the course, and the range marks the whole word
`алгебра`. A plain substring search would mark nothing here.

`matches` is off unless the search asks for it. Without it, a hit's
`matches` is `undefined` in Bun and Node, `None` in Python, `nil` in Go and
empty in Rust.

## Bold what matched

The ranges of one field come in the order of its text and never overlap, so
one pass over them builds the marked text:

```ts
import type { SearchMatch } from '@tinyshed/tinystore'

function bold(text: string, matches: SearchMatch[]): string {
	let out = ''
	let at = 0
	for (const { start, end } of matches) {
		out += `${text.slice(at, start)}<b>${text.slice(start, end)}</b>`
		at = end
	}
	return out + text.slice(at)
}

const found = await courses.search({ text: 'криптографичесике методы', matches: true })
for (const hit of found.hits) {
	const name = (hit.matches ?? []).filter(match => match.field === 'name')
	console.log(bold(hit.value.name, name)) // <b>Криптографические</b> <b>методы</b> защиты информации
}
```

```python
def bold(text: str, matches: list[tinystore.Match]) -> str:
    out, at = "", 0
    for match in matches:
        out += f"{text[at:match.start]}<b>{text[match.start:match.end]}</b>"
        at = match.end
    return out + text[at:]


result = await courses.search("криптографичесике методы", matches=True)
for hit in result.hits:
    name = [match for match in hit.matches or [] if match.field == "name"]
    print(bold(hit.value.name, name))  # <b>Криптографические</b> <b>методы</b> защиты информации
```

```go
func bold(text string, matches []search.Match) string {
	var out strings.Builder
	at := 0
	for _, m := range matches {
		out.WriteString(text[at:m.Start] + "<b>" + text[m.Start:m.End] + "</b>")
		at = m.End
	}
	return out.String() + text[at:]
}

result, err := courses.Search(ctx, "криптографичесике методы", search.Matches())
for _, hit := range result.Hits {
	var name []search.Match
	for _, m := range hit.Matches {
		if m.Field == "name" {
			name = append(name, m)
		}
	}
	fmt.Println(bold(hit.Value.Name, name)) // <b>Криптографические</b> <b>методы</b> защиты информации
}
```

```rust
fn bold(text: &str, matches: &[&Match]) -> String {
    let (mut out, mut at) = (String::new(), 0);
    for each in matches {
        out += &format!("{}<b>{}</b>", &text[at..each.at.start], &text[each.at.clone()]);
        at = each.at.end;
    }
    out + &text[at..]
}

let found = courses.search("криптографичесике методы").matches().run()?;
for hit in &found.hits {
    let name: Vec<&Match> = hit.matches.iter().filter(|each| each.field == "name").collect();
    println!("{}", bold(&hit.value.name, &name)); // <b>Криптографические</b> <b>методы</b> защиты информации
}
```

If the text goes into HTML, escape each piece rather than the whole text
first, because escaping changes the offsets.

## What a range covers

| The search                 | The text stored            | The range marks                                      |
|----------------------------|----------------------------|------------------------------------------------------|
| `алгебре`                  | `Линейная алгебра`         | `алгебра`, the whole word, found by its stem         |
| `крипто` with `prefix`     | `Криптографические методы` | `Крипто`, the letters typed                          |
| `криптографичесике методы` | `Криптографические методы` | `Криптографические` and `методы`, after the typo fix |
| `cafe`                     | `CAFÉ in Paris`            | `CAFÉ`, whatever its case and accents                |

- `word` is the query's word as the search's text wrote it, before any typo
  fix: `криптографичесике` above, while [`corrected`](typos.md) says
  `криптографические`.
- A word that matches in several places gives a range for each place.
- With [`prefix`](prefix.md), a word that the last word begins is marked only
  by the letters typed. A word that the reader typed whole is marked whole.
- Ranges that overlap are joined into one, named by the word of the first.
  This happens to the overlapping pairs that Chinese, Japanese and Thai text
  is read in: `東京タワー` is marked as one range.

The ranges point into the text as you stored it, not as the index
[read it](fields.md#words-in-any-language). So `Ёлка`, `CAFÉ` and `ＴＯＫＹＯ`
are marked whole, even where they take more or fewer characters than the
words the index read.

## Each language counts its own way

Each SDK asks the core for the unit its language slices strings in, so you
never convert a range:

| Language     | `start` and `end` count | The matched text         |
|--------------|-------------------------|--------------------------|
| Bun and Node | UTF-16 code units       | `text.slice(start, end)` |
| Python       | code points             | `text[start:end]`        |
| Go           | bytes                   | `text[m.Start:m.End]`    |
| Rust         | bytes                   | `&text[m.at.clone()]`    |

In `🦀 Ёлка`, the word `Ёлка` is 3 to 7 in Bun and Node, 2 to 6 in Python,
and 5 to 13 in Go and Rust: the emoji is two UTF-16 units, one code point and
four bytes.

## Lists, nested fields and plain text

`field` is the path the index reads, such as `author.name` for a field of a
field. When the field holds a list of strings, `item` is the place of the
string in the list:

```ts
const books = store.index<{ tags: string[] }>('books', { fields: ['tags'] })
await books.set('b-1', { tags: ['Алгебра', 'геометрия'] })
const { hits } = await books.search({ text: 'алгебра', matches: true })
hits[0].matches // [{ field: 'tags', item: 0, start: 0, end: 7, word: 'алгебра' }]
```

```python
books = store.index("books", Book, fields=["tags"])
await books.set("b-1", Book(tags=["Алгебра", "геометрия"]))
result = await books.search("алгебра", matches=True)
result.hits[0].matches  # [Match(field='tags', start=0, end=7, word='алгебра', item=0)]
```

```go
books, err := search.OpenIndex[Book](store, "books", search.FieldNames("tags"))
err = books.Set(ctx, "b-1", Book{Tags: []string{"Алгебра", "геометрия"}})
result, err := books.Search(ctx, "алгебра", search.Matches())
result.Hits[0].Matches // [{Field: "tags", Item: 0, Start: 0, End: 14, Word: "алгебра"}]
```

```rust
let books = store.index::<Book>("books").fields(["tags"]).open()?;
books.set("b-1", &Book { tags: vec!["Алгебра".into(), "геометрия".into()] })?;
let found = books.search("алгебра").matches().run()?;
found.hits[0].matches[0].item // Some(0)
```

`item` is absent for a field that holds one string. In Go, `Item` is 0 there.
An index of documents that are strings, with no fields, reads the document
itself: its matches have the field `''`.

## A table's index

In a [table's index](table-index.md), `field` is the column, and the range is
in the column's text as the row holds it. Search reads the row's columns in
the same snapshot as the search, inside a transaction too, so the ranges fit
the row that the hit carries.

## What is not marked

- A search by a vector alone marks nothing: each hit's `matches` is empty. A
  search by words and a vector marks the words.
- A `where` marks nothing, because a filter is not a word of the search.
- If Search cannot place the letters of a text back in the stored text, it
  gives no range for that text rather than a wrong one (contract SRCH-14). No
  text known does this.

## Limits and defaults

| What               | Value                                          |
|--------------------|------------------------------------------------|
| `matches`          | off unless the search asks                     |
| ranges in one text | in text order; ranges that overlap are joined  |
| a prefix's range   | the letters typed, not the whole word          |
| the work it adds   | each hit's text is read again after the search |

## See also

- [As you type](prefix.md): `prefix`, the last word as the start of others.
- [Typos](typos.md): the nearest word, and `corrected`.
- [Fields and weights](fields.md): which text is read.
- [A table's index](table-index.md): rows found by their words.
