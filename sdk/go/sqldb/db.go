// Package sqldb is the application's own SQLite databases, sql/<name>.db, as
// docs/guides/sql/README.md has them. The application writes its SQL, or builds it
// with a table's queries, and the store owns the file, its connections and
// its migrations, which every open applies and checks. A statement leaves as
// its text and the values of its ?s, each as SQLite keeps it.
//
//	db, err := sqldb.Open(ctx, store, "app", sqldb.Migrations(migrations))
//	mine, err := sqldb.All[Note](ctx, db, `select * from notes where author_id = ?`, userID)
//	result, err := db.Exec(ctx, `update notes set done = 1 where id = ?`, id)
package sqldb

import (
	"context"
	"encoding/binary"
	"io/fs"
	"iter"
	"math"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"sync"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// Option is how a database opens.
type Option interface{ open(*openOptions) }

type openOptions struct {
	migrations fs.FS
	files      map[string]string
	durability string
}

type migrations struct{ fsys fs.FS }

// Migrations are a database's migrations: the .sql files at the root of
// fsys, named 0001_notes.sql, applied in the order of their names and checked
// at every open. An embed.FS of migrations/*.sql is fs.Sub(migrations, "migrations").
func Migrations(fsys fs.FS) Option { return migrations{fsys} }

func (m migrations) open(o *openOptions) { o.migrations = m.fsys }

type files map[string]string

// MigrationFiles are a database's migrations as their text by their names.
func MigrationFiles(byName map[string]string) Option { return files(byName) }

func (f files) open(o *openOptions) { o.files = f }

type durability string

// Durability is how far a commit of the database goes before it returns:
// "full" or "os"; the store's own unless given.
func Durability(level string) Option { return durability(level) }

func (d durability) open(o *openOptions) { o.durability = string(d) }

// DB is an application's database. The verb says what comes back: all the
// rows, one or none, one value, or what a write changed. A write with
// returning goes through a read's verb and runs on the writer, its rows given
// once it is durable.
type DB struct {
	store    *core.Store
	name     string
	describe string
	opener   *core.Opener
	mu       sync.Mutex
	// guests are the buckets, queues and indexes kept in the file, by their
	// open: a transaction opens them first
	guests map[string]*core.Opener
}

// Open opens the application's database name, sql/<name>.db, once its
// migrations are applied and checked: a migration that fails, or one changed
// after it was applied, is told here, at the start. Without migrations the
// file opens as it is, an empty one when there is none.
func Open(ctx context.Context, store *tinystore.Store, name string, opts ...Option) (*DB, error) {
	var o openOptions
	for _, opt := range opts {
		opt.open(&o)
	}
	if err := core.CheckName(name, "database"); err != nil {
		return nil, err
	}
	if o.durability != "" && o.durability != "full" && o.durability != "os" {
		return nil, core.Errorf(core.Invalid, "sql %s: durability %q: it is \"full\" or \"os\"", name, o.durability)
	}
	message := wire.SqlOpen{Name: name}
	if o.durability != "" {
		message.Durability = &o.durability
	}
	migrated, err := migrationsOf(o)
	if err != nil {
		return nil, core.Wrap(core.Invalid, err, "sql %s: its migrations", name)
	}
	if migrated != nil {
		message.Migrations = &migrated
	}
	db := &DB{
		store: store.CoreHome().Store, name: name, describe: "sql " + name,
		opener: &core.Opener{Method: wire.MethodSqlOpen, Body: wire.Encode(&message)},
		guests: map[string]*core.Opener{},
	}
	if err := db.store.Link.Run(ctx, core.Reading, func(c *core.Connection) error {
		_, err := db.Handle(ctx, c)
		return err
	}); err != nil {
		return nil, err
	}
	return db, nil
}

func migrationsOf(o openOptions) ([]wire.SqlMigration, error) {
	var out []wire.SqlMigration
	switch {
	case o.files != nil:
		for name, text := range o.files {
			out = append(out, wire.SqlMigration{Name: name, Sql: text})
		}
	case o.migrations != nil:
		entries, err := fs.ReadDir(o.migrations, ".")
		if err != nil {
			return nil, err
		}
		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
				continue
			}
			text, err := fs.ReadFile(o.migrations, entry.Name())
			if err != nil {
				return nil, err
			}
			out = append(out, wire.SqlMigration{Name: entry.Name(), Sql: string(text)})
		}
	default:
		return nil, nil
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	if out == nil {
		out = []wire.SqlMigration{}
	}
	return out, nil
}

// Name is the database's name.
func (db *DB) Name() string { return db.name }

// CoreHome is how this module's packages reach the database as a handle's
// home; a program has no use for it.
func (db *DB) CoreHome() core.HomeOf { return core.HomeOf{Store: db.store, Database: db} }

// Describe is the database as an error names it: sql app.
func (db *DB) Describe() string { return db.describe }

// Handle is the database's handle on a connection, opened there first.
func (db *DB) Handle(ctx context.Context, c *core.Connection) (uint64, error) {
	return core.HandleOn(ctx, c, db.opener)
}

// Keep is the opener of a handle kept in the database's file: the one made
// first for the same open, so that its transaction opens each once, before
// it holds the writer.
func (db *DB) Keep(method uint16, body []byte, make func(ctx context.Context, c *core.Connection) ([]byte, error)) *core.Opener {
	db.mu.Lock()
	defer db.mu.Unlock()
	key := string(rune(method)) + "\x00" + string(body)
	if found, ok := db.guests[key]; ok {
		return found
	}
	opener := &core.Opener{Method: method, Body: body, Make: make}
	db.guests[key] = opener
	return opener
}

// Via is how a handle kept in the database's file reaches the store outside a transaction.
func (db *DB) Via(string) core.Via { return core.ViaLink{Link: db.store.Link} }

// Result is what a write changed: the rows, and SQLite's rowid of the row it
// inserted, 0 when none.
type Result struct {
	Changes         int64
	LastInsertRowid int64
}

func resultOf(done wire.SqlDone) Result {
	return Result{int64(done.Changes), done.LastInsertRowid}
}

// Runner reads and writes a database: the database itself, or a transaction of it.
type Runner interface {
	rows(ctx context.Context, statement Piece, want string) (answered, error)
	exec(ctx context.Context, statement Piece) (Result, error)
	each(ctx context.Context, statement Piece) iter.Seq2[[]any, error]
	describeIt() string
}

// answered is what one query answered: its columns, and its rows' values.
type answered struct {
	columns []string
	rows    [][]any
}

func (db *DB) describeIt() string { return db.describe }

var writes = regexp.MustCompile(`(?i)\b(insert|update|delete|replace|returning|create|drop|alter|vacuum|reindex|analyze)\b`)

// idempotenceOf is whether a statement may write, which says whether a lost
// connection may send it again.
func idempotenceOf(text string) core.Idempotence {
	if writes.MatchString(text) {
		return core.Writing
	}
	return core.Reading
}

func (db *DB) rows(ctx context.Context, statement Piece, want string) (answered, error) {
	if statement.err != nil {
		return answered{}, statement.err
	}
	parts, err := db.download(ctx, statement, wire.MethodSqlQuery, func(handle uint64) []byte {
		return wire.Encode(&wire.SqlQuery{Handle: handle, Text: statement.text, Values: statement.values, Want: want})
	})
	if err != nil {
		return answered{}, err
	}
	return joined(parts)
}

func joined(parts [][]byte) (answered, error) {
	var out answered
	for _, body := range parts {
		var part wire.SqlRows
		if err := decode(body, &part); err != nil {
			return out, err
		}
		if out.columns == nil {
			out.columns = part.Columns
		}
		for _, row := range part.Rows {
			out.rows = append(out.rows, row)
		}
	}
	return out, nil
}

func (db *DB) download(ctx context.Context, statement Piece, method uint16, body func(handle uint64) []byte) ([][]byte, error) {
	var parts [][]byte
	err := db.store.Link.Run(ctx, idempotenceOf(statement.text), func(c *core.Connection) error {
		handle, err := db.Handle(ctx, c)
		if err != nil {
			return err
		}
		_, items, trailer, err := core.Download(ctx, c, method, body(handle))
		parts = append(items, trailer)
		return err
	})
	return parts, err
}

func (db *DB) exec(ctx context.Context, statement Piece) (Result, error) {
	if statement.err != nil {
		return Result{}, statement.err
	}
	var done wire.SqlDone
	err := db.store.Link.Run(ctx, core.Writing, func(c *core.Connection) error {
		handle, err := db.Handle(ctx, c)
		if err != nil {
			return err
		}
		answer, err := c.Session.Call(ctx, wire.MethodSqlExec, wire.Encode(&wire.SqlStatement{Handle: handle, Text: statement.text, Values: statement.values}))
		if err != nil {
			return err
		}
		return decode(answer, &done)
	})
	return resultOf(done), err
}

func (db *DB) each(ctx context.Context, statement Piece) iter.Seq2[[]any, error] {
	return func(yield func([]any, error) bool) {
		if statement.err != nil {
			yield(nil, statement.err)
			return
		}
		c, err := db.store.Link.Connection(ctx)
		if err != nil {
			yield(nil, err)
			return
		}
		handle, err := db.Handle(ctx, c)
		if err != nil {
			yield(nil, err)
			return
		}
		body := wire.Encode(&wire.SqlQuery{Handle: handle, Text: statement.text, Values: statement.values, Want: "all"})
		stream, err := c.Session.Open(ctx, wire.MethodSqlQuery, body, true)
		if err != nil {
			yield(nil, err)
			return
		}
		defer stream.Cancel()
		head, err := stream.Next(ctx)
		if err != nil {
			yield(nil, err)
			return
		}
		var columns []string
		read := func(body []byte) bool {
			var part wire.SqlRows
			if err := decode(body, &part); err != nil {
				yield(nil, err)
				return false
			}
			if columns == nil {
				columns = part.Columns
				if err := checkColumns(columns); err != nil {
					yield(nil, err)
					return false
				}
				if !yield(append([]any{columnsMark{}}, toAny(columns)...), nil) {
					return false
				}
			}
			for _, row := range part.Rows {
				if !yield(row, nil) {
					return false
				}
			}
			return true
		}
		if head.End {
			read(head.Body)
			return
		}
		for {
			event, err := stream.Next(ctx)
			if err != nil {
				yield(nil, err)
				return
			}
			stream.Consumed(len(event.Body))
			if !read(event.Body) || event.End {
				return
			}
		}
	}
}

// columnsMark heads the first item each yields: the columns, before the rows.
type columnsMark struct{}

func toAny(names []string) []any {
	out := make([]any, len(names))
	for i, n := range names {
		out[i] = n
	}
	return out
}

// statementOf is a statement as a piece: its text and the values of its ?s,
// a piece or a query in a ?'s place written in.
func statementOf(text string, args []any) Piece { return SQL(text, args...) }

// All is every row a statement gives, read into T: a struct, a map, or one
// column's value.
func All[T any](ctx context.Context, r Runner, text string, args ...any) ([]T, error) {
	got, err := r.rows(ctx, statementOf(text, args), "all")
	if err != nil {
		return nil, err
	}
	return readAll[T](got)
}

func readAll[T any](got answered) ([]T, error) {
	if err := checkColumns(got.columns); err != nil {
		return nil, err
	}
	out := make([]T, 0, len(got.rows))
	for _, values := range got.rows {
		row, err := readRow[T](got.columns, values)
		if err != nil {
			return nil, err
		}
		out = append(out, row)
	}
	return out, nil
}

// One is the one row a statement gives, and whether it gave one; two are
// Invalid, and a write that gave them rolls back.
func One[T any](ctx context.Context, r Runner, text string, args ...any) (T, bool, error) {
	var zero T
	got, err := r.rows(ctx, statementOf(text, args), "one")
	if err != nil || len(got.rows) == 0 {
		return zero, false, err
	}
	rows, err := readAll[T](answered{got.columns, got.rows[:1]})
	if err != nil {
		return zero, false, err
	}
	return rows[0], true, nil
}

// Scalar is the one value of the one row a statement gives, as count(*) does;
// T's zero for NULL.
func Scalar[T any](ctx context.Context, r Runner, text string, args ...any) (T, error) {
	var out T
	got, err := r.rows(ctx, statementOf(text, args), "scalar")
	if err != nil || len(got.rows) == 0 || len(got.rows[0]) == 0 {
		return out, err
	}
	column := "the value"
	if len(got.columns) > 0 {
		column = got.columns[0]
	}
	err = readValue(reflect.ValueOf(&out).Elem(), got.rows[0][0], column)
	return out, err
}

// Each is the rows a statement gives, one at a time as their parts arrive,
// from one snapshot; inside a transaction, all of them at once.
func Each[T any](ctx context.Context, r Runner, text string, args ...any) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		var zero T
		var columns []string
		for values, err := range r.each(ctx, statementOf(text, args)) {
			if err != nil {
				yield(zero, err)
				return
			}
			if len(values) > 0 {
				if _, mark := values[0].(columnsMark); mark {
					columns = make([]string, len(values)-1)
					for i, v := range values[1:] {
						columns[i] = v.(string)
					}
					continue
				}
			}
			row, err := readRow[T](columns, values)
			if !yield(row, err) || err != nil {
				return
			}
		}
	}
}

// Columns is what All gives, by its columns: T is a struct whose fields are
// slices, one a column, by the column's name; a nil pointer where a row holds
// NULL. For rows by the thousand, a chart's points: they come faster, and
// nobody turns rows into columns after.
func Columns[T any](ctx context.Context, db *DB, text string, args ...any) (T, error) {
	var out T
	statement := statementOf(text, args)
	if statement.err != nil {
		return out, statement.err
	}
	parts, err := db.download(ctx, statement, wire.MethodSqlColumns, func(handle uint64) []byte {
		return wire.Encode(&wire.SqlStatement{Handle: handle, Text: statement.text, Values: statement.values})
	})
	if err != nil {
		return out, err
	}
	names, columns, err := columnsOf(parts)
	if err != nil {
		return out, err
	}
	into := reflect.ValueOf(&out).Elem()
	if into.Kind() != reflect.Struct {
		return out, core.Errorf(core.Invalid, "columns read into a struct of slices, not %s", into.Type())
	}
	f := fieldsOf(into.Type())
	for i, name := range names {
		index, ok := f.byColumn[name]
		if !ok {
			continue
		}
		field := into.FieldByIndex(index)
		if field.Kind() != reflect.Slice {
			return out, core.Errorf(core.Invalid, "column %s reads into a slice, not %s", name, field.Type())
		}
		slice := reflect.MakeSlice(field.Type(), len(columns[i]), len(columns[i]))
		for row, value := range columns[i] {
			if err := readValue(slice.Index(row), value, name); err != nil {
				return out, err
			}
		}
		field.Set(slice)
	}
	return out, nil
}

// columnsOf is a statement's rows by their columns, from the parts of its answer.
func columnsOf(parts [][]byte) ([]string, [][]any, error) {
	var names []string
	var columns [][]any
	for _, body := range parts {
		var part wire.SqlColumns
		if err := decode(body, &part); err != nil {
			return nil, nil, err
		}
		for index, held := range part.Columns {
			if len(columns) <= index {
				names = append(names, held.Name)
				columns = append(columns, nil)
			}
			switch {
			case held.Reals != nil:
				for at := 0; at+8 <= len(*held.Reals); at += 8 {
					real := math.Float64frombits(binary.LittleEndian.Uint64((*held.Reals)[at:]))
					if math.IsNaN(real) {
						// SQLite keeps no NaN: the store writes one where a row holds NULL
						columns[index] = append(columns[index], nil)
					} else {
						columns[index] = append(columns[index], real)
					}
				}
			case held.Integers != nil:
				for row, at := 0, 0; at+8 <= len(*held.Integers); row, at = row+1, at+8 {
					if held.Nulls != nil && (*held.Nulls)[row>>3]>>(row&7)&1 == 1 {
						columns[index] = append(columns[index], nil)
					} else {
						columns[index] = append(columns[index], int64(binary.LittleEndian.Uint64((*held.Integers)[at:])))
					}
				}
			case held.Values != nil:
				columns[index] = append(columns[index], *held.Values...)
			}
		}
	}
	return names, columns, checkColumns(names)
}

// Exec runs a write once it is durable; writes from many callers share a
// commit, each failing alone.
func (db *DB) Exec(ctx context.Context, text string, args ...any) (Result, error) {
	return db.exec(ctx, statementOf(text, args))
}

// Batch runs writes known before they run as one: all of them or none, in a
// shared commit. A write is a statement, or a job's add of a queue opened
// from the database, its Adding, which commits with the rows.
//
//	err = db.Batch(ctx, sqldb.SQL(`update notes set body = ? where id = ?`, body, id), indexing.Adding(Index{NoteID: id}))
func (db *DB) Batch(ctx context.Context, statements ...Statement) error {
	for _, each := range statements {
		if each.lent != nil {
			if err := db.takes(each.lent); err != nil {
				return err
			}
		} else if each.err != nil {
			return each.err
		}
	}
	return db.store.Link.Run(ctx, core.Writing, func(c *core.Connection) error {
		handle, err := db.Handle(ctx, c)
		if err != nil {
			return err
		}
		texts := make([]wire.SqlText, len(statements))
		for i, each := range statements {
			if texts[i], err = textOf(ctx, c, each); err != nil {
				return err
			}
		}
		_, err = c.Session.Call(ctx, wire.MethodSqlBatch, wire.Encode(&wire.SqlBatch{Handle: handle, Statements: texts}))
		return err
	})
}

// takes refuses a write that cannot be made, and one of a handle kept
// outside the database, whose write would not commit with its rows.
func (db *DB) takes(lent *core.Lent) error {
	if lent.Err != nil {
		return lent.Err
	}
	if lent.Home != core.Database(db) {
		kept := "jobs.db"
		if lent.Home != nil {
			kept = lent.Home.Describe()
		}
		return core.Errorf(core.Invalid, "%s: %s: kept in %s, outside this batch of %s", db.describe, lent.What, kept, db.describe)
	}
	return nil
}

// textOf is a write of a batch as it goes on c: a statement's text and
// values, or another engine's call, its handle opened on c first.
func textOf(ctx context.Context, c *core.Connection, write Statement) (wire.SqlText, error) {
	if write.lent == nil {
		return wire.SqlText{Text: write.text, Values: write.values}, nil
	}
	handle, err := core.HandleOn(ctx, c, write.lent.Opener)
	if err != nil {
		return wire.SqlText{}, err
	}
	method, body := uint64(write.lent.Method), write.lent.Body(handle)
	return wire.SqlText{Method: &method, Body: &body}, nil
}

func decode(answer []byte, m wire.Message) error {
	if err := wire.Decode(answer, m); err != nil {
		return core.Wrap(core.Protocol, err, "an answer that does not read")
	}
	return nil
}
