# Full-text search

Search the text of your rows with SQLite's FTS5. Create the search index in a
migration, keep it up to date with triggers, and query it with plain SQL.
R*Tree, for searching by location or range, is available the same way.

## Turn it on in Go

```go
import (
	"github.com/tinyshed/tinystore/sqldb"
	_ "github.com/tinyshed/tinystore/sqldb/fts5"  // full-text search
	_ "github.com/tinyshed/tinystore/sqldb/rtree" // R*Tree and Geopoly
)
```

In Bun and Python, FTS5 and R*Tree are always there, because the `tinystore`
server includes them. In Go, each is a package that you import, so a program
that doesn't search doesn't carry their code. If a database has a virtual
table whose package your program doesn't import, `sqldb.Open` fails with an
invalid error (`ErrInvalid`) that names the import to add:

```text
sql "app": invalid request: messages_fts uses fts5, which the program did not link: import _ "github.com/tinyshed/tinystore/sqldb/fts5"
```

## Create the index

```sql title="migrations/004_search.sql"
create virtual table messages_fts using fts5(body, content = 'messages', content_rowid = 'id');

create trigger messages_fts_insert after insert on messages begin
  insert into messages_fts (rowid, body) values (new.id, new.body);
end;
create trigger messages_fts_delete after delete on messages begin
  insert into messages_fts (messages_fts, rowid, body) values ('delete', old.id, old.body);
end;
create trigger messages_fts_update after update on messages begin
  insert into messages_fts (messages_fts, rowid, body) values ('delete', old.id, old.body);
  insert into messages_fts (rowid, body) values (new.id, new.body);
end;
```

The index stores no copy of the text: `content = 'messages'` makes it read the
text from the `messages` table. The triggers keep the index in step with every
insert, update and delete, inside the same transaction.

## Search

```ts
const hits = await db.all<Message>`
	select m.* from messages_fts f join messages m on m.id = f.rowid
	where messages_fts match ${query} and m.chat_id = ${chatId}
	order by bm25(messages_fts) limit 50`
```

```python
hits = await db.all(
    Message,
    """select m.* from messages_fts f join messages m on m.id = f.rowid
       where messages_fts match ? and m.chat_id = ?
       order by bm25(messages_fts) limit 50""",
    query, chat_id,
)
```

```go
hits, err := sqldb.All[Message](ctx, db, `
	select m.* from messages_fts f join messages m on m.id = f.rowid
	where messages_fts match ? and m.chat_id = ?
	order by bm25(messages_fts) limit 50`, query, chatID)
```

`match` takes FTS5's query syntax: words, `"exact phrases"`, `prefix*`, and
`AND`, `OR` and `NOT`. `bm25` ranks the best matches first. You can combine a
search with any other condition of your tables, such as a chat id.

## Languages

The default `unicode61` tokenizer ignores case in every alphabet, including
Cyrillic, but it doesn't know word forms: a search for `message` doesn't find
`messages`. Use prefix queries, `messag*`, or the `porter` tokenizer for
English, `fts5(body, tokenize = 'porter unicode61')`.

## Schemas and backups

A virtual table and its triggers belong to your migrations. The schema check
in Go doesn't compare them, and they are copied with the database in every
[backup](../running/backups.md).

## See also

- [SQL](README.md): migrations and queries.
- [SQLite's FTS5 documentation](https://www.sqlite.org/fts5.html): the full
  query syntax and tokenizers.
