# SQL API for Go

Every public type, function and constant of the SQL engine in `github.com/tinyshed/tinystore/sqldb`, generated from its source. The [SQL guide](../../sql/README.md) explains how to use them, and the [Bun and Node](../bun/sql.md) and [Python](../python/sql.md) pages list the same API.

## ErrOutcomeUnknown

```go
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown
```

ErrOutcomeUnknown is a write whose group's commit failed: it may or may not be in the file, and its caller reads it back before writing again.

## ErrPending

```go
var ErrPending = sqlite.ErrPending
```

ErrPending is a migration the file has not applied, which ApplyNone and Migrated find and apply none of; it comes with tinystore.ErrInvalid.

## All

```go
func All[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error)
```

All reads every row query returns, holding them in the store's memory as it decodes them. Past 64 MiB of them it is tinystore.ErrLimit, and Each reads them instead.

## Copy

```go
func Copy(ctx context.Context, store *tinystore.Store, name, dir string) (tinystore.SnapshotFile, error)
```

Copy writes a consistent copy of the database name into dir, as the store's Snapshot copies a database that is open, while no one in this store has it open: a backup of a whole directory takes the databases no program opened too. Opening one instead would make it open for good, and a migration that waits for its program would then wait for the store to open again.

It holds the name while it copies, so an Open meanwhile is tinystore.ErrInUse, as is a copy of a database that is open.

## Each

```go
func Each[T any](ctx context.Context, h Handle, query string, args ...any) iter.Seq2[T, error]
```

Each decodes the rows query returns a row at a time, from one snapshot held at most five seconds, which a break lets go; an export longer than that pages by key. An error ends the rows as their last element.

## ExecAll

```go
func ExecAll[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error)
```

ExecAll runs a write on the writer and reads every row its returning clause gives, as All reads a query's.

## ExecOne

```go
func ExecOne[T any](ctx context.Context, h Handle, query string, args ...any) (T, bool, error)
```

ExecOne runs a write on the writer and reads the one row its returning clause gives, as One reads a query's.

## ExecScalar

```go
func ExecScalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error)
```

ExecScalar runs a write on the writer and reads the one column of the one row its returning clause gives, such as the id of insert … returning id.

## Insert

```go
func Insert[T any](ctx context.Context, h Handle, table *TableDef[T], row T) (T, error)
```

Insert writes row into table in one statement, grouped like any Exec: every column but the generated ones as its field holds it, false as 0 and nil as NULL. It returns the row as the file keeps it, a time to the millisecond in UTC, with what the database generated: the rowid its result carries, and the generated columns the insert returns.

	INSERT INTO notes (id, title, done) VALUES (?, ?, ?)

## One

```go
func One[T any](ctx context.Context, h Handle, query string, args ...any) (T, bool, error)
```

One reads the one row query returns into T: a struct takes each column into the field of its name, and any other T takes the only column. found is false for no row; two are tinystore.ErrInvalid.

## Scalar

```go
func Scalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error)
```

Scalar reads one column of one row, for a query that always answers, as count(\*) does; a query that may find nothing asks One.

## Action

```go
type Action int
```

Action is what a reference does when the row it refers to is deleted.

### Cascade

```go
const (
	Cascade Action = iota + 1
	SetNull
)
```

### Action.String

```go
func (a Action) String() string
```

## AnyTable

```go
type AnyTable interface {
	// contains filtered or unexported methods
}
```

AnyTable is a table of any row type, as Schema and References take it.

## Batch

```go
type Batch struct {
	// contains filtered or unexported fields
}
```

Batch is writes gathered before any of them runs, which then commit together or not at all: statements, and what another engine keeps in this database, such as a job that a jobs store opened with Options.In enqueues.

Nothing runs while the batch is built, so building it may take as long as it likes; what it cannot do is read what an earlier write of it wrote. A statement that depends on an earlier one says so in SQL, with last\_insert\_rowid() or a key the program chose; one that needs to read first and decide belongs in a Tx.

### Batch.Add

```go
func (b *Batch) Add(change Change)
```

Add adds another engine's write to the batch; it runs after the statements.

### Batch.Exec

```go
func (b *Batch) Exec(query string, args ...any)
```

Exec adds a statement to the batch; it runs when the batch does.

## Change

```go
type Change interface {
	// Bytes is what the change holds, counted against the group's bound, or
	// why it cannot be written, which writes nothing of the batch.
	Bytes() (int, error)
	// Apply writes the change with the batch's writer. file is the database's,
	// which the change refuses when its tables are in another.
	Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error
	// Done is told the batch's outcome once, nil when it is durable, whatever
	// became of it, so that the change lets go of what it holds.
	Done(err error)
}
```

Change is a write another engine keeps in this database, which a Batch runs beside its statements, in their savepoint. A program does not implement it: its methods name the store's own types. An engine gives one out, as a jobs queue's Enqueued does.

## ConstraintError

```go
type ConstraintError struct {
	// Kind comes from SQLite's extended code.
	Kind ConstraintKind
	// Table and Constraint are empty when SQLite's message does not name them.
	Table        string
	Constraint   string
	ExtendedCode int
	Err          error
	// contains filtered or unexported fields
}
```

ConstraintError is a write that broke a constraint. A unique or primary key is tinystore.ErrConflict, the others tinystore.ErrInvalid.

SQLite's message becomes Table and Constraint when it names them:

	UNIQUE constraint failed: users.email      Table users, Constraint email
	CHECK constraint failed: length(title) > 0 Constraint length(title) > 0

### ConstraintError.Error

```go
func (e *ConstraintError) Error() string
```

### ConstraintError.Unwrap

```go
func (e *ConstraintError) Unwrap() []error
```

## ConstraintKind

```go
type ConstraintKind int
```

ConstraintKind is the constraint a write broke.

### UniqueViolation

```go
const (
	UniqueViolation ConstraintKind = iota + 1
	PrimaryKeyViolation
	ForeignKeyViolation
	CheckViolation
	NotNullViolation
)
```

### ConstraintKind.String

```go
func (k ConstraintKind) String() string
```

## DB

```go
type DB struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, name string, migrations fs.FS, schema *SchemaDef,
	options ...OpenOption,
) (*DB, error)
```

Open opens or creates sql/\&lt;name>.db inside the store, applies in one transaction every migration the file has not applied, then checks the file against schema, which may be nil:

	001_users.sql   applied on the first run
	002_posts.sql   applied the first time a binary carrying it opens the file

The migrations are the .sql files at the root of migrations or, when it has none, in its one directory, as an embed.FS of migrations/\*.sql holds them. A migration changed after it was applied, a file that has applied more of them than the binary knows, another engine's file, or a file that does not match the schema refuses to open.

Nil migrations open the file as it is, making an empty one when there is none: nothing is applied and no history is checked, as a script or a first try wants. A later Open that carries migrations applies them from the first.

### DB.Batch

```go
func (d *DB) Batch(ctx context.Context, build func(*Batch) error) (err error)
```

Batch runs what build adds in one savepoint of a grouped commit, as an Exec runs its one statement: they share an fsync with the writes beside them, and a batch whose statement or change fails rolls back alone. An error from build, an argument no column can hold or a change that cannot be written writes nothing.

A Tx holds the writer for itself and pays a commit of its own; a batch, whose writes are known before it runs, need not.

### DB.Close

```go
func (d *DB) Close(ctx context.Context) error
```

Close lets the calls in flight finish and closes the file; cancellation stops waiting, not the cleanup. The store calls it: an application closes the store instead.

### DB.Exec

```go
func (d *DB) Exec(ctx context.Context, query string, args ...any) (sql.Result, error)
```

Exec runs one statement that may write on the file's one writer, and returns once it is durable.

Statements from many goroutines commit together, each in its savepoint of one transaction, with one fsync.

A statement that fails rolls back alone. One whose caller's context ends before its turn writes nothing. A group whose commit fails answers ErrOutcomeUnknown.

### DB.Migrated

```go
func (d *DB) Migrated(ctx context.Context, migrations fs.FS) error
```

Migrated checks migrations against those the file applied, applying none: one it has not applied is ErrPending, and one changed after it was applied, or missing, is refused as Open refuses it. A database opens once in a store, so a program that opens it again checks with Migrated instead. Nil migrations check nothing, as Open opens with none.

### DB.SQLiteFile

```go
func (d *DB) SQLiteFile() *sqlite.File
```

SQLiteFile is the database's file, for an engine that keeps its tables in it, as a jobs store opened with Options.In does. A program has no use for it.

### DB.Snapshot

```go
func (d *DB) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies the database into dir while it keeps working.

### DB.Tx

```go
func (d *DB) Tx(ctx context.Context, work func(tx *Tx) error) (err error)
```

Tx runs work in one writer transaction of its own, on the caller's goroutine: nil commits, an error rolls back, and a panic rolls back and goes on.

A call on the DB inside its own Tx waits for the Tx, which waits for it, until the call's context ends. sqldb logs a write that has waited ten seconds, naming the transaction.

### DB.View

```go
func (d *DB) View(ctx context.Context, read func(tx *Tx) error) error
```

View runs read against one snapshot of the database, held at most five seconds, since the write-ahead log grows with the oldest reader. A write inside it is tinystore.ErrInvalid.

## Date

```go
type Date struct {
	Year  int
	Month time.Month
	Day   int
}
```

Date is a day of the calendar without a time or a zone, kept as its YYYY-MM-DD text, which SQLite's date functions read and write.

### DateOf

```go
func DateOf(t time.Time) Date
```

DateOf is the day t falls on in its own location.

### Date.In

```go
func (d Date) In(loc *time.Location) time.Time
```

In is the day's first moment in loc.

### Date.String

```go
func (d Date) String() string
```

## Handle

```go
type Handle interface {
	// contains filtered or unexported methods
}
```

Handle is where a call runs: a \*DB, or the \*Tx that DB.Tx or DB.View gives.

## JSON

```go
type JSON[T any] struct{ V T }
```

JSON holds V, which its column keeps as JSON text through encoding/json. A JSON holding a nil slice or map writes null, a valid JSON text; the column is NULL only through a pointer to the JSON.

### JSONOf

```go
func JSONOf[T any](v T) JSON[T]
```

JSONOf wraps v, as a query's argument or a field's value.

## OpenOption

```go
type OpenOption func(*tuning)
```

### ApplyNone

```go
func ApplyNone() OpenOption
```

ApplyNone opens a file only if it has applied every migration given, and applies none, as a program that may change rows and not the schema opens one.

A migration the file has not applied is ErrPending, and a file that is not there is not made.

### Readers

```go
func Readers(n int) OpenOption
```

Readers is how many reader connections the database opens under load, eight unless it says, and fewer when the store's Options.Readers says; half a MiB each, and those beyond one close after a minute unused.

## Rows

```go
type Rows struct {
	Columns []string
	Values  [][]any
}
```

Rows is what a query returns without a struct to take it: the columns' names, and each row's values as SQLite returned them, a column each.

A value is nil, an int64, a float64, a string or a \[]byte, what the row keeps whatever its column declares.

### ExecQuery

```go
func ExecQuery(ctx context.Context, h Handle, query string, args ...any) (Rows, error)
```

ExecQuery runs a write on the writer and reads the rows its returning clause gives, as Query reads a query's. A write that returns none answers its columns alone, or none.

### Query

```go
func Query(ctx context.Context, h Handle, query string, args ...any) (Rows, error)
```

Query reads every row query returns as SQLite returns it, for a program that has no struct for them, as the server has none for its clients. The rows are held in the store's memory as All holds them.

## SchemaDef

```go
type SchemaDef struct {
	// contains filtered or unexported fields
}
```

SchemaDef is a database's tables, in the order Schema was given them.

### Schema

```go
func Schema(tables ...AnyTable) *SchemaDef
```

Schema is what the migrations of one database make, and what Open checks the file against. It panics, as Table does, on two tables of one name and on a reference to a table it does not hold.

### SchemaDef.SQL

```go
func (s *SchemaDef) SQL() string
```

SQL is every statement the schema stands for, as its migrations have to make it.

## StorageClass

```go
type StorageClass int
```

StorageClass is the STRICT column a custom type, or a uuid, is kept in.

### Text

```go
const (
	Text StorageClass = iota + 1
	Integer
	Real
	Blob
)
```

### StorageClass.String

```go
func (s StorageClass) String() string
```

## TableDef

```go
type TableDef[T any] struct {
	// contains filtered or unexported fields
}
```

TableDef is a table declared for the rows of T: what Schema prints, what Open checks the file against and what Insert writes. Table makes it.

### Table

```go
func Table[T any](name string, options ...TableOption) *TableDef[T]
```

Table declares the table name for the rows of T, and panics, as regexp.MustCompile does, on a declaration that cannot be a table:

	an option naming a column T lacks
	a default of another type
	a generated column nothing fills
	a field sqldb cannot store
	a custom type without Storage
	a reference to a table keyed by several columns

## TableOption

```go
type TableOption func(*table) error
```

TableOption says what a struct cannot: keys, indexes, references, defaults, checks and storage.

### Check

```go
func Check(expression string) TableOption
```

Check is a condition every row keeps, as SQL spells it.

### Default

```go
func Default(column string, value any) TableOption
```

Default is the value SQL gives column when an insert leaves it out, and what lets a later migration add a NOT NULL column to a table holding rows. Insert writes every field, so it never uses one. value is of the field's type, or of the type a pointer, a sql.Null or a sqldb.JSON holds.

### DefaultSQL

```go
func DefaultSQL(column, expression string) TableOption
```

DefaultSQL is an expression SQL computes for column when an insert leaves it out, such as lower(hex(randomblob(4))).

### Index

```go
func Index(columns ...string) TableOption
```

Index is an index named \&lt;table>\_\&lt;columns>.

### IndexWhere

```go
func IndexWhere(where string, columns ...string) TableOption
```

IndexWhere is an index of the rows where holds, named \&lt;table>\_\&lt;columns>.

### NamedIndex

```go
func NamedIndex(name string, columns ...string) TableOption
```

NamedIndex is an index a migration made under a name of its own.

### PrimaryKey

```go
func PrimaryKey(columns ...string) TableOption
```

PrimaryKey is the table's key; a single INTEGER column is the rowid SQLite numbers new rows by.

### References

```go
func References(column string, parent AnyTable, action ...Action) TableOption
```

References points column at the primary key of parent, which must be one column of the same storage; the action, when given, is ON DELETE's.

### Storage

```go
func Storage(column string, class StorageClass) TableOption
```

Storage says how column keeps a type sqldb does not know, whose own Scan and Value convert it. It can also keep a uuid as its 16 bytes, which a raw query then passes as id\[:].

### Unique

```go
func Unique(columns ...string) TableOption
```

Unique is a unique index named \&lt;table>\_\&lt;columns>, not a table constraint. A later migration can then drop it without rebuilding the table.

### UniqueWhere

```go
func UniqueWhere(where string, columns ...string) TableOption
```

UniqueWhere is a unique index of the rows where holds, named \&lt;table>\_\&lt;columns>: a promise about part of a table, such as one owner.

	UniqueWhere("role = 'owner'", "role")  →  CREATE UNIQUE INDEX users_role ON users (role) WHERE role = 'owner';

## Tx

```go
type Tx struct {
	// contains filtered or unexported fields
}
```

Tx is one transaction of the database, given to the function DB.Tx or DB.View runs and used by the goroutine that runs it. Every typed call takes it where it takes a DB.

### Tx.Add

```go
func (t *Tx) Add(ctx context.Context, change Change) error
```

Add writes another engine's change in the transaction, as a Batch's Add does after its statements: a job that a jobs store opened with Options.In enqueues, which then commits with the transaction's rows or not at all. The change is told the transaction's outcome once it ends, nil when it committed.

Make the change before the transaction begins. A change holds the memory its value needs, and a transaction holding the writer must not wait for memory that a write queued behind it holds.

### Tx.Exec

```go
func (t *Tx) Exec(ctx context.Context, query string, args ...any) (sql.Result, error)
```

Exec runs one statement of the transaction.

<!-- Generated by task reference from sqldb/. Edit the doc comments there, not this file. -->
