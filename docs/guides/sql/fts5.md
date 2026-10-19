# FTS5 and R*Tree

A database is a plain SQLite file, built with SQLite's own full-text index,
FTS5, and its spatial index, R*Tree. Both are virtual tables a migration
creates and SQL queries; TinyStore adds nothing to them and takes nothing
away.

## Full text with FTS5

An FTS5 table that indexes an ordinary table, kept in step by triggers:

```sql title="migrations/0001_notes.sql"
create table notes (
    id    integer primary key,
    title text not null,
    body  text not null
) strict;

create virtual table notes_fts using fts5 (
    title, body,
    content = 'notes', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 2'
);

create trigger notes_ai after insert on notes begin
    insert into notes_fts (rowid, title, body) values (new.id, new.title, new.body);
end;
create trigger notes_ad after delete on notes begin
    insert into notes_fts (notes_fts, rowid, title, body) values ('delete', old.id, old.title, old.body);
end;
create trigger notes_au after update on notes begin
    insert into notes_fts (notes_fts, rowid, title, body) values ('delete', old.id, old.title, old.body);
    insert into notes_fts (rowid, title, body) values (new.id, new.title, new.body);
end;
```

`content = 'notes'` keeps the text once, in `notes`; the FTS5 table holds
only its index. The triggers run inside each write's own savepoint, so a row
and its index entry commit together, and a write from `tinystore sql --write`
or another program reaches the index too.

```ts
const found = await db.all<{ id: number; title: string; excerpt: string }>`
	select n.id, n.title, snippet(notes_fts, 1, '<b>', '</b>', '…', 8) as excerpt
	from notes_fts join notes n on n.id = notes_fts.rowid
	where notes_fts match ${phrase(query)}
	order by bm25(notes_fts, 2.0, 1.0)
	limit 20`
// [{ id: 1, title: 'Café in Lisbon', excerpt: 'The best pastel de nata is near the…' }]
```

```python
found = await db.all(
    """select n.id, n.title, snippet(notes_fts, 1, '<b>', '</b>', '…', 8) as excerpt
    from notes_fts join notes n on n.id = notes_fts.rowid
    where notes_fts match ?
    order by bm25(notes_fts, 2.0, 1.0)
    limit 20""",
    phrase(query),
)
```

```go
type Found struct {
	ID      int64
	Title   string
	Excerpt string
}

found, err := sqldb.All[Found](ctx, db, `
	select n.id, n.title, snippet(notes_fts, 1, '<b>', '</b>', '…', 8) as excerpt
	from notes_fts join notes n on n.id = notes_fts.rowid
	where notes_fts match ?
	order by bm25(notes_fts, 2.0, 1.0)
	limit 20`, phrase(query))
```

```rust
#[derive(Deserialize)]
struct Found {
    id: i64,
    title: String,
    excerpt: String,
}

let found: Vec<Found> = db.all(sql!(
    "select n.id, n.title, snippet(notes_fts, 1, '<b>', '</b>', '…', 8) as excerpt
     from notes_fts join notes n on n.id = notes_fts.rowid
     where notes_fts match ?
     order by bm25(notes_fts, 2.0, 1.0)
     limit 20",
    phrase(&query)
))?;
```

`bm25(notes_fts, 2.0, 1.0)` ranks a word in the title twice a word in the
body; a lower score is better, so it orders ascending. `snippet` cuts the
words around a match, and `highlight` marks every match in a column.

## A user's words are not a query

`match` reads FTS5's own query language: `AND`, `OR`, `NOT`, `"phrases"`,
`prefix*`, parentheses. A user who types `wine AND (` gets a syntax error.
Quote each word, so that the user's text is only words:

```ts
const phrase = (text: string) =>
	text.split(/\s+/).filter(Boolean).map(word => `"${word.replaceAll('"', '""')}"`).join(' ')
phrase('cafe lisbon') // '"cafe" "lisbon"': both words, anywhere
```

```python
def phrase(text: str) -> str:
    return " ".join('"' + word.replace('"', '""') + '"' for word in text.split())
```

```go
func phrase(text string) string {
	words := strings.Fields(text)
	for i, word := range words {
		words[i] = `"` + strings.ReplaceAll(word, `"`, `""`) + `"`
	}
	return strings.Join(words, " ")
}
```

```rust
fn phrase(text: &str) -> String {
    text.split_whitespace().map(|word| format!("\"{}\"", word.replace('"', "\"\""))).collect::<Vec<_>>().join(" ")
}
```

Add a `*` after the last quoted word for search as you type: `"lisb"*`.

## Tokenizers

| `tokenize =`                    | Reads                                                          |
|---------------------------------|----------------------------------------------------------------|
| `unicode61 remove_diacritics 2` | words by Unicode's letters, accents off: `café` finds `cafe`   |
| `porter unicode61`              | English words by their stem: `running` finds `run`             |
| `trigram`                       | every three letters: any substring, any language, larger index |

The tokenizer is set when the table is created. Changing it is a migration
that creates the table again and fills it with
`insert into notes_fts (notes_fts) values ('rebuild')`.

## FTS5 or the Search engine?

| You need                                                     | Use                                                                     |
|--------------------------------------------------------------|-------------------------------------------------------------------------|
| snippets, highlights, phrases, `NEAR`, a query language      | FTS5                                                                    |
| typos forgiven, Russian stems, Chinese and Japanese words    | [Search](../search/README.md)                                           |
| vectors from your model, alone or with words                 | [Search](../search/README.md)                                           |
| filters that keep each user to their own rows before ranking | [Search](../search/README.md)                                           |
| the index in the same file and commit as the rows            | either: FTS5's triggers, or [a table's index](../search/table-index.md) |

## Places with R*Tree

An R*Tree indexes boxes, ranges of numbers in up to five dimensions, and
finds those that overlap a box: the places on the map the user is looking at.

```sql title="migrations/0002_places.sql"
create table places (
    id   integer primary key,
    name text not null,
    lat  real not null,
    lon  real not null
) strict;

create virtual table places_box using rtree (id, min_lat, max_lat, min_lon, max_lon);

create trigger places_ai after insert on places begin
    insert into places_box values (new.id, new.lat, new.lat, new.lon, new.lon);
end;
create trigger places_ad after delete on places begin
    delete from places_box where id = old.id;
end;
```

```ts
const visible = await db.all<{ name: string }>`
	select p.name from places_box b join places p on p.id = b.id
	where b.min_lat >= ${south} and b.max_lat <= ${north}
	  and b.min_lon >= ${west} and b.max_lon <= ${east}`
```

```python
visible = await db.all(
    """select p.name from places_box b join places p on p.id = b.id
    where b.min_lat >= ? and b.max_lat <= ? and b.min_lon >= ? and b.max_lon <= ?""",
    south, north, west, east,
)
```

```go
visible, err := sqldb.All[string](ctx, db, `
	select p.name from places_box b join places p on p.id = b.id
	where b.min_lat >= ? and b.max_lat <= ? and b.min_lon >= ? and b.max_lon <= ?`,
	south, north, west, east)
```

```rust
let visible: Vec<String> = db.all(sql!(
    "select p.name from places_box b join places p on p.id = b.id
     where b.min_lat >= ? and b.max_lat <= ? and b.min_lon >= ? and b.max_lon <= ?",
    south,
    north,
    west,
    east
))?;
```

An R*Tree keeps its coordinates as 32-bit floats, rounded outwards: a box it
returns may reach a few metres past the one asked for. Compare `p.lat` and
`p.lon` too when the edge matters.

## Maintenance

FTS5 merges its segments as it writes. After a large import, one statement
merges them all:

```sql
insert into notes_fts (notes_fts) values ('optimize');
```

It is a write like any other: `db.exec` runs it in a shared commit.

## See also

- [SQL](README.md): the engine's overview.
- [Migrations](migrations.md): where virtual tables are created.
- SQLite's own [FTS5](https://sqlite.org/fts5.html) and
  [R*Tree](https://sqlite.org/rtree.html) documentation.
