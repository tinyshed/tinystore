# Schema in Go

In Go, you can declare your tables with structs. TinyStore prints the SQL
those tables need, checks in a test that your migrations create exactly that
schema, writes the next migration when you change a struct, and checks the
database file against the schema when your program starts. This page is Go
only: Bun and Python programs write their migrations as `.sql` files.

## Declare a table

```go
type Note struct {
	ID        uuid.UUID            // TEXT, 8-4-4-4-12
	AuthorID  int64
	Title     string
	Done      bool                 // INTEGER, 0 or 1
	Tags      sqldb.JSON[[]string] // TEXT, valid JSON
	Due       *sqldb.Date          // TEXT YYYY-MM-DD, or NULL
	CreatedAt time.Time            // INTEGER, unix milliseconds
}

var Notes = sqldb.Table[Note]("notes",
	sqldb.PrimaryKey("id"),
	sqldb.References("author_id", Users, sqldb.Cascade),
	sqldb.Default("done", false),
	sqldb.Index("author_id", "created_at"),
	sqldb.Check("length(title) > 0"),
)

var Schema = sqldb.Schema(Users, Notes)
```

A struct's fields are the table's columns, in order: `CreatedAt` becomes
`created_at`. A pointer or `sql.Null[T]` can be `NULL`, and every other field
is `NOT NULL`. `Table` adds what a struct can't say: keys, references,
defaults, indexes and checks.

Tags are only for exceptions: `db:"name"` renames a column, `db:"-"` leaves a
field out, and `db:",generated"` marks a column that the database fills, such
as an `INTEGER PRIMARY KEY`.

A declaration that can't be a table, such as an option that names a missing
column, panics when the program starts, with the table and the column in the
message.

## Indexes on part of a table

```go
var Users = sqldb.Table[User]("users",
	sqldb.PrimaryKey("id"),
	sqldb.UniqueWhere("role = 'owner'", "role"), // at most one owner
)

var Sources = sqldb.Table[Source]("sources",
	sqldb.UniqueWhere("agent_id IS NOT NULL", "agent_id", "remote_id"),
	sqldb.IndexWhere("deleted_at IS NULL", "created_at"),
)
```

```sql
CREATE UNIQUE INDEX users_role ON users (role) WHERE role = 'owner';
```

`UniqueWhere` and `IndexWhere` declare a partial index: an index of the rows
where the condition holds. A unique one is a promise of the database, such as
one owner per installation, and the schema checks it like any other index. A
file without it, or with an index of every row instead, fails to open. A
condition spelled differently, such as `ROLE = 'owner'`, doesn't stop the file
from opening. The schema test reports it, so you can check that the meaning is
the same.

## See the SQL

```go
fmt.Println(Schema.SQL())
```

```sql
CREATE TABLE notes (
    id         TEXT NOT NULL PRIMARY KEY,
    author_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
    tags       TEXT NOT NULL CHECK (json_valid(tags)),
    due        TEXT CHECK (due IS date(due)),
    created_at INTEGER NOT NULL,
    CHECK (length(title) > 0)
) STRICT;
CREATE INDEX notes_author_id_created_at ON notes (author_id, created_at);
```

Every table is `STRICT`, and the checks keep a boolean a boolean and JSON
valid. The same SQL is printed by `go tool tinystore schema app`.

## Check the migrations in a test

```go
// internal/data/schema_test.go
func TestSchema(t *testing.T) {
	sqldbtest.CheckSchema(t, "app", Schema, "migrations")
}
```

The test applies your migrations to a new database and compares the result
with the schema. When they differ, it fails and tells you what is missing:

```text
--- FAIL: TestSchema (0.02s)
    sql "app": the schema declares what the migrations do not make:
      + notes.description TEXT
    write it with: go tool tinystore migrate app new <what>
```

## Generate the next migration

```text
$ go get -tool github.com/tinyshed/tinystore/cmd/tinystore

$ go tool tinystore migrate app new add_description
wrote internal/data/migrations/002_add_description.sql:

    ALTER TABLE notes ADD COLUMN description TEXT;
```

The tool runs your own test to compute the difference, so it always uses the
sqldb version of your module. A test never writes files: only the tool does.

When a change is ambiguous, the tool writes a draft that doesn't run until you
finish it. A column removed next to a new one may be a rename, which keeps the
data, or a drop, which loses it. A changed type requires rebuilding the table,
and the draft marks where each old value must be converted:

```sql
-- REVIEW: SQLite cannot make this change in place; notes is rebuilt:
--   notes.status is INTEGER in the schema and TEXT in the migrations
CREATE TABLE notes_new ( … status INTEGER NOT NULL … ) STRICT;
INSERT INTO notes_new ( … ) SELECT …, /* TODO: how does each old status, TEXT, become INTEGER? */ FROM notes;
```

## The program checks the file

```go
db, err := sqldb.Open(ctx, store, "app", migrations, Schema)
```

`Open` applies pending migrations, then compares the file with the schema:
tables, columns and their types, nullability, keys, references and indexes,
partial ones included. If they differ, `Open` fails and lists every
difference. It never changes the file to match. Triggers, views, indexes on
expressions and partial indexes that the schema doesn't declare belong to your
migrations and are not compared.

A migration that was already applied must never change. If its checksum
differs, or a migration is missing or renamed, `Open` refuses the file.

## Insert a row

```go
note, err := sqldb.Insert(ctx, db, Notes, Note{
	ID:        uuid.NewV7(),
	AuthorID:  user.ID,
	Title:     "Buy milk",
	Tags:      sqldb.JSONOf([]string{"home"}),
	CreatedAt: store.Now(),
})
```

`Insert` writes one row from a struct, every field except the generated ones,
and returns the row as the database stored it. Everything else, updates and
deletes included, is SQL.

Use UUID version 7 for keys: its first bits are a timestamp, so new rows land
next to each other in the index. Inserting 500,000 rows was 5 to 6 times
faster than with random version 4 UUIDs, on a Ryzen 7 7700 with an NVMe disk
([report](https://github.com/tinyshed/research/blob/main/tinystore/reports/sqldb-mechanics-2026-09-28.md)).

A `uuid.UUID` is stored as text, `8-4-4-4-12` in lower case. This works for
the standard library's `uuid` package, which Go 1.27 added, and for
`github.com/google/uuid` and `github.com/gofrs/uuid`. A UUID passed as a query
argument is written as text too, so `where id = ?` finds the row.

> [!WARNING]
> **A UUID stored as bytes**
> `sqldb.Storage("id", sqldb.Blob)` stores a UUID column as 16 bytes instead
> of text. A UUID passed as a query argument is still written as text, so
> `where id = ?` finds nothing. Pass its bytes instead: `id[:]`.

## See also

- [Writes and transactions](writes.md): grouped commits and constraint
  errors.
- [sqldb/README.md](../../sqldb/README.md): the full contract of schemas.
