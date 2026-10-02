package sqldb

import (
	"context"
	"database/sql"
	"fmt"
	"iter"
	"reflect"

	"github.com/tinyshed/tinystore"
)

var (
	errTwoRows  = fmt.Errorf("%w: two rows, and One reads one; All reads several", tinystore.ErrInvalid)
	errNoScalar = fmt.Errorf("%w: no row, and Scalar reads one; a query that may find nothing asks One",
		tinystore.ErrInvalid)
)

// One reads the one row query returns into T: a struct takes each column into
// the field of its name, and any other T takes the only column. found is
// false for no row; two are tinystore.ErrInvalid.
func One[T any](ctx context.Context, h Handle, query string, args ...any) (T, bool, error) {
	return one[T](ctx, h, &call{query: query, args: args})
}

// All reads every row query returns, holding them in the store's memory as it
// decodes them. Past 64 MiB of them it is tinystore.ErrLimit, and Each reads
// them instead.
func All[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error) {
	return all[T](ctx, h, &call{query: query, args: args, bound: allBound})
}

// Each decodes the rows query returns a row at a time, from one snapshot held
// at most five seconds, which a break lets go; an export longer than that
// pages by key. An error ends the rows as their last element.
func Each[T any](ctx context.Context, h Handle, query string, args ...any) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		stopped := false
		c := &call{query: query, args: args, snapshot: true}
		c.rows = func(rows *sql.Rows, held *held) error {
			var reader *rowReader[T]
			for rows.Next() {
				var value T
				if err := readRow(rows, &reader, held.holdRow, &value); err != nil {
					return err
				}
				if !yield(value, nil) {
					stopped = true
					return nil
				}
			}
			return rows.Err()
		}
		if err := h.run(ctx, c); err != nil && !stopped {
			var zero T
			yield(zero, err)
		}
	}
}

// Scalar reads one column of one row, for a query that always answers, as
// count(*) does; a query that may find nothing asks One.
func Scalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return scalar[T](ctx, h, &call{query: query, args: args})
}

// ExecOne runs a write on the writer and reads the one row its returning
// clause gives, as One reads a query's.
func ExecOne[T any](ctx context.Context, h Handle, query string, args ...any) (T, bool, error) {
	return one[T](ctx, h, &call{query: query, args: args, write: true})
}

// ExecAll runs a write on the writer and reads every row its returning clause
// gives, as All reads a query's.
func ExecAll[T any](ctx context.Context, h Handle, query string, args ...any) ([]T, error) {
	return all[T](ctx, h, &call{query: query, args: args, write: true, bound: allBound})
}

// ExecScalar runs a write on the writer and reads the one column of the one
// row its returning clause gives, such as the id of insert … returning id.
func ExecScalar[T any](ctx context.Context, h Handle, query string, args ...any) (T, error) {
	return scalar[T](ctx, h, &call{query: query, args: args, write: true})
}

// Rows is what a query returns without a struct to take it: the columns' names,
// and each row's values as SQLite returned them, a column each.
//
// A value is nil, an int64, a float64, a string or a []byte, what the row
// keeps whatever its column declares.
type Rows struct {
	Columns []string
	Values  [][]any
}

// Query reads every row query returns as SQLite returns it, for a program that
// has no struct for them, as the server has none for its clients. The rows are
// held in the store's memory as All holds them.
func Query(ctx context.Context, h Handle, query string, args ...any) (Rows, error) {
	return rowsOf(ctx, h, &call{query: query, args: args, bound: allBound})
}

// ExecQuery runs a write on the writer and reads the rows its returning clause
// gives, as Query reads a query's. A write that returns none answers its
// columns alone, or none.
func ExecQuery(ctx context.Context, h Handle, query string, args ...any) (Rows, error) {
	return rowsOf(ctx, h, &call{query: query, args: args, write: true, bound: allBound})
}

func rowsOf(ctx context.Context, h Handle, c *call) (Rows, error) {
	answer := Rows{Values: [][]any{}}
	c.rows = func(rows *sql.Rows, held *held) error {
		columns, err := rows.Columns()
		if err != nil {
			return err
		}
		answer.Columns = columns
		for rows.Next() {
			values, weight, err := scanValues(rows, len(columns))
			if err == nil {
				err = held.take(weight)
			}
			if err != nil {
				return err
			}
			answer.Values = append(answer.Values, values)
		}
		return rows.Err()
	}
	if err := h.run(ctx, c); err != nil {
		return Rows{}, err
	}
	return answer, nil
}

// scanValues is a row's values as the driver gives them, and what they weigh
func scanValues(rows *sql.Rows, columns int) ([]any, int64, error) {
	values := make([]any, columns)
	into := make([]any, columns)
	for i := range values {
		into[i] = &values[i]
	}
	if err := rows.Scan(into...); err != nil {
		return nil, 0, err
	}
	weight := int64(24 * columns)
	for _, value := range values {
		switch held := value.(type) {
		case string:
			weight += int64(len(held))
		case []byte:
			weight += int64(len(held))
		}
	}
	return values, weight, nil
}

func one[T any](ctx context.Context, h Handle, c *call) (T, bool, error) {
	var value T
	found := false
	c.rows = func(rows *sql.Rows, held *held) error {
		if !rows.Next() {
			return rows.Err()
		}
		var reader *rowReader[T]
		if err := readRow(rows, &reader, held.take, &value); err != nil {
			return err
		}
		if rows.Next() {
			return errTwoRows
		}
		found = true
		return rows.Err()
	}
	if err := h.run(ctx, c); err != nil {
		var zero T
		return zero, false, err
	}
	return value, found, nil
}

func all[T any](ctx context.Context, h Handle, c *call) ([]T, error) {
	values := make([]T, 0)
	c.rows = func(rows *sql.Rows, held *held) error {
		var reader *rowReader[T]
		for rows.Next() {
			var value T
			if err := readRow(rows, &reader, held.take, &value); err != nil {
				return err
			}
			values = append(values, value)
		}
		return rows.Err()
	}
	if err := h.run(ctx, c); err != nil {
		return nil, err
	}
	return values, nil
}

func scalar[T any](ctx context.Context, h Handle, c *call) (T, error) {
	var value T
	c.rows = func(rows *sql.Rows, held *held) error {
		if isRecord(reflect.TypeFor[T]()) {
			return fmt.Errorf("%w: Scalar reads one column into a value; One reads a row into a %s",
				tinystore.ErrInvalid, reflect.TypeFor[T]())
		}
		if !rows.Next() {
			if err := rows.Err(); err != nil {
				return err
			}
			return errNoScalar
		}
		var reader *rowReader[T]
		if err := readRow(rows, &reader, held.take, &value); err != nil {
			return err
		}
		if rows.Next() {
			return errTwoRows
		}
		return rows.Err()
	}
	if err := h.run(ctx, c); err != nil {
		var zero T
		return zero, err
	}
	return value, nil
}

// readRow decodes the row rows stands on into value, making the reader on the
// first row, and counts the row's bytes before it decodes them
func readRow[T any](rows *sql.Rows, reader **rowReader[T], count func(int64) error, value *T) error {
	if *reader == nil {
		made, err := newRowReader[T](rows)
		if err != nil {
			return err
		}
		*reader = made
	}
	weight, err := (*reader).scan(rows)
	if err == nil {
		err = count(weight)
	}
	if err == nil {
		err = (*reader).decode(value)
	}
	return err
}
