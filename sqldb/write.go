package sqldb

import (
	"context"
	"database/sql"
	"fmt"
	"reflect"

	"github.com/tinyshed/tinystore"
)

// Exec runs one statement that may write on the file's one writer, and returns
// once it is durable. Statements from many goroutines commit together, each in
// its savepoint of one transaction, with one fsync: one that fails rolls back
// alone, one whose caller's context ends before its turn writes nothing, and a
// group whose commit fails answers ErrOutcomeUnknown.
func (d *DB) Exec(ctx context.Context, query string, args ...any) (sql.Result, error) {
	c := &call{query: query, args: args, write: true}
	err := d.run(ctx, c)
	return c.result, err
}

// Insert writes row into table in one statement, grouped like any Exec: every
// column but the generated ones as its field holds it, false as 0 and nil as
// NULL. It returns the row as the file keeps it, a time to the millisecond in
// UTC, with what the database generated: the rowid its result carries, and
// the generated columns the insert returns.
//
//	INSERT INTO notes (id, title, done) VALUES (?, ?, ?)
func Insert[T any](ctx context.Context, h Handle, table *TableDef[T], row T) (T, error) {
	var zero T
	args, err := table.arguments(row)
	if err != nil {
		return zero, err
	}
	inserted, err := table.asStored(row)
	if err != nil {
		return zero, err
	}
	c := &call{query: table.insert, args: args, encoded: true, write: true}
	if len(table.returned) > 0 {
		c.rows = func(rows *sql.Rows, held *held) error { return readReturned(rows, held, &inserted) }
	}
	if err = h.run(ctx, c); err != nil {
		return zero, err
	}
	if err = table.number(&inserted, c.result); err != nil {
		return zero, err
	}
	return inserted, nil
}

// readReturned fills the generated columns an insert returned into the row it
// wrote, which keeps its other fields
func readReturned[T any](rows *sql.Rows, held *held, inserted *T) error {
	if !rows.Next() {
		return rows.Err()
	}
	var reader *rowReader[T]
	if err := readRow(rows, &reader, held.take, inserted); err != nil {
		return err
	}
	return rows.Err()
}

// number gives the row the rowid SQLite numbered it by, when the insert's
// result carries it
func (d *TableDef[T]) number(inserted *T, result sql.Result) error {
	if d.numbered == nil || result == nil {
		return nil
	}
	id, err := result.LastInsertId()
	if err != nil {
		return err
	}
	v, err := settable(reflect.ValueOf(inserted).Elem(), d.numbered.field.index)
	if err != nil {
		return err
	}
	return d.numbered.field.value.decode(v, id)
}

// asStored is row as the file keeps it: a time to the millisecond in UTC, a
// duration to the millisecond; a pointer to either is a new one, so that the
// caller's value stays as it was
func (d *TableDef[T]) asStored(row T) (T, error) {
	root := reflect.ValueOf(&row).Elem()
	for _, c := range d.written {
		if c.logical != kindTime && c.logical != kindDuration {
			continue
		}
		v, present := readable(root, c.field.index)
		if !present || !v.CanSet() {
			continue
		}
		stored, err := c.field.value.encode(v, false)
		if err != nil || stored == nil {
			continue
		}
		if c.field.value.wrap == pointer {
			v.SetZero()
		}
		if err = c.field.value.decode(v, stored); err != nil {
			return row, fmt.Errorf("%w: insert into %s: %s: %w", tinystore.ErrInvalid, d.table.name, c.field.name, err)
		}
	}
	return row, nil
}

// arguments are row's fields as their columns hold them, in the order of the
// insert's columns; a field a nil embedded pointer stands for is its zero value
func (d *TableDef[T]) arguments(row T) ([]any, error) {
	root := reflect.ValueOf(&row).Elem()
	args := make([]any, len(d.written))
	for i, c := range d.written {
		v, present := readable(root, c.field.index)
		if !present {
			v = reflect.Zero(c.field.typ)
		}
		value, err := c.field.value.encode(v, c.logical == kindUUID && c.storage == Blob)
		if err != nil {
			return nil, fmt.Errorf("%w: insert into %s: %s: %w", tinystore.ErrInvalid, d.table.name, c.field.name, err)
		}
		args[i] = value
	}
	return args, nil
}
