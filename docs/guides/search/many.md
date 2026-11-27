# Ask several indexes at once

A "find anything" box asks one question of every index a program keeps:
courses, pages, people. `store.search` asks them all in one call, each index
with its own filter, limit and snippet, and answers a group of hits for each
index and one list of the best of all of them, merged by rank. In Search, it
replaces a loop of single searches, and over the wire it is one round trip.

```ts
const courses = store.index<Course>('courses', { fields: { title: 2, summary: 1 } })
const pages = store.index<Page>('pages', { fields: { title: 2, plain: 1 }, filters: ['space_id'] })
const people = store.index<Person>('people', { fields: { name: 1 } })

const found = await store.search({
	text: 'algebra',
	in: [
		{ index: courses, boost: 2, limit: 5 },
		{ index: pages, where: { space_id: 7 }, limit: 5, snippet: { field: 'plain', around: 60 } },
		{ index: people, limit: 5 },
	],
})
found.groups // [{ index: 'courses', hits, more: true }, { index: 'pages', hits, more: false }, …]
found.merged // [{ index: 'courses', group: 0, hit }, { index: 'pages', group: 1, hit }, …]
```

```python
courses = store.index("courses", Course, fields={"title": 2, "summary": 1})
pages = store.index("pages", Page, fields={"title": 2, "plain": 1}, filters=["space_id"])
people = store.index("people", Person, fields={"name": 1})

found = await store.search(
    "algebra",
    among=[
        search.Among(courses, boost=2, limit=5),
        search.Among(pages, where={"space_id": 7}, limit=5, snippet={"field": "plain", "around": 60}),
        search.Among(people, limit=5),
    ],
)
found.groups  # [ManyGroup(index='courses', hits=[…], more=True, …), …]
found.merged  # [Merged(index='courses', group=0, hit=Hit(…)), …]
```

```go
courses, err := search.OpenIndex[Course](store, "courses", search.Fields{"title": 2, "summary": 1})
pages, err := search.OpenIndex[Page](store, "pages", search.Fields{"title": 2, "plain": 1}, search.Filters("space_id"))
people, err := search.OpenIndex[Person](store, "people", search.Fields{"name": 1})

found, err := search.Many(ctx, store, "algebra",
	search.Among(courses, search.Boost(2), search.Limit(5)),
	search.Among(pages, search.Where{"space_id": 7}, search.Limit(5), search.Snippet("plain", 60)),
	search.Among(people, search.Limit(5)))
found.Groups // [{Index: "courses", Hits: […], More: true}, …]
found.Merged // [{Index: "courses", Group: 0, Hit: &found.Groups[0].Hits[0]}, …]
```

```rust
use tinystore::search::eq;

let courses = store.index::<Course>("courses").field("title", 2.0).field("summary", 1.0).open()?;
let pages = store.index::<Page>("pages").field("title", 2.0).field("plain", 1.0).filters(["space_id"]).open()?;
let people = store.index::<Person>("people").field("name", 1.0).open()?;

let found = store
    .search("algebra")
    .among(&courses, |a| a.boost(2.0).limit(5))
    .among(&pages, |a| a.filter(eq("space_id", &7)).limit(5).snippet("plain", 60))
    .among(&people, |a| a.limit(5))
    .run()?;
found.groups[0].more // true: courses has more than five
found.merged[0].hit.id // the best of all three, by rank
```

The text is the call's, and every index reads it as a search of that index
alone would: its own fields, typos, synonyms and layouts. The same index may
be asked twice, each time with another filter. A call asks 1 to 32 indexes.

## What each group says

`groups` holds one group for each index, in the order asked. A group has the
index's hits, the best by that index's own scores first, and `more`, which
says whether the index has hits past the group's limit. Search asks each
index for one hit more than the limit and drops it, so `more` is exact: a
"show all courses" link appears only when there are more courses.

A group also has `corrected` when its index read a misspelt word as its
nearest, as a [single search](typos.md) does. Each index corrects by its own
words, so one group may read `algebar` as `algebra` while another finds
nothing to correct.

Each hit's value is its document. In Python and Go, a group's hits are of its
index's type: `found.groups[0].hits[0].value` is a `Course`, and in Go
`found.Groups[0].Hits[0].Value.(Course)`. In Rust and in Bun and Node the
indexes of one call keep values of different types, so a hit's value is
JSON: a `serde_json::Value` in Rust, `unknown` in TypeScript. A [table's
index](table-index.md) answers among them too, each hit's value its row.

## An index that fails

An index that fails does not fail the others. Its group carries the error
and no hits, and every other group answers:

```ts
const found = await store.search({ text: 'algebra', in: [{ index: courses }, { index: dropped }] })
found.groups[1] // { index: 'dropped', error: InvalidError: index dropped: … }
```

```python
found = await store.search("algebra", among=[search.Among(courses), search.Among(dropped)])
found.groups[1].error  # InvalidError('index dropped: …')
```

```go
found, err := search.Many(ctx, store, "algebra", search.Among(courses), search.Among(dropped))
errors.Is(found.Groups[1].Err, tinystore.ErrInvalid) // true
```

```rust
let found = store.search("algebra").among(&courses, |a| a).among(&dropped, |a| a).run()?;
found.groups[1].error // Some(invalid: index dropped: …)
```

The error is of the same kind a search of that index alone would raise,
and names the index. A call fails only when every group fails, with the
first group's error. A call that asks no index, more than 32, a limit
outside 1 to 100 or a boost that is not a positive number is refused as
`invalid` before any index is searched, and so is an index of another store.

## Merged: by rank, not by score

`merged` holds every group's hits in one list. Each entry names its group, by
the index's name and the group's place in `groups`, and carries the same hit
as its group: in Bun, Python and Go the same object.

A score orders the hits of one index and means nothing beside another's,
since each index scores by the words of its own documents: a word rare in
courses and common in pages scores differently in each. So `merged` never
compares scores. It merges by reciprocal rank fusion: the hit at rank `r` of
its group, counted from 1, weighs `boost / (60 + r)`.

| Courses, `boost` 2 | weight         | Pages, `boost` 1 | weight         |
|--------------------|----------------|------------------|----------------|
| rank 1             | 2 / 61 = 0.033 | rank 1           | 1 / 61 = 0.016 |
| rank 2             | 2 / 62 = 0.032 | rank 2           | 1 / 62 = 0.016 |

Of equal weights the group asked first comes first, then the higher rank, so
with no boost the groups take turns: the first of each, then the second of
each. A boost of 2 puts a group's first hits before another's first, without
ever looking at how high either index scored them. `boost` is 1 unless said.

## One moment, or one file at a time

The store's own indexes live in one file, search.db, and one call searches
all of them in one snapshot of it: a write that commits while the call runs
reaches none of their groups or all of them. A [table's index](table-index.md)
lives in its database's file and is searched in a snapshot of that file, so
groups from different files are not one moment.

## Limits and defaults

| What                      | Value                                            |
|---------------------------|--------------------------------------------------|
| indexes a call asks       | 1 to 32; the same index may come twice           |
| a group's `limit`         | 1 to 100, 10 unless said                         |
| `boost`                   | a positive number, 1 unless said                 |
| an entry's own options    | `where`, `limit`, `prefix`, `matches`, `snippet` |
| an entry's text or vector | not taken: the text is the call's                |
| `offset` and `total`      | not taken: `more` says whether there are more    |
| `merged`                  | every group's hits, by reciprocal rank fusion    |

Over the wire, an entry that gives its own text, a vector, an offset or a
total is refused as `invalid`; the SDKs do not offer them.

## See also

- [Overview](README.md): an index, its documents and a search of one.
- [Filters](filters.md): what `where` takes.
- [Show what matched](matches.md): `matches` and snippets.
- [A table's index](table-index.md): a database's rows found by their words.
