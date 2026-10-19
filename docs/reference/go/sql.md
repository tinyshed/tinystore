# SQL API for Go

Every public type, function and constant of the SQL engine in `tinyshed.org/tinystore/sqldb`, generated from its source. The [SQL guide](../../sql/README.md) explains how to use them, and the [Bun and Node](../bun/sql.md) and [Python](../python/sql.md) pages list the same API.

## All

```go
func All[T any](ctx context.Context, r Runner, text string, args ...any) ([]T, error)
```

All is every row a statement gives, read into T: a struct, a map, or one column's value.

## Columns

```go
func Columns[T any](ctx context.Context, db *DB, text string, args ...any) (T, error)
```

Columns is what All gives, by its columns: T is a struct whose fields are slices, one a column, by the column's name; a nil pointer where a row holds NULL. For rows by the thousand, a chart's points: they come faster, and nobody turns rows into columns after.

## Each

```go
func Each[T any](ctx context.Context, r Runner, text string, args ...any) iter.Seq2[T, error]
```

Each is the rows a statement gives, one at a time as their parts arrive, from one snapshot; inside a transaction, all of them at once.

## One

```go
func One[T any](ctx context.Context, r Runner, text string, args ...any) (T, bool, error)
```

One is the one row a statement gives, and whether it gave one; two are Invalid, and a write that gave them rolls back.

## ReadRow

```go
func ReadRow[T any](columns []string, cells []any) (T, error)
```

ReadRow is a row of these columns and cells as a query reads it into T: a struct by its fields' columns, a map, or a single column's value. A table's search index reads its hits' rows with it.

## Scalar

```go
func Scalar[T any](ctx context.Context, r Runner, text string, args ...any) (T, error)
```

Scalar is the one value of the one row a statement gives, as count(\*) does; T's zero for NULL.

## Condition

```go
type Condition interface {
	// contains filtered or unexported methods
}
```

Condition is a condition a query keeps to: equal values by column, Eq, or a piece of SQL.

## DB

```go
type DB struct {
	// contains filtered or unexported fields
}
```

DB is an application's database. The verb says what comes back: all the rows, one or none, one value, or what a write changed. A write with returning goes through a read's verb and runs on the writer, its rows given once it is durable.

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, name string, opts ...Option) (*DB, error)
```

Open opens the application's database name, sql/\&lt;name>.db, once its migrations are applied and checked: a migration that fails, or one changed after it was applied, is told here, at the start. Without migrations the file opens as it is, an empty one when there is none.

### DB.Batch

```go
func (db *DB) Batch(ctx context.Context, statements ...Statement) error
```

Batch runs statements known before they run as one: all of them or none, in a shared commit.

### DB.CoreHome

```go
func (db *DB) CoreHome() core.HomeOf
```

CoreHome is how this module's packages reach the database as a handle's home; a program has no use for it.

### DB.Describe

```go
func (db *DB) Describe() string
```

Describe is the database as an error names it: sql app.

### DB.Exec

```go
func (db *DB) Exec(ctx context.Context, text string, args ...any) (Result, error)
```

Exec runs a write once it is durable; writes from many callers share a commit, each failing alone.

### DB.Handle

```go
func (db *DB) Handle(ctx context.Context, c *core.Connection) (uint64, error)
```

Handle is the database's handle on a connection, opened there first.

### DB.Keep

```go
func (db *DB) Keep(method uint16, body []byte, make func(ctx context.Context, c *core.Connection) ([]byte, error)) *core.Opener
```

Keep is the opener of a handle kept in the database's file: the one made first for the same open, so that its transaction opens each once, before it holds the writer.

### DB.Name

```go
func (db *DB) Name() string
```

Name is the database's name.

### DB.Tx

```go
func (db *DB) Tx(ctx context.Context, fn func(tx *Tx) error) error
```

Tx runs fn in a transaction that holds the writer, for writes that depend on what its reads found: an error fn returns rolls it back, and is Tx's. It runs once, five seconds at most, past which the store rolls it back; inside it, call nothing that waits on the world.

### DB.Via

```go
func (db *DB) Via(string) core.Via
```

Via is how a handle kept in the database's file reaches the store outside a transaction.

## Direction

```go
type Direction string
```

Direction is an order's direction.

### Asc

```go
const (
	Asc  Direction = "asc"
	Desc Direction = "desc"
)
```

The directions an order takes.

## Eq

```go
type Eq map[string]any
```

Eq is equal values by column, written in the order of the columns' names: nil is is null, and a slice in (...).

## ListOption

```go
type ListOption interface {
	// contains filtered or unexported methods
}
```

ListOption is how a page of rows reads.

### After

```go
func After(next string) ListOption
```

After reads the page after the one whose Next it is.

### Limit

```go
func Limit(n int) ListOption
```

Limit is how many rows a page holds: 100 unless given.

## Option

```go
type Option interface {
	// contains filtered or unexported methods
}
```

Option is how a database opens.

### Durability

```go
func Durability(level string) Option
```

Durability is how far a commit of the database goes before it returns: "full" or "os"; the store's own unless given.

### MigrationFiles

```go
func MigrationFiles(byName map[string]string) Option
```

MigrationFiles are a database's migrations as their text by their names.

### Migrations

```go
func Migrations(fsys fs.FS) Option
```

Migrations are a database's migrations: the .sql files at the root of fsys, named 0001\_notes.sql, applied in the order of their names and checked at every open. An embed.FS of migrations/\*.sql is fs.Sub(migrations, "migrations").

## Order

```go
type Order struct {
	Column    string
	Direction Direction
}
```

Order is an order a request chooses by name: sqldb.Order{"created\_at", sqldb.Desc}.

## Page

```go
type Page[T any] struct {
	Rows []T
	Next string
}
```

Page is a page of a query's rows, and the cursor that reads the next: "" after the last.

## Piece

```go
type Piece struct {
	// contains filtered or unexported fields
}
```

Piece is a piece of SQL and the values of its ?s, which are always values and never SQL: SQL makes one, and a call, a batch and a table's query take it.

### And

```go
func And(conditions ...Condition) Piece
```

And is conditions joined with and, in parentheses; an empty and finds everything.

### Has

```go
func Has(name string, value any) Piece
```

Has is a JSON list in column name holding value.

### Ident

```go
func Ident(name string) Piece
```

Ident is a name, quoted, never SQL: a column a request chooses.

### JSON

```go
func JSON(value any) Piece
```

JSON is a value kept as its JSON.

### List

```go
func List[T any](values []T) Piece
```

List is a list for in (...), as one value SQLite reads with json\_each: any length, the same statement.

### Or

```go
func Or(conditions ...Condition) Piece
```

Or is conditions joined with or, in parentheses; an empty or finds nothing.

### SQL

```go
func SQL(text string, values ...any) Piece
```

SQL is a piece of SQL from text and the values of its ?s; a piece or a query in a ?'s place is written in: sqldb.SQL("exists ?", paid).

### Piece.Text

```go
func (p Piece) Text() string
```

Text is the piece's SQL, and Values the values of its ?s.

### Piece.Values

```go
func (p Piece) Values() []any
```

Values are the values of the piece's ?s, as they leave.

## Query

```go
type Query[T any] struct {
	// contains filtered or unexported fields
}
```

Query is a query on a table: a value, each call giving a new one, so that the first stays as it was. All, One, Count, Each and List read; Update and Delete write.

### Select

```go
func Select[R any](q queried, columns string) *Query[R]
```

Select is a query's columns, as SQL text, and the type of the row they make.

### Query.All

```go
func (q *Query[T]) All(ctx context.Context) ([]T, error)
```

All is every row the query reads.

### Query.Count

```go
func (q *Query[T]) Count(ctx context.Context) (int64, error)
```

Count counts every row the conditions match, without the order and the limit.

### Query.Delete

```go
func (q *Query[T]) Delete(ctx context.Context) (Result, error)
```

Delete deletes the rows the conditions match, and says how many it deleted.

### Query.Each

```go
func (q *Query[T]) Each(ctx context.Context) iter.Seq2[T, error]
```

Each is the rows one at a time, as their parts arrive.

### Query.GroupBy

```go
func (q *Query[T]) GroupBy(columns ...string) *Query[T]
```

GroupBy groups the rows by columns, quoted as names.

### Query.Having

```go
func (q *Query[T]) Having(conditions ...Condition) *Query[T]
```

Having is a condition on the groups, as Where takes one.

### Query.Include

```go
func (q *Query[T]) Include(name string, query queried) *Query[T]
```

Include gives each row the rows of another query as a list under name, from the same statement: a field of that name holds them. The query names the row it belongs to in its where, by this table's alias, names its columns with Select, and has a limit, which counts for each row. Every value comes back as it is kept.

### Query.Join

```go
func (q *Query[T]) Join(table string, on any) *Query[T]
```

Join joins a table, "users as u", on a condition as SQL text or a piece of SQL.

### Query.LeftJoin

```go
func (q *Query[T]) LeftJoin(table string, on any) *Query[T]
```

LeftJoin joins a table whose columns may be NULL where no row of it meets the condition.

### Query.Limit

```go
func (q *Query[T]) Limit(rows int) *Query[T]
```

Limit is how many rows the query reads at most.

### Query.List

```go
func (q *Query[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error)
```

List is a page after the last row of the one before, by the query's order, which ends in a unique column so that no row is read twice or skipped; Next reads the next page, and only of this query.

### Query.Offset

```go
func (q *Query[T]) Offset(rows int) *Query[T]
```

Offset is how many rows the query skips.

### Query.One

```go
func (q *Query[T]) One(ctx context.Context) (T, bool, error)
```

One is the one row the query reads, and whether it read one; two are Invalid.

### Query.OrderBy

```go
func (q *Query[T]) OrderBy(column any, direction Direction) *Query[T]
```

OrderBy orders the rows by a column, quoted as a name, or by a piece of SQL: OrderBy(sqldb.SQL("lower(title)"), sqldb.Asc).

### Query.SQL

```go
func (q *Query[T]) SQL() (string, []any, error)
```

SQL is the text and the values the query sends, to read or to log.

### Query.Update

```go
func (q *Query[T]) Update(ctx context.Context, set Set) (Result, error)
```

Update sets columns of the rows the conditions match, and says how many changed.

### Query.Where

```go
func (q *Query[T]) Where(conditions ...Condition) *Query[T]
```

Where adds conditions with and: equal values, sqldb.Eq{"status": "paid"}, nil as is null and a slice as in (...); or a piece of SQL from sqldb.SQL.

## Result

```go
type Result struct {
	Changes         int64
	LastInsertRowid int64
}
```

Result is what a write changed: the rows, and SQLite's rowid of the row it inserted, 0 when none.

## Row

```go
type Row = map[string]any
```

Row is a row as SQLite gave it, by its columns' names.

## Runner

```go
type Runner interface {
	// contains filtered or unexported methods
}
```

Runner reads and writes a database: the database itself, or a transaction of it.

## Set

```go
type Set map[string]any
```

Set is the columns an update sets: values, or pieces of SQL.

## Statement

```go
type Statement = Piece
```

Statement is a statement of a batch: a piece of SQL.

## Table

```go
type Table[T any] struct {
	*Query[T]
	// contains filtered or unexported fields
}
```

Table is a table: its queries, and its rows' inserts and upserts. Its rows are T, a struct whose fields are the table's columns.

### OpenTable

```go
func OpenTable[T any](r Runner, name string) *Table[T]
```

OpenTable is a table of a database, or of a transaction of it, "orders" or "orders as o", typed where it opens. It costs nothing until its first call.

### Table.Insert

```go
func (t *Table[T]) Insert(ctx context.Context, row T) (T, error)
```

Insert inserts a row and gives it back as the file keeps it: the defaults and the generated columns the database filled.

### Table.InsertMany

```go
func (t *Table[T]) InsertMany(ctx context.Context, rows []T) ([]T, error)
```

InsertMany inserts rows of one shape as one write, all of them or none, and gives them back as the file keeps them.

### Table.Name

```go
func (t *Table[T]) Name() string
```

Name is the table's name.

### Table.Upsert

```go
func (t *Table[T]) Upsert(ctx context.Context, row T, opts ...UpsertOption) (Result, error)
```

Upsert inserts a row, or, when the columns Conflict names are taken, sets the columns Update names on the row that holds them, when that row meets Where: its Changes is 0 when Where kept it.

## Tx

```go
type Tx struct {
	// contains filtered or unexported fields
}
```

Tx is a database's transaction, from db.Tx. Its reads see its writes, a call that fails leaves it as it was before the call, and its calls go one at a time, in order. A bucket, a queue or a channel takes it in with In: bucket.In(tx), and what they write commits with the rows.

### Tx.CoreTx

```go
func (tx *Tx) CoreTx() core.TxOf
```

CoreTx is how this module's packages take a handle into the transaction; a program has no use for it.

### Tx.Database

```go
func (tx *Tx) Database() core.Database
```

Database is the database the transaction is of.

### Tx.Exec

```go
func (tx *Tx) Exec(ctx context.Context, text string, args ...any) (Result, error)
```

Exec runs a write in the transaction.

### Tx.Nested

```go
func (tx *Tx) Nested(ctx context.Context, method uint16, body []byte) ([]byte, error)
```

Nested is a call of kv, jobs or channels in the transaction: its method, its request, and the method's answer.

### Tx.Via

```go
func (tx *Tx) Via() core.Via
```

Via is how a handle's calls go inside the transaction, on its stream.

## UpsertOption

```go
type UpsertOption interface {
	// contains filtered or unexported methods
}
```

UpsertOption is what an upsert says: the columns whose taken values make it an update, the columns that update sets, and the condition the row that holds them must meet.

### Conflict

```go
func Conflict(columns ...string) UpsertOption
```

Conflict names the columns whose values, when taken, make an upsert an update.

### Update

```go
func Update(columns ...string) UpsertOption
```

Update names the columns an upsert's update sets from the row.

### Where

```go
func Where(c Condition) UpsertOption
```

Where is the condition the row that holds an upsert's key must meet, its owner's, so that an id from a request changes nothing of another's row.

<!-- Generated by task reference from sdk/go/sqldb/. Edit the doc comments there, not this file. -->
