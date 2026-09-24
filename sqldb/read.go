package sqldb

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"reflect"

	sqlite3 "modernc.org/sqlite"
	sqlitelib "modernc.org/sqlite/lib"

	"github.com/tinyshed/tinystore"
)

var (
	ErrManyRows = fmt.Errorf("%w: more than one row", tinystore.ErrInvalid)
	ErrShape    = fmt.Errorf("%w: columns do not fit the destination", tinystore.ErrInvalid)
)

// Handle is where a call runs: a *DB, or the *Tx of a DB.Tx.
type Handle interface {
	read(ctx context.Context, work func(querier) error) error
	write(ctx context.Context, work func(querier) error) error
}

func (t *Tx) read(_ context.Context, work func(querier) error) error  { return work(t.q) }
func (t *Tx) write(_ context.Context, work func(querier) error) error { return work(t.q) }

// One reads exactly one row: none is sql.ErrNoRows, two are ErrManyRows.
func One[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return collect(ctx, h.read, one[T], query, args)
}

func All[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error) {
	return collect(ctx, h.read, all[T], query, args)
}

// Scalar reads one row of one column, such as a count.
func Scalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return collect(ctx, h.read, scalar[T], query, args)
}

// ExecOne writes on the writer and reads back the one row its returning
// clause gives: none is sql.ErrNoRows.
func ExecOne[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return collect(ctx, h.write, one[T], query, args)
}

func ExecAll[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error) {
	return collect(ctx, h.write, all[T], query, args)
}

// ExecScalar writes on the writer and reads back one column, such as the id
// of `insert … returning id`.
func ExecScalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return collect(ctx, h.write, scalar[T], query, args)
}

func collect[R any](
	ctx context.Context, run func(context.Context, func(querier) error) error,
	shape func(*sql.Rows) (R, error), query string, args []any,
) (R, error) {
	var result R
	err := run(ctx, func(q querier) error {
		rows, err := q.QueryContext(ctx, query, args...) //nolint:rowserrcheck // every shape checks Err
		if err != nil {
			return err
		}
		defer rows.Close()
		if result, err = shape(rows); err != nil {
			return err
		}
		return rows.Close()
	})
	return result, err
}

func one[T any](rows *sql.Rows) (T, error) {
	var zero T
	if !rows.Next() {
		if err := rows.Err(); err != nil {
			return zero, err
		}
		return zero, sql.ErrNoRows
	}
	value, err := scanRow[T](rows)
	if err != nil {
		return zero, err
	}
	if rows.Next() {
		return zero, ErrManyRows
	}
	return value, rows.Err()
}

func all[T any](rows *sql.Rows) ([]T, error) {
	var out []T
	for rows.Next() {
		value, err := scanRow[T](rows)
		if err != nil {
			return nil, err
		}
		out = append(out, value)
	}
	return out, rows.Err()
}

func scalar[T any](rows *sql.Rows) (T, error) {
	var zero T
	if isRecord(reflect.TypeFor[T]()) {
		return zero, fmt.Errorf("%w: Scalar reads one column, One reads a %s", ErrShape, reflect.TypeFor[T]())
	}
	return one[T](rows)
}

// explain turns a write that reached a reader, which refuses it with
// SQLITE_READONLY before writing anything, into the call to use instead
func (d *DB) explain(err error) error {
	var failure *sqlite3.Error
	if errors.As(err, &failure) && failure.Code()&0xff == sqlitelib.SQLITE_READONLY {
		return fmt.Errorf("%w: sql %q: a read cannot write; use Exec or an Exec form: %w",
			tinystore.ErrInvalid, d.name, err)
	}
	return err
}
