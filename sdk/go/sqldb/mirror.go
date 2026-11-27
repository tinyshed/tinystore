package sqldb

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"sync"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
	"tinyshed.org/tinystore/limits"
)

// MirrorOptions is how a mirror opens: what names a row at the source, what
// marks it gone, and what makes it changed.
type MirrorOptions struct {
	// Key is the columns that name a row at the source: a unique index or the
	// table's primary key covers them.
	Key []string
	// Gone is a column that marks what the source dropped with the store's
	// time instead of deleting it; empty deletes it.
	Gone string
	// GoneUnit is the unit Gone marks the store's time in: Millis when
	// empty, or Seconds.
	GoneUnit Unit
	// ChangeOn is the columns whose change makes a row changed; every column
	// the rows carry but the key's when nil.
	ChangeOn []string
}

// Mirror is a table that mirrors another system: its Replace takes the
// source's list of a scope and its Upsert a change of some rows, and each
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

// MinKept writes nothing, and answers Suspect, when the list would leave
// fewer than fraction of the scope's live rows live.
func MinKept(fraction float64) ReplaceOption { return func(minKept *float64) { *minKept = fraction } }

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
// outside the scope is invalid, and so is one whose key holds a null. Values
// compare as the table keeps them: 4 given to a text column is "4", and a
// key's text compares by its unique index's collation. A list, or an answer,
// past one message goes in the parts of a run, as Begin makes one, and
// writes in one commit all the same.
func (m *Mirror[T]) Replace(ctx context.Context, scope map[string]any, rows []T, options ...ReplaceOption) (Diff[T], error) {
	columns, values, err := scopeOf(scope)
	if err != nil {
		return Diff[T]{}, err
	}
	asked := wire.SqlMirror{Mode: "replace", Scope: &columns, ScopeValues: &values, MinKept: minKeptOf(options)}
	diff, err := m.run(ctx, asked, rows)
	if !pastOneMessage(err) {
		return diff, err
	}
	// refused before it was written: the same list, in parts over one run
	run := m.Begin(scope, options...)
	defer func() { _ = run.Close(ctx) }()
	if err := run.Add(ctx, rows); err != nil {
		return Diff[T]{}, err
	}
	return run.Commit(ctx)
}

// Begin begins a Replace of the scope whose list comes in parts: each Add
// gives a part, and Commit makes the scope what every part listed, in one
// write, so that only what no part named is gone. Nothing is sent until the
// first Add or Commit. A run never committed writes nothing: closed, idle
// for five minutes, its connection lost or the store closed; defer
// run.Close(ctx) lets go of one not committed.
//
//	run := works.Begin(map[string]any{"course_id": 1})
//	defer run.Close(ctx)
//	for page := range pages {
//		if err := run.Add(ctx, page); err != nil { … }
//	}
//	diff, err := run.Commit(ctx)
func (m *Mirror[T]) Begin(scope map[string]any, options ...ReplaceOption) *Run[T] {
	r := &Run[T]{db: m.db}
	columns, values, err := scopeOf(scope)
	if err != nil {
		r.refused = err
		return r
	}
	s, err := m.shape()
	if err != nil {
		r.refused = err
		return r
	}
	r.begin = wire.SqlBegin{
		Table: s.table, Key: s.key, Gone: s.gone, GoneUnit: s.goneUnit, ChangeOn: s.changeOn,
		Scope: &columns, ScopeValues: &values, MinKept: minKeptOf(options),
	}
	return r
}

// scopeOf is a scope's columns, sorted, and their values in their order.
func scopeOf(scope map[string]any) ([]string, []wire.Cell, error) {
	columns := make([]string, 0, len(scope))
	for column := range scope {
		columns = append(columns, column)
	}
	sort.Strings(columns)
	values := make([]wire.Cell, len(columns))
	for i, column := range columns {
		cell, err := cellOf(scope[column], "the scope's "+column)
		if err != nil {
			return nil, nil, err
		}
		values[i] = cell
	}
	return columns, values, nil
}

func minKeptOf(options []ReplaceOption) *float64 {
	var kept *float64
	for _, option := range options {
		var minKept float64
		option(&minKept)
		kept = &minKept
	}
	return kept
}

// pastOneMessage says whether a call was refused for a request or an answer
// past one message, which wrote nothing.
func pastOneMessage(err error) bool {
	return limitMet(err, limits.RequestBytes.Name) || limitMet(err, limits.AnswerBytes.Name)
}

func limitMet(err error, name string) bool {
	var e *core.Error
	return errors.As(err, &e) && e.Kind == core.Limit && e.Limit == name
}

// Run is a mirror's Replace whose list comes in parts, from Begin. Its parts
// wait on the server, outside the table, until Commit. A run is its
// connection's: every call of it goes on the one it began on, never again on
// another, since another holds none of its parts.
type Run[T any] struct {
	db *DB
	// refused is why the run cannot begin: its scope, or its mirror's options
	refused error
	begin   wire.SqlBegin
	mu      sync.Mutex
	conn    *core.Connection
	handle  uint64
	ended   bool
}

// Add gives a part of the list: rows of the first part's shape, inside the
// scope, each key once in the whole run. A part past one message goes as
// several. A part the server refuses ends the run.
func (r *Run[T]) Add(ctx context.Context, rows []T) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if err := r.started(ctx); err != nil {
		return err
	}
	return r.send(ctx, rows)
}

// Commit makes the scope what every part listed, in one write, and answers
// what changed.
func (r *Run[T]) Commit(ctx context.Context) (Diff[T], error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if err := r.started(ctx); err != nil {
		return Diff[T]{}, err
	}
	r.ended = true
	_, items, trailer, err := core.Download(ctx, r.conn, wire.MethodSqlCommit, wire.Encode(&wire.SqlRun{Run: r.handle}))
	if err != nil {
		return Diff[T]{}, err
	}
	diff, err := joinedDiff(append(items, trailer))
	if err != nil {
		return Diff[T]{}, err
	}
	return diffOf[T](diff)
}

// Close lets go of a run not committed, which writes nothing; after Commit,
// or before anything was sent, it sends nothing.
func (r *Run[T]) Close(ctx context.Context) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	begun := r.conn != nil && !r.ended
	r.ended = true
	if !begun || r.conn.Session.Ended() != nil {
		return nil
	}
	_, err := r.conn.Session.Call(ctx, wire.MethodSqlDrop, wire.Encode(&wire.SqlRun{Run: r.handle}))
	// a connection lost let go of the run already
	if lost, _ := core.IsConnectionLost(err); lost {
		return nil
	}
	return err
}

// started begins the run on a connection of its own the first time; a run
// committed or closed is refused.
func (r *Run[T]) started(ctx context.Context) error {
	switch {
	case r.ended:
		return core.Errorf(core.Invalid, "%s: the run is committed or closed: begin another", r.db.describe)
	case r.refused != nil:
		return r.refused
	case r.conn != nil:
		return nil
	}
	c, err := r.db.store.Link.Connection(ctx)
	if err != nil {
		return err
	}
	handle, err := r.db.Handle(ctx, c)
	if err != nil {
		return err
	}
	r.begin.Handle = handle
	answer, err := c.Session.Call(ctx, wire.MethodSqlBegin, wire.Encode(&r.begin))
	if err != nil {
		return err
	}
	var begun wire.Handle
	if err := decode(answer, &begun); err != nil {
		return err
	}
	r.conn, r.handle = c, begun.Handle
	return nil
}

// send sends a part, halved until each half fits one message: the session
// refuses one past it before it leaves, so the run has not seen it.
func (r *Run[T]) send(ctx context.Context, rows []T) error {
	listed, err := listOf(rows)
	if err != nil {
		return err
	}
	_, err = r.conn.Session.Call(ctx, wire.MethodSqlAdd, wire.Encode(&wire.SqlAdd{Run: r.handle, Rows: listed}))
	if len(rows) < 2 || !limitMet(err, limits.RequestBytes.Name) {
		return err
	}
	half := (len(rows) + 1) / 2
	if err := r.send(ctx, rows[:half]); err != nil {
		return err
	}
	return r.send(ctx, rows[half:])
}

// joinedDiff is a difference from its parts, each list joined in their order.
func joinedDiff(parts [][]byte) (wire.SqlDiff, error) {
	var out wire.SqlDiff
	for _, body := range parts {
		var part wire.SqlDiff
		if err := decode(body, &part); err != nil {
			return out, err
		}
		if len(out.Columns) == 0 {
			out.Columns = part.Columns
		}
		out.Added = append(out.Added, part.Added...)
		out.Before = append(out.Before, part.Before...)
		out.After = append(out.After, part.After...)
		out.Gone = append(out.Gone, part.Gone...)
		out.Same += part.Same
		out.Suspect = out.Suspect || part.Suspect
	}
	return out, nil
}

// Upsert writes what rows adds and changes, a delta of the source's; its
// Gone is always empty.
func (m *Mirror[T]) Upsert(ctx context.Context, rows []T) (Diff[T], error) {
	return m.run(ctx, wire.SqlMirror{Mode: "upsert"}, rows)
}

func (m *Mirror[T]) run(ctx context.Context, asked wire.SqlMirror, rows []T) (Diff[T], error) {
	listed, err := listOf(rows)
	if err != nil {
		return Diff[T]{}, err
	}
	s, err := m.shape()
	if err != nil {
		return Diff[T]{}, err
	}
	asked.Table, asked.Key, asked.Gone, asked.GoneUnit, asked.ChangeOn = s.table, s.key, s.gone, s.goneUnit, s.changeOn
	asked.Rows = listed
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

// shape is the mirror as every call names it: its table, key, mark and
// compared columns.
type shape struct {
	table          string
	key            []string
	gone, goneUnit *string
	changeOn       *[]string
}

func (m *Mirror[T]) shape() (shape, error) {
	unit, err := unitOf([]Unit{m.options.GoneUnit}, "a mirror's gone option")
	if err != nil {
		return shape{}, core.Errorf(core.Invalid, "%s: mirror %s: %s", m.db.describe, m.table, err)
	}
	s := shape{table: m.table, key: m.options.Key}
	if m.options.Gone != "" {
		gone := m.options.Gone
		s.gone = &gone
	}
	// milliseconds go unnamed, which a server older than the unit reads too
	if unit == Seconds {
		seconds := string(Seconds)
		s.goneUnit = &seconds
	}
	if m.options.ChangeOn != nil {
		s.changeOn = &m.options.ChangeOn
	}
	return s, nil
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
