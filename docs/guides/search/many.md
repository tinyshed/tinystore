# Ask several indexes at once

A "find anything" box searches every index that a program keeps, for example
courses, pages and people. `store.search` searches them all in one call, and
each index gets its own filter, limit and snippet. The result contains a group
of hits for each index, and one list of the best hits from all of them, merged
by rank. It replaces a loop of single searches, and over the wire it takes one
round trip.

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

The call has one text. Each index reads it the same way a search of that
index alone would, with its own fields, typos, synonyms and layouts. You can
include the same index twice, for example with a different filter each time.
A call searches 1 to 32 indexes.

## What each group says

`groups` contains one group for each index, in the order you listed them. A
group contains the index's hits, sorted best first by that index's own
scores. It also has `more`, which tells you whether the index has more hits
than the group's limit. Search asks each index for one hit more than the
limit and drops the extra one, so `more` is exact. For example, a "show all
courses" link appears only when there are more courses.

A group also has `corrected` when its index replaced a misspelt word with the
nearest word it knows, as a [single search](typos.md) does. Each index
corrects against its own words. So one group may read `algebar` as `algebra`,
while another group finds nothing to correct.

Each hit's value is its document. In Python and Go, the hits of a group have
the type of its index. In Python, `found.groups[0].hits[0].value` is a
`Course`, and in Go you get it as `found.Groups[0].Hits[0].Value.(Course)`.
In Rust, Bun and Node, the indexes of one call can store values of different
types, so a hit's value is JSON: a `serde_json::Value` in Rust and `unknown`
in TypeScript. You can include a [table's index](table-index.md) in the call
too, and each of its hits has the row as its value.

## An index that fails

If one index fails, the other groups still get their hits. The failed group
contains the error and no hits:

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

The error has the same kind that a search of that index alone would raise,
and it identifies the index. The whole call fails only when every group
fails, and then it fails with the first group's error. It also fails with
`cancelled` when it is cancelled or runs past its timeout, no matter which
group it has reached.

Before it searches any index, the call fails with `invalid` in these cases:

- It lists no index, or more than 32.
- A limit is outside 1 to 100.
- A boost is not a positive number.
- An index belongs to another store.

## Merged: by rank, not by score

`merged` contains the hits of every group in one list. Each entry identifies
its group by the index's name and by the group's position in `groups`. Each
entry carries the same hit as its group, and in Bun, Python and Go it is the
same object.

A score orders the hits of one index, but you can't compare it with the
scores of another index. Each index scores by the words of its own
documents, so a word that is rare in courses and common in pages gets a
different score in each. So `merged` never compares scores. Instead it uses
reciprocal rank fusion: the hit at rank `r` of its group, counted from 1,
gets the weight `boost / (60 + r)`.

| Courses, `boost` 2 | weight         | Pages, `boost` 1 | weight         |
|--------------------|----------------|------------------|----------------|
| rank 1             | 2 / 61 = 0.033 | rank 1           | 1 / 61 = 0.016 |
| rank 2             | 2 / 62 = 0.032 | rank 2           | 1 / 62 = 0.016 |

When two hits have the same weight, the hit from the group listed earlier
comes first, and then the hit with the higher rank. So with no boost, the
groups take turns: first the top hit of each group, then the second hit of
each. A boost of 2 puts a group's top hits before the top hits of another
group, and it never looks at how high either index scored them. `boost` is 1
by default.

## One moment, or one file at a time

The store's own indexes all live in one file, search.db. One call searches
all of them in one snapshot of that file. So if a write commits while the
call runs, either all their groups see it or none of them do. A [table's
index](table-index.md) lives in its database's file, and the call searches
it in a snapshot of that file. So groups from different files can show
different moments.

## Limits and defaults

| What                      | Value                                            |
|---------------------------|--------------------------------------------------|
| indexes per call          | 1 to 32, and the same index can appear twice     |
| a group's `limit`         | 1 to 100, 10 by default                          |
| `boost`                   | a positive number, 1 by default                  |
| an entry's own options    | `where`, `limit`, `prefix`, `matches`, `snippet` |
| an entry's text or vector | not accepted: the call has one text              |
| `offset` and `total`      | not accepted: `more` tells you if there are more |
| `merged`                  | every group's hits, by reciprocal rank fusion    |

Over the wire, an entry that sets its own text, a vector, an offset or a
total fails with `invalid`. The SDKs don't offer these options.

## See also

- [Overview](README.md): an index, its documents, and how to search one.
- [Filters](filters.md): what `where` accepts.
- [Show what matched](matches.md): `matches` and snippets.
- [A table's index](table-index.md): find a database's rows by their words.
