package sqldb

import (
	"context"
	"fmt"
	"sort"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
)

// MirrorOptions is how a mirror opens: what names a row at the source, and
// what marks and compares a row.
type MirrorOptions struct {
	// Key is the columns that name a row at the source: a unique index or the
	// table's primary key covers them.
	Key []string
	// Gone is a column that marks what the source dropped with the store's
	// time, unix milliseconds, instead of deleting it; empty deletes it.
	Gone string
	// Compare is the columns whose change makes a row changed; every column
	// the rows carry but the key's when nil.
	Compare []string
}

// Mirror is a table that mirrors another system: its Replace takes the
// source's list of a scope and its Patch a change of some rows, and each
// answers what was added, changed and gone. A row the list gives as it is
// held is not written at all.
type Mirror[T any] struct {
	db      *DB
	table   string
	options MirrorOptions
}

// Diff is what a mirror's call changed; a Suspect one wrote nothing.
type Diff[T any] struct {
	Added   []T
	Changed []Changed[T]
	Gone    []T
	Same    int
	Suspect bool
}

// Changed is a row that changed: as the table held it, and as the source
// gave it.
type Changed[T any] struct {
	Before T
	After  T
}

// ReplaceOption guards a Replace.
type ReplaceOption func(*float64)

// Shrink writes nothing, and answers Suspect, when the list would leave fewer
// than fraction of the scope's live rows live.
func Shrink(fraction float64) ReplaceOption { return func(shrink *float64) { *shrink = fraction } }

// OpenMirror is the table name of db mirroring another system, its rows Ts:
//
//	works := sqldb.OpenMirror[Work](db, "works", sqldb.MirrorOptions{Key: []string{"course_id", "id"}, Gone: "gone_at"})
//	diff, err := works.Replace(ctx, map[string]any{"course_id": 1}, fetched)
func OpenMirror[T any](db *DB, table string, options MirrorOptions) *Mirror[T] {
	return &Mirror[T]{db: db, table: table, options: options}
}

// Replace makes the scope, the rows whose columns hold its values, nil for
// the whole table, what rows lists: it adds what the table lacks, writes what
// changed, and deletes, or marks gone, what the list leaves out. A row
// outside the scope is invalid. Values compare as SQLite's is does, so "4" is
// not 4.
func (m *Mirror[T]) Replace(ctx context.Context, scope map[string]any, rows []T, options ...ReplaceOption) (Diff[T], error) {
	columns := make([]string, 0, len(scope))
	for column := range scope {
		columns = append(columns, column)
	}
	sort.Strings(columns)
	values := make([]wire.Cell, len(columns))
	for i, column := range columns {
		cell, err := cellOf(scope[column], "the scope's "+column)
		if err != nil {
			return Diff[T]{}, err
		}
		values[i] = cell
	}
	asked := wire.SqlMirror{Mode: "replace", Scope: &columns, ScopeValues: &values}
	for _, option := range options {
		var shrink float64
		option(&shrink)
		asked.Shrink = &shrink
	}
	return m.run(ctx, asked, rows)
}

// Patch writes what rows adds and changes, a delta of the source's; its Gone
// is always empty.
func (m *Mirror[T]) Patch(ctx context.Context, rows []T) (Diff[T], error) {
	return m.run(ctx, wire.SqlMirror{Mode: "patch"}, rows)
}

func (m *Mirror[T]) run(ctx context.Context, asked wire.SqlMirror, rows []T) (Diff[T], error) {
	listed, err := listOf(rows)
	if err != nil {
		return Diff[T]{}, err
	}
	asked.Table, asked.Key, asked.Rows = m.table, m.options.Key, listed
	if m.options.Gone != "" {
		asked.Gone = &m.options.Gone
	}
	if m.options.Compare != nil {
		asked.Compare = &m.options.Compare
	}
	var diff wire.SqlDiff
	err = m.db.store.Link.Run(ctx, core.Writing, func(c *core.Connection) error {
		handle, err := m.db.Handle(ctx, c)
		if err != nil {
			return err
		}
		asked.Handle = handle
		answer, err := c.Session.Call(ctx, wire.MethodSqlMirror, wire.Encode(&asked))
		if err != nil {
			return err
		}
		return decode(answer, &diff)
	})
	if err != nil {
		return Diff[T]{}, err
	}
	return diffOf[T](diff)
}

// listOf is rows as cells, the first row's columns once: rows of another
// shape are invalid.
func listOf[T any](rows []T) (wire.SqlRows, error) {
	var listed wire.SqlRows
	for i, row := range rows {
		names, fields, err := rowFields(row)
		if err != nil {
			return listed, err
		}
		if i == 0 {
			listed.Columns = names
		} else if fmt.Sprint(names) != fmt.Sprint(listed.Columns) {
			return listed, core.Errorf(core.Invalid, "row %d: not the first row's columns: give rows of one shape", i+1)
		}
		cells := make([]wire.Cell, len(names))
		for at, name := range names {
			if cells[at], err = fieldOf(fields[name], name); err != nil {
				return listed, err
			}
		}
		listed.Rows = append(listed.Rows, cells)
	}
	return listed, nil
}

// diffOf is a mirror's answer, each row read into T.
func diffOf[T any](diff wire.SqlDiff) (Diff[T], error) {
	read := func(rows [][]wire.Cell) ([]T, error) {
		out := make([]T, len(rows))
		for i, row := range rows {
			var err error
			if out[i], err = readRow[T](diff.Columns, row); err != nil {
				return nil, err
			}
		}
		return out, nil
	}
	out := Diff[T]{Same: int(diff.Same), Suspect: diff.Suspect}
	var err error
	if out.Added, err = read(diff.Added); err != nil {
		return out, err
	}
	if out.Gone, err = read(diff.Gone); err != nil {
		return out, err
	}
	before, err := read(diff.Before)
	if err != nil {
		return out, err
	}
	after, err := read(diff.After)
	if err != nil {
		return out, err
	}
	for i := range before {
		out.Changed = append(out.Changed, Changed[T]{Before: before[i], After: after[i]})
	}
	return out, nil
}
