# sqldb: the application's own SQLite

Built on 28 September 2026, from this design as it was agreed that day. [The
mechanics round](reports/sqldb-mechanics-2026-09-28.md) measured what lies
under it, on Windows and in the container, and set the bounds and internals
below; [the engine round](reports/sqldb-engine-2026-09-28.md) measured what was
built. The contract as built is [sqldb/README.md](../sqldb/README.md). Building
it changed the surface in two names and one return, and the internals in a few
places; [what building it settled](#what-building-it-settled) says each and
why.

## What it is for

The application's relational data in `sql/<name>.db`: its tables, joins,
reports and transactions.

- **SQL stays SQL.** The application writes every query. TinyStore does the
  work around them: the file and its connections, migrations, grouped commits,
  snapshots, backup, and the values between Go and SQLite. It is
  `database/sql`, had it done an embedded database's chores.
- **A schema without SQLite's corners.** A struct says what a row is; sqldb
  writes the `STRICT` table, the `CHECK`s that keep a bool a bool and JSON
  valid, and the migration that gets there, and shows every line of it.
- **Not an ORM.** No relations, `Save`, dirty tracking, hooks, lazy loading or
  query builder. A typed `Insert` is one row and one statement; everything else
  is SQL.
- **Not search, and not another engine's store.** Full-text search is SQLite's
  FTS5, [a recipe](#full-text-search); no write spans two engines.

## The public surface

```go
type Note struct {
	ID        uuid.UUID
	AuthorID  int64
	Title     string
	Done      bool
	Tags      sqldb.JSON[[]string]
	Due       *sqldb.Date
	CreatedAt time.Time
}

var Notes = sqldb.Table[Note]("notes",
	sqldb.PrimaryKey("id"),
	sqldb.References("author_id", Users),
	sqldb.Default("done", false),
	sqldb.Index("author_id", "created_at"),
)

var Schema = sqldb.Schema(Users, Notes)

//go:embed migrations/*.sql
var migrations embed.FS

db, err := sqldb.Open(ctx, store, "app", migrations, Schema) // data/sql/app.db

note, err = sqldb.Insert(ctx, db, Notes, note)
note, found, err := sqldb.One[Note](ctx, db, `select * from notes where id = ?`, id)
mine, err := sqldb.All[Note](ctx, db, `select * from notes where author_id = ? order by created_at desc limit 50`, userID)
```

```go
// declaring: checked when the program starts
sqldb.Table[T](name, options...) *sqldb.TableDef[T]
sqldb.Schema(tables...) *sqldb.SchemaDef
sqldb.PrimaryKey(columns...)       sqldb.Unique(columns...)
sqldb.Index(columns...)            sqldb.NamedIndex(name, columns...)
sqldb.References(column, table, sqldb.Cascade | sqldb.SetNull)   // the action is optional
sqldb.Default(column, value)       sqldb.DefaultSQL(column, expression)
sqldb.Check(expression)            sqldb.Storage(column, sqldb.Text | Integer | Real | Blob)
(*SchemaDef).SQL() string                                          // every statement it stands for

// opening: apply what the file has not applied, then check it against the schema
sqldb.Open(ctx, store, name, migrations fs.FS, schema *SchemaDef) (*sqldb.DB, error) // schema may be nil

// reading: each takes a *DB or a *Tx
sqldb.One[T](ctx, h, query, args...) (T, bool, error)
sqldb.All[T](ctx, h, query, args...) ([]T, error)
sqldb.Each[T](ctx, h, query, args...) iter.Seq2[T, error]
sqldb.Scalar[T](ctx, h, query, args...) (T, error)

// writing
(*DB).Exec(ctx, query, args...) (sql.Result, error)                // and (*Tx).Exec
sqldb.ExecOne[T], sqldb.ExecAll[T], sqldb.ExecScalar[T]            // a write's returning rows
sqldb.Insert[T](ctx, h, table *TableDef[T], row T) (T, error)

// transactions
(*DB).Tx(ctx, func(*sqldb.Tx) error) error
(*DB).View(ctx, func(*sqldb.Tx) error) error

// values and errors
sqldb.JSON[T]{V}   sqldb.JSONOf(v)   sqldb.Date{Year, Month, Day}   sqldb.DateOf(t)
*sqldb.ConstraintError{Kind, Table, Constraint, ExtendedCode, Err}
sqldb.UniqueViolation | PrimaryKeyViolation | ForeignKeyViolation | CheckViolation | NotNullViolation

// tests, in package sqldbtest so that no program links testing
sqldbtest.CheckSchema(t, "app", schema, "migrations") // the database's name, as Open takes it
```

## Five cases

They are the API's examples, the gates' workloads and the round's.

**Notes.** A list of one author's notes, marked done and deleted by id.

```go
note, err := sqldb.Insert(ctx, db, Notes, Note{
	ID:        uuid.New(),
	AuthorID:  user.ID,
	Title:     title,
	Tags:      sqldb.JSONOf([]string{}),
	CreatedAt: store.Now(),
})

mine, err := sqldb.All[Note](ctx, db, `
	select * from notes where author_id = ? order by created_at desc limit 50`, user.ID)

done, found, err := sqldb.ExecOne[Note](ctx, db, `
	update notes set done = 1 where id = ? and author_id = ? returning *`, id, user.ID)

_, err = db.Exec(ctx, `delete from notes where id = ? and author_id = ?`, id, user.ID)
```

**Sign-up.** An email is taken once.

```go
user, err := sqldb.Insert(ctx, db, Users, User{Email: form.Email, Name: form.Name, CreatedAt: store.Now()})
if taken, ok := errors.AsType[*sqldb.ConstraintError](err); ok && taken.Kind == sqldb.UniqueViolation {
	return errEmailTaken
}
```

**A transfer.** Two balances change together or not at all.

```go
err = db.Tx(ctx, func(tx *sqldb.Tx) error {
	balance, err := sqldb.Scalar[int64](ctx, tx, `select balance from accounts where id = ?`, from)
	if err != nil {
		return err
	}
	if balance < amount {
		return errInsufficientFunds
	}
	if _, err = tx.Exec(ctx, `update accounts set balance = balance - ? where id = ?`, amount, from); err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `update accounts set balance = balance + ? where id = ?`, amount, to)
	return err
})
```

**A chat.** A page of messages before the oldest one shown, and unread counts
for the chats on screen.

```go
page, err := sqldb.All[Message](ctx, db, `
	select * from messages where chat_id = ? and id < ?
	order by id desc limit 50`, chatID, before)

type Unread struct {
	ChatID int64
	Count  int
}
unread, err := sqldb.All[Unread](ctx, db, `
	select chat_id, count(*) as count from messages
	where chat_id in (select value from json_each(?)) and read = 0
	group by chat_id`, sqldb.JSONOf(chatIDs))
```

**A report.** Notes a day, and every note written out from one snapshot.

```go
type Day struct {
	Day   sqldb.Date
	Notes int
}
days, err := sqldb.All[Day](ctx, db, `
	select date(created_at / 1000, 'unixepoch') as day, count(*) as notes
	from notes group by day order by day`)

for note, err := range sqldb.Each[Note](ctx, db, `select * from notes order by created_at`) {
	if err != nil {
		return err
	}
	if err = out.Write(record(note)); err != nil {
		return err
	}
}
```

## A model is a struct

- **Its fields are the columns, in order.** `CreatedAt` is `created_at` and
  `UserID` `user_id`; an embedded struct's fields count as the embedding one's.
- **A pointer or `sql.Null[T]` is nullable**; any other field is `NOT NULL`.
- **Tags are for exceptions only.** `db:"name"` names the column, `db:"-"`
  leaves the field out, and `db:",generated"` marks a column the database
  fills: it is read like any other and never written by `Insert`, whatever the
  field holds. Nothing is decided by a field's zero value.

```go
type User struct {
	ID        int64  `db:",generated"` // INTEGER PRIMARY KEY: SQLite numbers the rows
	Email     string
	Name      string `db:"display_name"`
	CreatedAt time.Time
	Password  string `db:"-"`          // a form's field, never a column
}
```

## A table is what the struct cannot say

```go
var Users = sqldb.Table[User]("users",
	sqldb.PrimaryKey("id"),
	sqldb.Unique("email"),
)

var Notes = sqldb.Table[Note]("notes",
	sqldb.PrimaryKey("id"),
	sqldb.References("author_id", Users, sqldb.Cascade),
	sqldb.Default("done", false),
	sqldb.Default("tags", []string{}),
	sqldb.Index("author_id", "created_at"),
	sqldb.NamedIndex("notes_open", "done", "due"),
	sqldb.Check("length(title) > 0"),
)

var Schema = sqldb.Schema(Users, Notes)
```

`Schema.SQL()` prints what the migrations have to make:

```sql
CREATE TABLE users (
    id           INTEGER PRIMARY KEY,
    email        TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX users_email ON users (email);

CREATE TABLE notes (
    id         TEXT NOT NULL PRIMARY KEY,
    author_id  INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title      TEXT NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
    tags       TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags)),
    due        TEXT CHECK (due IS date(due)),
    created_at INTEGER NOT NULL,
    CHECK (length(title) > 0)
) STRICT;
CREATE INDEX notes_author_id_created_at ON notes (author_id, created_at);
CREATE INDEX notes_open ON notes (done, due);
```

- **Checked when the program starts**, as `regexp.MustCompile` is: an option
  naming a column the struct lacks, a default of another type, a generated
  column nothing fills, a field sqldb cannot store, a custom type without
  `Storage`, each panics naming the table and the column.
- **`References` points at a table's primary key, which must be one column.**
  A table keyed by several columns cannot be referenced in this version, and a
  declaration that tries panics at start. The action is `ON DELETE`, and there
  is none unless one is given.
- **`Unique` is an index**, not a table constraint, so a later migration can
  drop it without rebuilding the table.
- **An index is named `<table>_<columns>`** unless `NamedIndex` names it, as an
  index a migration made under another name has to be.
- **`Default` is for SQL, not for `Insert`**, which writes every field it has.
  A default fills a column that a raw `insert` leaves out, and lets a later
  migration add a `NOT NULL` column to a table that holds rows.

## Values

| A field of type | is the column | which also checks | and reads from |
|---|---|---|---|
| `string` and named strings | `TEXT` | | `TEXT` |
| `[]byte` | `BLOB` | | `BLOB`, `TEXT` |
| `bool` | `INTEGER` | `IN (0, 1)` | `INTEGER` 0 or 1 |
| integers and named integers | `INTEGER` | | `INTEGER` |
| `float32`, `float64` | `REAL` | | `REAL`, `INTEGER` |
| `time.Time` | `INTEGER`, unix milliseconds | | `INTEGER`; `TEXT` as SQLite's date functions or RFC 3339 spell it |
| `time.Duration` | `INTEGER`, milliseconds | | `INTEGER` |
| `uuid.UUID` of `github.com/google/uuid` or `github.com/gofrs/uuid` | `TEXT`, `8-4-4-4-12` lower case | | `TEXT`; a 16-byte `BLOB` |
| any other byte array, `[N]byte` | `BLOB` | `length(…) = N` | a `BLOB` of `N` bytes |
| `sqldb.Date` | `TEXT`, `YYYY-MM-DD` | `IS date(…)` | `TEXT` |
| `sqldb.JSON[T]` | `TEXT` | `json_valid(…)` | `TEXT`, through `encoding/json` into `T` |
| a type with its own `Scan` and `Value` | what `Storage` names | | what its `Scan` takes |
| `*T`, `sql.Null[T]` | as `T`, and `NULL` | | as `T`, and `NULL` |

- **A read asks one question**: does the value SQLite returned decode into
  this field? Result columns of expressions have no declared type, so sqldb
  never asks the table what a column was meant to be. A value that does not
  decode is `ErrInvalid` naming the column and the field:
  `sqldb: column done: TEXT "yes" does not decode into Note.Done (bool)`.
- **A parameter is written by its Go type**, as its column would hold it: a
  `time.Time` as unix milliseconds, a uuid as its text, a `sqldb.JSON[T]` as
  its JSON, a `sqldb.Date` as `YYYY-MM-DD`. A raw query compares like with like
  without knowing the column.
- **Time is unix milliseconds**, as the jobs and blobs protocols carry it; a
  JavaScript number holds it exactly, and `datetime(created_at / 1000,
  'unixepoch')` shows it. A `time.Time` comes back in UTC without its
  nanoseconds. Write it from `store.Now()`: a test replaces the store's clock,
  and `unixepoch()` in a default reads the machine's.
- **A uuid is text**, because in SQL it is text: `where id = '0192f2a4-…'`
  finds it, `json_object` takes it, and a line from a log pastes into a query.
  As 16 bytes the same `where` finds nothing and says nothing, and
  `json_object` refuses a blob. It costs 20 to 23 bytes more wherever the key
  lies, in its row, its index and every reference and its index: 1.64 times
  those objects in [the round](reports/sqldb-mechanics-2026-09-28.md#uuid-keys),
  whose reads by key were 3 to 12 % slower. `sqldb.Storage("id", sqldb.Blob)`
  stores 16 bytes; a raw query then passes `id[:]`.
- **A key's uuid should be version 7**, its milliseconds first: 500,000 notes
  went in 5.1 to 6.5 times faster than with version 4, whose random keys land
  on random pages of the key's index, as text and as bytes alike.
- **A uuid is a type sqldb knows by name**: `uuid.UUID` of
  `github.com/google/uuid`, or of `github.com/gofrs/uuid` and its `/v5`,
  recognised by its import path without importing either. Sixteen bytes of any other type, an
  MD5, a trace id, a cipher block, are bytes: reflection cannot tell what they
  mean, so they are a `BLOB`. Another library's uuid says
  `Storage("id", sqldb.Text)` and encodes itself through its `Value`.
- **JSON is explicit.** `sqldb.JSON[T]` holds `T` in `V`; `sqldb.JSONOf(v)`
  wraps a value in place. A `sqldb.JSON[[]string]` holding nil writes `null`, a
  valid JSON text; the column is `NULL` only through a pointer to the wrapper.
- **A value SQLite would change is refused.** A NaN is `ErrInvalid`, since a
  `REAL` column stores it as `NULL`; a `uint64` past the `int64` range is
  `ErrInvalid`. SQLite stores `-0` as `0`.
- **A custom type encodes itself**: its `Value` writes and its `Scan` reads.
  What a `Value` returns may depend on the value, so sqldb never infers a
  column's storage from it, and `Storage` says it.

## Reading

- **`One`** answers `found` false for no row and `ErrInvalid` for two.
  **`Scalar`** reads one column of one row, for a query that always answers,
  as `count(*)` does; a query that may find nothing asks `One`.
- **A column no field takes is `ErrInvalid`**; a field no column fills keeps
  its zero value, so `select id, title` fills a `Note` partly.
- **A point read is one statement without a transaction**, a statement being
  its own snapshot, as kv's reads are: 38 to 73 % more reads a second than a
  transaction around a statement compiled each call on Windows, and 57 to 71 %
  in the container. `One[Note]`, decoding every column, serves 23 to 43 % more
  than that path in [the engine round](reports/sqldb-engine-2026-09-28.md).
- **Rows decode through a plan** made once for a type and its columns: as fast
  as decoding by hand in the round, where reflection every row cost a fifth to
  a third more.
- **`All` holds its rows in the store's memory**: it reserves as it decodes,
  and past [its bound](#bounds) it is `ErrLimit` saying to use `Each`.
- **`Each` decodes a row at a time from one snapshot**, and a `break` lets the
  snapshot go. The snapshot lasts at most five seconds, as kv's `View` and a
  records read do, because the write-ahead log grows with the oldest reader;
  an export longer than that pages by key.

## Writing

- **`Exec` is durable when it returns.** `Exec`s from many goroutines wait for
  the writer together and commit in one transaction, each in its savepoint,
  with one fsync: one that fails rolls back alone, and one whose context ends
  before its turn writes nothing (built). A group whose commit fails answers
  `ErrOutcomeUnknown`.
- **`ExecOne`, `ExecAll` and `ExecScalar`** read a write's `returning` rows, as
  `One`, `All` and `Scalar` read a query's.
- **`Insert` is one row and one statement**, grouped like any `Exec`:

```go
note, err = sqldb.Insert(ctx, db, Notes, note)
// INSERT INTO notes (id, author_id, title, done, tags, due, created_at)
// VALUES (?, ?, ?, ?, ?, ?, ?)
```

  Every column but the generated ones is written as its field holds it:
  `Done` false is `0`, `Due` nil is `NULL`. It returns the row as the file
  keeps it, a time to the millisecond in UTC, with what the database
  generated: a rowid the insert's result carries, and the generated columns a
  default fills, which the insert returns: `RETURNING id, code`.
- **Everything else is SQL.** A partial update names its columns; named
  parameters are `database/sql`'s own `sql.Named`.

```go
_, err = db.Exec(ctx, `update notes set title = @title where id = @id`,
	sql.Named("title", title), sql.Named("id", id))
```

- **`?` is always a value.** A query built at run time is a string and its
  arguments, and a list is one JSON value that SQL reads with `json_each`:

```go
query, args := `select * from notes where author_id = ?`, []any{userID}
if text != "" {
	query += ` and title like ?`
	args = append(args, "%"+text+"%")
}
notes, err := sqldb.All[Note](ctx, db, query+` order by created_at desc limit 50`, args...)

picked, err := sqldb.All[Note](ctx, db, `select * from notes where id in (select value from json_each(?))`,
	sqldb.JSONOf(ids))
```

## Transactions and snapshots

- **`Tx` is its own transaction** on the writer, run on the caller's
  goroutine: nil commits, an error rolls back, and a panic rolls back and goes
  on. Calls inside it take `tx`; every typed call takes a `*DB` or a `*Tx`.
- **A call on the DB inside its own `Tx` waits for the `Tx`, which waits for
  it**: it ends when the caller's context does. sqldb logs it once when a write
  has waited ten seconds behind a transaction, naming both.
- **`View` reads several statements from one snapshot**, held at most five
  seconds; a write inside it is `ErrInvalid`.

```go
err = db.View(ctx, func(tx *sqldb.Tx) error { // the page and the total agree
	var err error
	if page, err = sqldb.All[Note](ctx, tx, `select * from notes order by id limit 20`); err != nil {
		return err
	}
	total, err = sqldb.Scalar[int](ctx, tx, `select count(*) from notes`)
	return err
})
```

## Migrations

```text
schema.go           what the program wants now: its structs and Table[…]
migrations/*.sql    what every file has already been through; never edited
sqldbtest           compares the two, and writes what is missing when asked
sqldb.Open          applies what the file has not applied, then checks it against the schema
```

- **A migration is a `.sql` file** named `NNN_what.sql`, applied in name order.
  Anything SQLite runs belongs in one: triggers, views, FTS5, indexes on
  expressions.
- **Applied once, never edited** (built). The file keeps each one's checksum:
  an edited, renamed or missing migration, or a file that has run more of them
  than the binary knows, refuses to open.
- **Every pending migration runs in one transaction** (built), so the file has
  the old schema or the whole new one.
- **Foreign keys are off while migrations run**, and `pragma
  foreign_key_check` must find nothing before the commit, as SQLite's own
  procedure for changing a table asks. With them on, rebuilding a parent table
  deletes its children through `on delete cascade`, and nothing says so: all
  100,000 in the round, a deferred check included. `foreign_keys` cannot change
  inside a transaction, so the migrator turns it off on the writer before
  `BEGIN` and on after `COMMIT`.
- **`Open` checks and never changes.** After the migrations it compares what
  SQLite describes as data: every declared table, its columns with their
  storage, nullability and defaults that are values, the primary key,
  references and indexes. `table_xinfo`, `index_list` and `foreign_key_list`
  report all of it; a default that is a value compares by what `select` makes
  of its text, so `00` is `0`. A difference is `ErrInvalid` listing it:

```text
sql "app": the file does not match the schema:
  notes.description is declared in Note, and the file has no such column; is a migration missing?
```

  What the schema does not declare, a trigger, a view, a virtual table, an
  index on an expression, is the migrations' own and not compared. A column
  the file has and the struct lacks is a difference: a model describes its
  table whole.
- **An expression is text, and text never refuses a file.** SQLite keeps a
  `CHECK` and a `DefaultSQL` only as the words that made them, so
  `CHECK (done IN (0, 1))` and `check ( done in(0,1) )` are one rule spelled
  twice. `Open` does not compare expressions. The test compares them with
  space and case folded outside quotes, and reports a difference as a line to
  read rather than a failure; no SQL parser is written for them. SQLite keeps
  the text as written: `ADD COLUMN` appends the new column's, `RENAME COLUMN`
  rewrites the name inside a check, and `RENAME TABLE` quotes the new name,
  which is enough folding for the migrations the tool writes.
- **A migration is never made from the model at run time.** The model changes
  and the history must not:

```go
var migrations = sqldb.Migrations{sqldb.Create(1, Notes)} // not this

// a month later Note gains Description, and Create(1, Notes) is other SQL:
//   an existing file: migration 1 changed after it was applied, and Open refuses
//   a new file: description is made by migration 1, and the migration adding it fails
```

## Tools

The one piece of glue a project writes, a line for each database:

```go
// internal/data/schema_test.go
func TestSchema(t *testing.T) {
	sqldbtest.CheckSchema(t, "app", AppSchema, "migrations/app")
	sqldbtest.CheckSchema(t, "billing", BillingSchema, "migrations/billing")
}
```

- **A database is known by its name**, the one `Open` takes and the file
  `sql/<name>.db` bears, so one package may hold several schemas and a tool
  still finds each. Two checks of one name in a project are an error.
- **`go test` compares and never writes**; `go tool tinystore` writes the next
  migration from the difference, and needs the name only when there is more
  than one database:

```text
go tool tinystore migrate [name]               what differs; writes nothing
go tool tinystore migrate [name] new <what>    write the next migration from the difference
go tool tinystore schema [name]                the schema's SQL
```

```text
$ go get -tool github.com/tinyshed/tinystore/cmd/tinystore

$ go test ./...
--- FAIL: TestSchema (0.02s)
    sql "app": the schema declares what the migrations do not make:
      + notes.description TEXT
    write it with: go tool tinystore migrate app new <what>

$ go tool tinystore migrate app new add_description
wrote internal/data/migrations/app/002_add_description.sql:

    ALTER TABLE notes ADD COLUMN description TEXT;

$ go tool tinystore migrate
app: 2 migrations, and the schema matches them
billing: 5 migrations, and the schema matches them

$ go tool tinystore schema app
CREATE TABLE users ( … ) STRICT;
…
```

- **A change the tool cannot decide is a draft to finish.** A column gone and
  another new may be a rename; a new type rebuilds the table and needs every
  row's conversion. The draft says so and does not run until a person writes
  it:

```sql
-- migrations/003_notes_status.sql, written by migrate new
-- REVIEW: SQLite cannot make this change in place; notes is rebuilt:
--   notes.status is INTEGER in the schema and TEXT in the migrations
CREATE TABLE notes_new ( … status INTEGER NOT NULL … ) STRICT;
INSERT INTO notes_new ( … ) SELECT …, /* TODO: how does each old status, TEXT, become INTEGER? */ FROM notes;
DROP TABLE notes;
ALTER TABLE notes_new RENAME TO notes;
CREATE INDEX notes_author_id_created_at ON notes (author_id, created_at);
```

  A column gone beside a new one leaves `ALTER TABLE notes /* TODO: RENAME
  COLUMN body TO …, or DROP COLUMN body and ADD COLUMN … */;`, since a rename
  keeps the rows' values and a drop loses them.

- **`cmd/tinystore` is one executable** for every command TinyStore will have:
  `migrate` and `schema` now; `serve`, `backup`, `restore` and `inspect`
  later. It is a module of its own, as a service is: nothing it links reaches
  the library's dependency list.
- **Its `migrate` commands only launch a test.** They find the package whose
  test checks the named database, run that test with the migration's name in
  its environment, and print what it wrote. The comparison and the file come
  from the sqldb the application pinned, so a tool of another version cannot
  disagree with its library.

## The schema outside Go

- **The schema is a value of its own**: tables; columns with a logical type
  (`bool`, `integer`, `real`, `text`, `bytes`, `time`, `duration`, `uuid`,
  `date`, `json`), their storage, nullability and default; keys, indexes,
  references and checks. `Table[T]` builds it from a struct, and the DDL, the
  check and the drafts are made from it rather than from reflection.
- **The runtime needs only the migrations.** `Open`'s schema is an embedded
  program's check. A server, later, opens a file with its migrations and,
  optionally, a manifest that `go tool tinystore schema export` writes from
  the same value; it never needs the application's sources or runs its tests.
- **Another language's SDK is a client of that server**: it reads the
  manifest for its codecs, and writes in batches that run as one transaction
  inside the server, never in a transaction held open across the network.

## Full-text search

FTS5 is compiled into SQLite, and sqldb leaves it as it is. A migration makes
the index and keeps it in step:

```sql
-- migrations/004_search.sql
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

```go
hits, err := sqldb.All[Message](ctx, db, `
	select m.* from messages_fts f join messages m on m.id = f.rowid
	where messages_fts match ? and m.chat_id = ?
	order by bm25(messages_fts) limit 50`, query, chatID)
```

The `unicode61` tokenizer folds Cyrillic case but knows neither `ё` nor word
endings: `сообщения` does not find `сообщение`. Search that does is a package
of its own, after this version.

## Errors

| Sentinel | When |
|---|---|
| `ErrInvalid` | a write sent to a read; a value that does not decode into its field; a column no field takes; two rows for `One`, none for `Scalar`; a NaN; a write inside `View`; a file that does not match the schema |
| `ErrConflict` | a unique key or primary key already held |
| `ErrLimit` | `All` past its bound; a call inside `Tx` or `View` needing more of the store's memory than is free |
| `ErrClosed` | the store closed |
| `ErrCorrupt` | a file SQLite finds damaged |
| `ErrOutcomeUnknown` | a grouped write whose commit failed; read it back before writing again |

- **A constraint is a `*sqldb.ConstraintError`.** Its `Kind`, `Unique`,
  `PrimaryKey`, `ForeignKey`, `Check` or `NotNull`, comes from SQLite's
  extended code; `Table` and `Constraint` are filled when SQLite's message names
  them and empty otherwise. A unique or primary key is `ErrConflict`, the rest
  `ErrInvalid`.
- **An error names its database**, `sql "app"`, and a context's error is kept
  as it came.

## Under the surface

- **One file a database**, `sql/<name>.db`, its tables `STRICT`, one writer
  and a pool of eight `query_only` readers.
- **The writer commits `Exec`s in groups** of at most 1024 writes and 8 MiB,
  each write weighed by its arguments. A group's ten seconds count from when it
  holds the writer, so a transaction holding the writer longer fails none of
  the writes waiting behind it; they all failed at ten seconds before sqldb was
  built. The leader of a group waits for the writer as its own caller, and one
  whose caller leaves hands the lead on without writing.
- **Every connection keeps 128 compiled statements**, by their text, the least
  recently used closed first: 0.7 to 1.1 MiB a connection for statements of
  5.4 to 8.8 KiB. A cache holding the texts in use served half again as many
  point reads as compiling each call; one smaller than them served 5 to 16 %
  fewer than none, since a miss closes, compiles and then runs through a
  `Stmt`. A door keeping a text only when it is read more often than the one it
  would evict compiled as often, and is not built.
- **A `limit ?` compiles every call**, 11 to 14 µs on a page of about 90 in the
  round: `computeLimitRegisters` reads the bound value through
  `sqlite3ExprIsInteger`, which marks the parameter with
  `sqlite3VdbeSetVarmask`, and `vdbeUnbind` expires the statement whenever that
  parameter is bound again. `limit cast(? as integer)` is not marked and costs
  nothing; the documentation shows it, and sqldb rewrites no SQL. QPSG would
  stop the marking too, and modernc v1.59 exposes no call for it.
- **Work enters through the store's gate and slots**, `internal/admission`, as
  every engine's does, and reserves the store's memory before it decodes: 64
  KiB and its arguments before it waits for a connection, more only while it is
  free once it holds one.
- **Snapshots and backup** are the store's: `Store.Snapshot` copies the file
  while the writer writes, and `backup` stores it with the other engines'
  (built).
- **Maintenance is SQLite's own** in this version: automatic checkpoints.
  `pragma optimize` and incremental vacuum wait for a measurement.

## Bounds

| Object | Limit |
|---|---:|
| A database's name | `[a-z0-9][a-z0-9_-]{0,63}` (built) |
| Rows `All` holds | 64 MiB of decoded values, or the store's memory |
| A `View` or `Each` snapshot | 5 s |
| `Exec`s committed together | 1024 writes, 8 MiB |
| A group's hold on the writer | 10 s from when it holds it |
| Compiled statements a connection keeps | 128 |
| Readers | 8: four served as many on Windows and 15 to 26 % fewer in the container |

## Gates

The five cases are their workloads. Every one is built.

| Promise | What enforces it |
|---|---|
| an application's read cannot write | `TestAReadCannotWriteAndSaysWhereToWrite` |
| an application's writes share a commit and fail alone | `TestExecsShareACommitAndFailAlone`, `TestAPanicInsideAWriteRollsBackItsStatementAlone` |
| an applied migration cannot change under the file | `TestMigrationsApplyOnceAndAChangedOneRefuses` |
| a declaration that cannot be a table fails at start | `TestADeclarationThatCannotBeATableFailsAtStart`: a reference to a key of several columns included |
| sixteen bytes of a type sqldb does not know are bytes | `TestOnlyAKnownUUIDTypeIsText` |
| a schema is the DDL it prints | `TestASchemaIsTheSQLItPrints`, golden; `TestANameSQLWouldMisreadIsQuoted` |
| every value comes back as it went in | `TestEveryValueComesBackAsItWentIn`: time to the millisecond, uuid, date, JSON, bool, named types |
| a parameter is written by its Go type | `TestArgumentsAreWrittenByTheirGoType` |
| a value that does not decode names its column and field | `TestAValueThatDoesNotDecodeNamesItsColumnAndField` |
| a value SQLite would change is refused | `TestAValueSQLiteWouldChangeIsRefused`: NaN, `uint64` past `int64`, a day no calendar has |
| `Insert` writes every field but the generated ones | `TestInsertWritesEveryFieldButTheGeneratedOnes`, `TestInsertReturnsWhatTheDatabaseGenerated` |
| `Open` checks the file and changes nothing | `TestOpenChecksTheFileAgainstTheSchemaAndChangesNothing`, `TestOpenNamesEachDifferenceOfStructure` |
| an expression spelled otherwise never refuses a file | `TestOpenDoesNotRefuseAnExpressionSpelledOtherwise` |
| a tool finds each database by its name | `TestTheToolFindsEachDatabaseByItsName`, `TestTheToolWritesTheNextMigrationThroughTheCheck`, in `cmd/tinystore` |
| a rebuilt table keeps its children | `TestARebuiltTableKeepsItsChildren`; `TestMigrationsRunWithoutForeignKeysAndCheckThemBeforeCommit` in `internal/sqlite` |
| `CheckSchema` finds what is missing and writes only when asked | `TestCheckSchemaFindsWhatIsMissingAndWritesOnlyWhenAsked`, `TestTwoChecksOfOneNameFail` |
| an ambiguous change is a draft that does not run | `TestAnAmbiguousChangeIsADraftThatDoesNotRun` |
| a long transaction fails none of the writes behind it | `TestALongTransactionFailsNoWriteBehindIt`; `TestALongTransactionFailsNoGroupedWriteBehindIt`, `TestALeaderWhoseCallerLeavesHandsTheLeadOn` in `internal/sqlite` |
| a call on the DB inside its own `Tx` ends with its context | `TestACallOnTheDBInsideItsOwnTxEndsWithItsContext` |
| `Each` holds one snapshot and one row | `TestEachHoldsOneSnapshotAndOneRow`, `TestASnapshotHeldPastItsBoundSaysSo` |
| `All` past its bound refuses | `TestAllPastItsBoundRefuses` |
| a constraint says its kind | `TestAConstraintSaysItsKind` |
| a statement is compiled once a connection | `TestAStatementIsCompiledOnceAConnection`; `TestAConnectionKeepsTheStatementsItsFileWasOpenedWith` in `internal/sqlite` |
| sqldb holds the store's memory before it decodes | `TestStoreMemoryBoundsReadsAndWrites` |

## Not in the first version

Each waits for a workload that needs it and a measurement that pays for it.

- `Update` of a whole row by its key, `Upsert`, `InsertAll`, and a
  `sqldb.Named(row)` that binds a struct's fields by name.
- A reference over several columns, to a table keyed by several.
- A package for building queries from pieces, and generated query code.
- `schema export` and its manifest; the server and other languages' SDKs.
- Change notifications: a trigger writing another table makes "which tables
  changed" a design of its own.
- Search that knows Russian: `ё`, word endings, stop words, ranking and
  highlights, with its own round.
- `decimal`; money is an integer of its smallest unit.
- `SQLITE_DBCONFIG_ENABLE_QPSG`, a policy for `ATTACH`, `pragma optimize` and
  incremental vacuum.

## What building it settled

- **A constraint's kinds are `UniqueViolation`, `PrimaryKeyViolation`,
  `ForeignKeyViolation`, `CheckViolation` and `NotNullViolation`.** `Unique`,
  `PrimaryKey` and `Check` name the table's options, and Go gives a name to a
  function or to a constant, not to both. The names are those Postgres gives
  the same errors.
- **Eight readers**, not four: in the container eight served 15 to 26 % more
  point reads from eight and sixty-four goroutines, and on Windows as many.
- **`Insert` returns the row it wrote**, a time to the millisecond in UTC as
  the file keeps it, with the rowid the insert's result carries and the
  generated columns a default fills, which the insert returns. With `RETURNING
  *`, returning the row cost 7 % of a grouped insert's rate at 512 goroutines on
  Windows and decoding it on the writer 11 % more: 26,083 inserts a second
  against 32,758 for the same insert returning nothing; returning only what the
  database generated, 31,293 to 31,989 against 32,981 to 33,602.
- **Migrations are the `.sql` files of the FS's root, or of its one
  directory**, so that an `embed.FS` of `migrations/*.sql`, which holds them
  under `migrations/`, passes to `Open` as it is, as the examples above pass it.
- **A `View`'s statements end with its snapshot.** database/sql rolls a
  transaction back when its context ends, and a View's statements run on the
  connection, prepared, where after that they would read outside the snapshot.
  Each carries the snapshot's deadline, and the View ends with `ErrLimit`
  saying to page by key; `TestASnapshotHeldPastItsBoundSaysSo` found it.
- **`Open` compares the file with a database in memory that the schema's SQL
  makes**, so that both sides are what SQLite's pragmas describe, and a schema
  whose SQL does not run fails at `Open`; the check cost 0.8 to 1.5 ms an
  `Open` on the design's two tables. `sqldb/internal/catalog` reads a file's
  tables, compares two files and drafts a migration from the difference; a
  lexer finds a `CHECK`'s text, and no parser is written.
- **An index is found by its name, then by its columns**, so that one a
  migration made under another name still counts, and the test says so as a
  line to read; `NamedIndex` still names it in the SQL the schema prints.
- **The tool and the test speak through the environment.** `go tool
  tinystore` finds the test functions calling `CheckSchema` by reading the
  module's test files, runs the one that checks the database named with
  `TINYSTORE_SQLDB` set to a request in JSON, and reads the check's answer from
  the file the request names. `cmd/tinystore` needs the standard library alone.
- **A write that has waited ten seconds is logged from the writer's wait**:
  `internal/sqlite` tells its engine, through `Config.Waited`, the label of the
  grouped write leading the wait, which in sqldb is its query, and sqldb adds
  the transaction holding the writer and the function that opened it.
- **A cache that does not churn is not built.** A door keeping a text only when
  it is read more often than the one it would evict compiled as often as the
  plain least-recently-used cache, 87.4 to 87.5 % of the reads over 1024
  texts, and served 2 % fewer to 6 % more.

## What was measured

[The mechanics round](reports/sqldb-mechanics-2026-09-28.md), the prototype
`spike/sqldb_*` on one AMD Ryzen 7 7700 with an NVMe disk under Windows 11 and
in a Linux container on it, twice each; [the engine
round](reports/sqldb-engine-2026-09-28.md), the built engine on the same
machine. The rounds have the environment, the command and every figure.

| | Windows |
|---|---:|
| point reads by id a second, one goroutine; eight: today's path, the prepared statement alone | 34,616 to 36,749, 83,962 to 89,258; 59,670 to 63,724, 115,779 to 125,656 |
| a page of twenty: `limit 20`, `limit ?`, `limit cast(? as integer)` | 88.4 to 93.9 µs; 100.3 to 107.9; 87.9 to 93.3 |
| point reads a second over 1024 texts: no cache; 32 kept; 512 kept | 46,878 to 48,342; 40,710 to 42,108; 46,785 to 48,442 |
| a row of the model decoded: by hand; a plan; reflection every row | 1.16 to 1.33 µs; 1.19 to 1.30; 1.47 to 1.61 |
| 500,000 notes and a comment each: text keys; 16-byte keys | 105.4 to 105.8 MiB; 64.4 to 64.7 |
| 500,000 notes inserted: uuid version 4; version 7 | 6.8 to 9.9 s; 1.3 to 1.6 |
| a parent rebuilt under 100,000 children: foreign keys on; off and checked | 0 children left; 100,000 |

The built engine, Windows and the container's first pass:

| | Windows | the container |
|---|---:|---:|
| point reads a second, 1/8/64 goroutines: `One[Note]`; a statement scanned by hand | 58,688 to 60,234 / 123,961 to 126,540 / 111,726 to 118,809; 67,930 to 68,343 / 132,929 to 134,778 / 124,876 to 126,658 | 95,281 / 286,430 / 301,711; 106,045 / 309,693 / 329,937 |
| the same against `database/sql` as a program opens it, 100,000 notes | 80,329 to 80,591 / 206,160 to 207,521 / 189,461 to 190,389; 64,589 to 64,747 / 111,387 to 112,094 / 73,913 to 74,012 | 101,468 / 300,583 / 313,010; 78,517 / 153,322 / 102,174 |
| inserts a second from 64 and 512 goroutines: grouped `Exec`; `database/sql` | 12,815 to 13,162, 18,054 to 18,538; 389 to 404, 388 to 393 with `database is locked` | 6,435, 23,908; 188, 205 with `database is locked` |
| `Insert`; the same insert returning nothing, 512 goroutines | 31,293 to 31,989; 32,981 to 33,602 | 29,931; 31,344 |

Earlier rounds measured the part of sqldb built before:

- [The kv engine round](reports/kv-engine-2026-09-26.md): a stored session
  read through sqldb at 141,791 a second against kv's 289,717, eight
  goroutines over a million sessions; sqldb prepared its SQL on every call and
  read inside a transaction.
- [The jobs engine round](reports/jobs-engine-2026-09-27.md): inserts through
  grouped `Exec` at 301, 1,447, 10,663 and 63,727 a second from 1, 8, 64 and
  512 goroutines, against about 300 a transaction each.

## Open

- **A write waiting behind its own caller's `Tx`**: a warning after ten
  seconds is built; a bound on how long a `Tx` may hold the writer is the
  alternative.
- **An `Each` longer than five seconds** for exports that cannot page by key.
- **Readers as read-only connections at the file**, which no pragma can turn
  into writers, on every platform the store runs on.
- **The range read after `analyze`**, 3 to 5 µs slower in all four runs of the
  mechanics round: a recompile or another plan.
- **`Open`'s check on a large schema**: 0.8 to 1.5 ms on two tables; it reads
  each declared table's pragmas, and a hundred tables are not measured.
- **A custom type's value as `Insert` returns it**: the row it was given, since
  only a read through its `Scan` could say otherwise.
