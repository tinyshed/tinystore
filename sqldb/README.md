# sqldb

The application's own SQLite databases inside a `tinystore.Store`, each in
`sql/<name>.db`: its tables, joins, reports and transactions. The application
writes every query; sqldb owns the file, its connections, the migrations and
the values between Go and SQLite. A struct says what a row is, `Table[T]` what
a struct cannot say, and `Open` checks the file the migrations made against
them. The design and the measurements behind it are
research's [design/sqldb.md](https://github.com/tinyshed/research/blob/main/tinystore/design/sqldb.md).

```go
type Note struct {
	ID        uuid.UUID // text, 8-4-4-4-12
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
)

var Schema = sqldb.Schema(Users, Notes)

//go:embed migrations/*.sql
var migrations embed.FS

db, err := sqldb.Open(ctx, store, "app", migrations, Schema) // data/sql/app.db

note, err = sqldb.Insert(ctx, db, Notes, note)
note, found, err := sqldb.One[Note](ctx, db, `select * from notes where id = ?`, id)
mine, err := sqldb.All[Note](ctx, db, `select * from notes where author_id = ? order by created_at desc limit 50`, user)
count, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`)
_, err = db.Exec(ctx, `delete from notes where id = ? and author_id = ?`, id, user)
```

A test compares the schema with what the migrations make, and a tool writes
the next migration from the difference:

```go
func TestSchema(t *testing.T) {
	sqldbtest.CheckSchema(t, "app", Schema, "migrations")
}
```

```text
$ go get -tool github.com/tinyshed/tinystore/cmd/tinystore
$ go tool tinystore migrate app new add_description
wrote internal/data/migrations/002_add_description.sql:

    ALTER TABLE notes ADD COLUMN description TEXT NOT NULL DEFAULT '';
```

## Contracts

- **What a call's name starts with says where it runs.** `Exec`, `ExecOne`,
  `ExecAll`, `ExecScalar` and `Insert` may write, and run on the file's one
  writer; `One`, `All`, `Each` and `Scalar` read, on readers that refuse to
  write: a write sent to one fails before writing anything, with
  `tinystore.ErrInvalid` naming the call to use. Every typed call takes a
  `*DB`, or the `*Tx` of `DB.Tx` or `DB.View`.
- **A read is one statement without a transaction**, a statement being its
  own snapshot, on one of eight readers. Each connection keeps 128 compiled
  statements by their text, the least recently used closed first; a bound
  `limit ?` compiles every call, and `limit cast(? as integer)` does not.
- **`One` answers `found`**: false for no row, `ErrInvalid` for two. `Scalar`
  reads one column of one row, for a query that always answers, as `count(*)`
  does, and no row is `ErrInvalid`. `All` holds its rows in the store's memory
  as it decodes them, and past 64 MiB of them is `tinystore.ErrLimit`; `Each`
  decodes a row at a time from one snapshot, which a `break` lets go and
  which lasts at most five seconds, since the write-ahead log grows with the
  oldest reader: an export longer than that pages by key.
- **A struct takes each column into the field of its name**: `CreatedAt` is
  `created_at`, `db:"name"` names a column otherwise, `db:"-"` leaves a field
  out, and `db:",generated"` marks a column the database fills, read like any
  other and never written by `Insert`. An embedded struct's fields count as the
  embedding one's. A column no field takes is `ErrInvalid`; a field no column
  fills keeps its zero value. A `T` that is not such a struct takes the only
  column.
- **A value decodes into its field or says why**, as `ErrInvalid` naming the
  column, the value and the field: `column done: TEXT "yes" does not decode
  into Note.Done (bool)`. A read asks what SQLite returned, never what the
  table meant, since a column of an expression has no declared type:

  | a field of type                                                                          | is the column                | which also checks | and reads from                                                    |
  |------------------------------------------------------------------------------------------|------------------------------|-------------------|-------------------------------------------------------------------|
  | `string`, named strings                                                                  | `TEXT`                       |                   | `TEXT`                                                            |
  | `[]byte`                                                                                 | `BLOB`                       |                   | `BLOB`, `TEXT`                                                    |
  | `bool`                                                                                   | `INTEGER`                    | `IN (0, 1)`       | `INTEGER` 0 or 1                                                  |
  | integers, named integers                                                                 | `INTEGER`                    |                   | `INTEGER` in the field's range                                    |
  | `float32`, `float64`                                                                     | `REAL`                       |                   | `REAL`, `INTEGER`                                                 |
  | `time.Time`                                                                              | `INTEGER`, unix milliseconds |                   | `INTEGER`; `TEXT` as SQLite's date functions or RFC 3339 spell it |
  | `time.Duration`                                                                          | `INTEGER`, milliseconds      |                   | `INTEGER`                                                         |
  | `uuid.UUID` of the standard library, `github.com/google/uuid` or `github.com/gofrs/uuid` | `TEXT`, lower case           |                   | `TEXT`; a 16-byte `BLOB`                                          |
  | any other `[N]byte`                                                                      | `BLOB`                       | `length(…) = N`   | a `BLOB` of `N` bytes                                             |
  | `sqldb.Date`                                                                             | `TEXT`, `YYYY-MM-DD`         | `IS date(…)`      | `TEXT`                                                            |
  | `sqldb.JSON[T]`                                                                          | `TEXT`                       | `json_valid(…)`   | `TEXT`, through `encoding/json`                                   |
  | a type with its own `Scan` and `Value`                                                   | what `sqldb.Storage` says    |                   | what its `Scan` takes                                             |
  | `*T`, `sql.Null[T]`, `sql.NullString` and its kin                                        | as `T`, and `NULL`           |                   | as `T`, and `NULL`                                                |

- **A parameter is written by its Go type**, as its column would hold it: a
  `time.Time` as unix milliseconds, a known uuid as its text, a
  `sqldb.JSON[T]` as its JSON, a `sqldb.Date` as `YYYY-MM-DD`, a `bool` as 0 or
  1, inside `sql.Named` too. A value SQLite would store otherwise is
  `ErrInvalid` before anything is written: a NaN, which a `REAL` column keeps as
  `NULL`, a `uint64` past `int64`, a `Date` no calendar has.
  A uuid column that `Storage` keeps as 16 bytes therefore takes `id[:]` in a
  query, not the uuid.
- **A write returns once it is durable.** Writes from many goroutines wait for
  the writer together and commit in one transaction, each in a savepoint, with
  one fsync: one that fails rolls back alone, one whose caller's context ends
  before its turn writes nothing, one that has started finishes with its
  group though its caller cancels. A deadline that passes while its statement
  runs ends the statement, since the application's SQL may never end, and
  SQLite then rolls back the whole transaction: the group fails with it, each
  write told so. A group holds the writer at most ten seconds counted from when it
  holds it, so a transaction holding the writer longer fails none of the
  writes behind it; a group whose commit fails answers
  `sqldb.ErrOutcomeUnknown`, and its caller reads back before writing again. A
  panic while an `Exec` form reads its rows rolls back its statement alone and
  goes on in the caller's goroutine.
- **`Insert` writes one row in one statement**, grouped like any `Exec`:
  every column but the generated ones as its field holds it, `false` as 0 and
  `nil` as `NULL`, whatever a `Default` says. It returns the row as the file
  keeps it, a time to the millisecond in UTC, with what the database
  generated: the rowid the insert's result carries, and the generated columns
  a default fills, which it returns. Everything else is SQL.
- **`Batch` writes several statements as one**, gathered before any runs and
  then committed with the writes beside them, in one savepoint of the group:
  all of them or none, a failure rolling back the batch alone, one fsync for
  the group. Building it runs nothing, so it may take its time; a statement
  that needs an earlier one's result says so in SQL, `last_insert_rowid()` or
  a key the program chose, and one that must read before it decides is a `Tx`.
  An engine that keeps its tables in the database adds its own writes, as a
  jobs queue's `Enqueued` adds a job that commits with the rows it is about.
- **`Tx` is a transaction of its own** on the writer, run on the caller's
  goroutine: nil commits, an error rolls back, and a panic rolls back and goes
  on. A call on the `DB` inside its own `Tx` waits for the `Tx`, which waits
  for it, until the call's context ends; a write that has waited ten seconds
  for the writer is logged once, naming the transaction's caller. `View` reads
  several statements from one snapshot, held at most five seconds; a write
  inside it is `ErrInvalid`, and a `Tx` used after its function returned is
  `tinystore.ErrClosed`. A `Tx` holds the writer for itself and pays an fsync
  of its own, so transactions from many goroutines commit one at a time: what
  can be a `Batch` should be. `Tx.Add` writes another engine's change in the
  transaction, as a `Batch` adds one, told the outcome once the transaction
  ends; make the change before the transaction begins, since it holds memory
  that a transaction holding the writer must not wait for.
- **`Copy` copies a database no one in the store has open** into a snapshot's
  directory, as the store's `Snapshot` copies an open one, without opening it:
  a backup of a whole directory takes it, and a migration that waits for its
  program still applies when the program opens it. It holds the name while it
  copies, so an `Open` meanwhile, or a copy of an open one, is
  `tinystore.ErrInUse`.
- **Tables named `_tinystore_…` are the store's**: its migration history, and
  the queues a jobs store opened `In` the database keeps there. A schema leaves
  them out, and a table cannot be declared with such a name.
- **A declaration that cannot be a table panics when the program starts**,
  as `regexp.MustCompile` does, naming the table: an option naming a column
  the struct lacks, a default of another type, a generated column nothing
  fills, a field sqldb cannot store, a custom type without `Storage`, a
  reference to a table keyed by several columns or of another storage, two
  primary keys or two indexes of one name. `References` points at its
  parent's one-column key, `Unique` is an index rather than a table
  constraint, an index is named `<table>_<columns>` unless `NamedIndex` names
  it, and `Schema(...).SQL()` prints every statement the migrations have to
  make, each table `STRICT`.
- **Migrations are the `.sql` files** at the root of the `fs.FS` given or, when
  it holds none, in its one directory, applied in name order and all pending
  ones in one transaction, with foreign keys off and `foreign_key_check`
  before the commit, since rebuilding a parent with them on deletes its
  children. Each is kept by its checksum: an edited, renamed or missing one, a
  file that has applied more than the binary knows, or another engine's file
  refuses to open, `ErrInvalid`.
- **Nil migrations open the file as it is.** `Open(ctx, store, "app", nil, nil)`
  makes an empty file when there is none, applies nothing and checks no
  history, for a script or a first try; with `ApplyNone` it opens only a file
  that is there. A later `Open` that carries migrations applies them from the
  first.
- **`ApplyNone` and `Migrated` apply nothing.** `Open(…, sqldb.ApplyNone())`
  opens only a file that applied every migration given, and makes none that
  is not there; `db.Migrated(ctx, migrations)` checks an open file the same
  way, since a database opens once a store. A migration the file has not
  applied is `sqldb.ErrPending` with `ErrInvalid`; an edited one refuses as
  `Open` refuses it. The server opens a data client's database so.
- **`Query` and `ExecQuery` read rows without a struct**: the columns' names,
  even of no row, and each value as SQLite returned it, nil, an int64, a
  float64, a string or a []byte, held in the store's memory as `All` holds
  rows; `ExecQuery` runs on the writer, for a returning clause. A value is
  what the row keeps, whatever its column declares: a time held as text comes
  back as its text.
- **A statement SQLite refuses is `ErrInvalid`**: a syntax error, a table,
  column or function the file does not have, a parameter out of range or one
  no argument fills; a value past SQLite's length is `ErrLimit`.
- **`Open` checks the file and never changes it.** With a schema, it compares
  what SQLite describes of each declared table, its columns' storage,
  nullability and defaults that are values, its key, references and indexes,
  `STRICT`, and refuses a difference, `ErrInvalid` listing each. An expression,
  a `CHECK` or a `DefaultSQL`, never refuses a file; a table, trigger, view or
  index on an expression the schema does not declare is the migrations' own.
- **A constraint is a `*sqldb.ConstraintError`**: its `Kind`,
  `UniqueViolation`, `PrimaryKeyViolation`, `ForeignKeyViolation`,
  `CheckViolation` or `NotNullViolation`, comes from SQLite's extended code,
  and `Table` and `Constraint` from its message when it names them. A unique
  or primary key is `tinystore.ErrConflict`, the others `ErrInvalid`.
- **Calls hold the store's memory.** With `Options.Memory`, a call reserves 64
  KiB and its arguments before it waits for a connection, and takes more only
  while it is free once it holds one, which it may otherwise wait for itself:
  a result past the free memory is `ErrLimit`. Inside `Tx` or `View` a call
  waits for no memory. No statement makes a string, blob or row longer than
  the whole budget: SQLite would allocate it before a call could count it, so
  it is `ErrLimit` at once, a `length(printf(…))` whose answer is one number
  included.
- **An error names its database**, `sql "app"`, and a context's error is kept
  as it came. A name is `[a-z0-9][a-z0-9_-]{0,63}`; a second `Open` of a name
  in one store is `tinystore.ErrInUse`; after the store closes, every call is
  `tinystore.ErrClosed`.

## The schema in a test

`sqldbtest.CheckSchema(t, name, schema, dir)` applies the migrations in `dir`
to a new file through `Open` and compares it with the schema. A difference of
structure fails the test, a line each, `+` for what the schema declares and
the migrations do not make, `-` for the reverse:

```text
sql "app": the schema declares what the migrations do not make:
  + notes.description TEXT NOT NULL DEFAULT ''
write it with: go tool tinystore migrate app new <what>
```

A `CHECK` or a default's expression spelled otherwise is logged as a line to
read, never a failure. A test never writes. `go tool tinystore migrate
[name] new <what>`, of the module `cmd/tinystore`, finds the test that checks
the database by its name, runs it asking for the next migration, and prints
what it wrote; a change the tool cannot decide, a rename or a new type, is a
draft whose `TODO` keeps it from running until a person writes it. `go tool
tinystore migrate [name]` says what differs, `go tool tinystore schema [name]`
prints the schema's SQL.

## Not in the first version

What [design/sqldb.md](https://github.com/tinyshed/research/blob/main/tinystore/design/sqldb.md) leaves for later: `Update` of a whole
row, `Upsert`, `InsertAll` and binding a struct's fields by name; a reference
over several columns; building queries from pieces; the schema's manifest, a
server and other languages; change notifications; search that knows Russian;
`decimal`.
