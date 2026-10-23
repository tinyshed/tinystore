package sqldb

import (
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"iter"
	"math"
	"reflect"
	"regexp"
	"sort"
	"strconv"
	"strings"

	"tinyshed.org/tinystore/internal/core"
)

// A table's queries, built from pieces into the SQL a person would write: the
// builder adds the ands, the parentheses, the placeholders and the quotes
// around names, and nothing else. It runs in this process; the store sees
// the text and the values, as it would a statement written by hand.

// Direction is an order's direction.
type Direction string

// The directions an order takes.
const (
	Asc  Direction = "asc"
	Desc Direction = "desc"
)

// Order is an order a request chooses by name: sqldb.Order{"created_at", sqldb.Desc}.
type Order struct {
	Column    string
	Direction Direction
}

// Set is the columns an update sets: values, or pieces of SQL.
type Set map[string]any

// Page is a page of a query's rows, and the cursor that reads the next: "" after the last.
type Page[T any] struct {
	Rows []T
	Next string
}

type kept struct {
	piece Piece
	bare  bool
}

type ordered struct {
	piece     Piece
	direction Direction
	column    string
}

type included struct {
	name    string
	parts   parts
	columns []string
}

type parts struct {
	table, alias string
	selected     *Piece
	joins        []Piece
	conditions   []kept
	groups       []string
	having       []kept
	order        []ordered
	limit        *int64
	offset       *int64
	includes     []included
	err          error
}

func (p parts) with(change func(*parts)) parts {
	next := p
	next.joins = append([]Piece{}, p.joins...)
	next.conditions = append([]kept{}, p.conditions...)
	next.groups = append([]string{}, p.groups...)
	next.having = append([]kept{}, p.having...)
	next.order = append([]ordered{}, p.order...)
	next.includes = append([]included{}, p.includes...)
	change(&next)
	return next
}

func (p *parts) fail(err error) {
	if p.err == nil && err != nil {
		p.err = err
	}
}

// Query is a query on a table: a value, each call giving a new one, so that
// the first stays as it was. All, One, Count, Each and List read; Update and
// Delete write.
type Query[T any] struct {
	runner Runner
	parts  parts
}

// queried is any query, whatever its rows' type: what Select and Include take.
type queried interface {
	queryParts() parts
	queryRunner() Runner
}

func (q *Query[T]) queryParts() parts   { return q.parts }
func (q *Query[T]) queryRunner() Runner { return q.runner }
func (q *Query[T]) with(change func(*parts)) *Query[T] {
	return &Query[T]{q.runner, q.parts.with(change)}
}

// Table is a table: its queries, and its rows' inserts and upserts. Its rows
// are T, a struct whose fields are the table's columns.
type Table[T any] struct {
	*Query[T]
	name string
}

var tableRef = regexp.MustCompile(`(?i)^\s*([A-Za-z_][\w$]*(?:\.[A-Za-z_][\w$]*)?)(?:\s+(?:as\s+)?([A-Za-z_][\w$]*))?\s*$`)

// OpenTable is a table of a database, or of a transaction of it, "orders" or
// "orders as o", typed where it opens. It costs nothing until its first call.
func OpenTable[T any](r Runner, name string) *Table[T] {
	matched := tableRef.FindStringSubmatch(name)
	p := parts{}
	if matched == nil {
		p.err = core.Errorf(core.Invalid, "the table %q: a table is its name, and an alias after as", name)
	} else {
		p.table, p.alias = matched[1], matched[2]
	}
	return &Table[T]{&Query[T]{r, p}, p.table}
}

// Name is the table's name.
func (t *Table[T]) Name() string { return t.name }

// Where adds conditions with and: equal values, sqldb.Eq{"status": "paid"},
// nil as is null and a slice as in (...); or a piece of SQL from sqldb.SQL.
func (q *Query[T]) Where(conditions ...Condition) *Query[T] {
	return q.with(func(p *parts) {
		for _, c := range conditions {
			p.conditions = append(p.conditions, keptOf(c)...)
		}
	})
}

func keptOf(c Condition) []kept {
	switch v := c.(type) {
	case Eq:
		names := make([]string, 0, len(v))
		for name := range v {
			names = append(names, name)
		}
		sort.Strings(names)
		out := make([]kept, len(names))
		for i, name := range names {
			out[i] = kept{equal(name, v[name]), true}
		}
		return out
	case Piece:
		return []kept{{v, v.bare || grouped(v.text)}}
	}
	piece := c.condition()
	return []kept{{piece, piece.bare || grouped(piece.text)}}
}

// Select is a query's columns, as SQL text, and the type of the row they make.
func Select[R any](q queried, columns string) *Query[R] {
	p := q.queryParts().with(func(p *parts) { p.selected = &Piece{text: columns} })
	return &Query[R]{q.queryRunner(), p}
}

// Include gives each row the rows of another query as a list under name, from
// the same statement: a field of that name holds them. The query names the
// row it belongs to in its where, by this table's alias, names its columns
// with Select, and has a limit, which counts for each row. Every value comes
// back as it is kept.
func (q *Query[T]) Include(name string, query queried) *Query[T] {
	describe := fmt.Sprintf("the included %q", name)
	child := query.queryParts()
	return q.with(func(p *parts) {
		switch {
		case child.selected == nil:
			p.fail(core.Errorf(core.Invalid, "%s: it names its columns with Select", describe))
		case child.limit == nil:
			p.fail(core.Errorf(core.Invalid, "%s: it needs a limit, which counts for each row", describe))
		case len(child.includes) > 0:
			p.fail(core.Errorf(core.Invalid, "%s: an included query includes nothing itself", describe))
		}
		for _, each := range p.includes {
			if each.name == name {
				p.fail(core.Errorf(core.Invalid, "%s: an included list has a name of its own", describe))
			}
		}
		if child.selected == nil {
			return
		}
		columns, err := namesOf(child.selected.text, describe)
		p.fail(err)
		p.fail(child.err)
		p.includes = append(p.includes, included{name, child, columns})
	})
}

// Join joins a table, "users as u", on a condition as SQL text or a piece of SQL.
func (q *Query[T]) Join(table string, on any) *Query[T] { return q.joined("join", table, on) }

// LeftJoin joins a table whose columns may be NULL where no row of it meets the condition.
func (q *Query[T]) LeftJoin(table string, on any) *Query[T] { return q.joined("left join", table, on) }

func (q *Query[T]) joined(kind, table string, on any) *Query[T] {
	var condition Piece
	switch v := on.(type) {
	case string:
		condition = Piece{text: v}
	case Piece:
		condition = v
	default:
		condition = Piece{err: core.Errorf(core.Invalid, "a join's condition is SQL text or a piece of SQL, not %T", on)}
	}
	return q.with(func(p *parts) {
		ref, err := quotedTable(table)
		p.fail(err)
		p.fail(condition.err)
		p.joins = append(p.joins, Piece{text: kind + " " + ref + " on " + condition.text, values: condition.values})
	})
}

func quotedTable(text string) (string, error) {
	matched := tableRef.FindStringSubmatch(text)
	if matched == nil {
		return "", core.Errorf(core.Invalid, "the table %q: a table is its name, and an alias after as", text)
	}
	if matched[2] == "" {
		return mustIdent(matched[1]), nil
	}
	return mustIdent(matched[1]) + " as " + mustIdent(matched[2]), nil
}

// GroupBy groups the rows by columns, quoted as names.
func (q *Query[T]) GroupBy(columns ...string) *Query[T] {
	return q.with(func(p *parts) {
		for _, column := range columns {
			quoted, err := identOf(column)
			p.fail(err)
			p.groups = append(p.groups, quoted)
		}
	})
}

// Having is a condition on the groups, as Where takes one.
func (q *Query[T]) Having(conditions ...Condition) *Query[T] {
	return q.with(func(p *parts) {
		for _, c := range conditions {
			p.having = append(p.having, keptOf(c)...)
		}
	})
}

// OrderBy orders the rows by a column, quoted as a name, or by a piece of
// SQL: OrderBy(sqldb.SQL("lower(title)"), sqldb.Asc).
func (q *Query[T]) OrderBy(column any, direction Direction) *Query[T] {
	return q.with(func(p *parts) {
		if direction != Asc && direction != Desc {
			p.fail(core.Errorf(core.Invalid, "an order is asc or desc, not %q", direction))
		}
		switch v := column.(type) {
		case string:
			quoted, err := identOf(v)
			p.fail(err)
			p.order = append(p.order, ordered{Piece{text: quoted}, direction, v})
		case Piece:
			p.fail(v.err)
			p.order = append(p.order, ordered{v, direction, ""})
		default:
			p.fail(core.Errorf(core.Invalid, "an order is by a column's name or a piece of SQL, not %T", column))
		}
	})
}

// Limit is how many rows the query reads at most.
func (q *Query[T]) Limit(rows int) *Query[T] {
	return q.with(func(p *parts) {
		if rows < 0 {
			p.fail(core.Errorf(core.Invalid, "a limit of %d: a whole number", rows))
		}
		n := int64(rows)
		p.limit = &n
	})
}

// Offset is how many rows the query skips.
func (q *Query[T]) Offset(rows int) *Query[T] {
	return q.with(func(p *parts) {
		if rows < 0 {
			p.fail(core.Errorf(core.Invalid, "an offset of %d: a whole number", rows))
		}
		n := int64(rows)
		p.offset = &n
	})
}

// SQL is the text and the values the query sends, to read or to log.
func (q *Query[T]) SQL() (string, []any, error) {
	rendered := render(q.parts)
	return rendered.text, rendered.values, rendered.err
}

// embedded is the query inside another piece of SQL: a subquery, in parentheses.
func (q *Query[T]) embedded() Piece {
	rendered := render(q.parts)
	return Piece{text: "(" + rendered.text + ")", values: rendered.values, err: rendered.err}
}

// All is every row the query reads.
func (q *Query[T]) All(ctx context.Context) ([]T, error) {
	got, err := q.runner.rows(ctx, render(q.parts), "all")
	if err != nil {
		return nil, err
	}
	return q.read(got)
}

// One is the one row the query reads, and whether it read one; two are Invalid.
func (q *Query[T]) One(ctx context.Context) (T, bool, error) {
	var zero T
	got, err := q.runner.rows(ctx, render(q.parts), "one")
	if err != nil {
		return zero, false, err
	}
	rows, err := q.read(got)
	if err != nil || len(rows) == 0 {
		return zero, false, err
	}
	return rows[0], true, nil
}

// Count counts every row the conditions match, without the order and the limit.
func (q *Query[T]) Count(ctx context.Context) (int64, error) {
	counted := q.parts.with(func(p *parts) {
		p.selected = &Piece{text: "count(*)"}
		p.order, p.limit, p.offset, p.includes = nil, nil, nil, nil
	})
	statement := render(counted)
	if len(counted.groups) > 0 {
		inner := render(counted.with(func(p *parts) { p.selected = &Piece{text: "1"} }))
		statement = Piece{text: "select count(*) from (" + inner.text + ")", values: inner.values, err: inner.err}
	}
	got, err := q.runner.rows(ctx, statement, "scalar")
	if err != nil || len(got.rows) == 0 || len(got.rows[0]) == 0 {
		return 0, err
	}
	n, _ := got.rows[0][0].(int64)
	return n, nil
}

// Each is the rows one at a time, as their parts arrive.
func (q *Query[T]) Each(ctx context.Context) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		var zero T
		var columns []string
		for values, err := range q.runner.each(ctx, render(q.parts)) {
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
			rows, err := q.read(answered{columns, [][]any{values}})
			var row T
			if len(rows) > 0 {
				row = rows[0]
			}
			if !yield(row, err) || err != nil {
				return
			}
		}
	}
}

// ListOption is how a page of rows reads.
type ListOption interface{ list(*listOptions) }

type listOptions struct {
	limit int64
	after string
}

type limitOption int

// Limit is how many rows a page holds: 100 unless given.
func Limit(n int) ListOption { return limitOption(n) }

func (l limitOption) list(o *listOptions) { o.limit = int64(l) }

type afterOption string

// After reads the page after the one whose Next it is.
func After(next string) ListOption { return afterOption(next) }

func (a afterOption) list(o *listOptions) { o.after = string(a) }

// List is a page after the last row of the one before, by the query's order,
// which ends in a unique column so that no row is read twice or skipped;
// Next reads the next page, and only of this query.
func (q *Query[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error) {
	o := listOptions{limit: 100}
	for _, opt := range opts {
		opt.list(&o)
	}
	order := q.parts.order
	for _, each := range order {
		if each.column == "" {
			return Page[T]{}, core.Errorf(core.Invalid, "a page is read by the query's order, of columns by their names: OrderBy first")
		}
	}
	if len(order) == 0 {
		return Page[T]{}, core.Errorf(core.Invalid, "a page is read by the query's order, of columns by their names: OrderBy first")
	}
	if o.limit < 0 {
		return Page[T]{}, core.Errorf(core.Invalid, "a page limit of %d: a whole number", o.limit)
	}
	digest := digestOf(render(q.parts.with(func(p *parts) { p.limit, p.offset = nil, nil })))
	limit := o.limit
	paged := q.parts.with(func(p *parts) { p.limit, p.offset = &limit, nil })
	if o.after != "" {
		after, err := cursorOf(o.after, digest)
		if err != nil {
			return Page[T]{}, err
		}
		keyset := keysetOf(order, after)
		paged = paged.with(func(p *parts) { p.conditions = append(p.conditions, kept{keyset, true}) })
	}
	got, err := q.runner.rows(ctx, render(paged), "all")
	if err != nil {
		return Page[T]{}, err
	}
	rows, err := q.read(got)
	if err != nil {
		return Page[T]{}, err
	}
	if int64(len(got.rows)) < limit {
		return Page[T]{Rows: rows}, nil
	}
	last := got.rows[len(got.rows)-1]
	values := make([]any, len(order))
	for i, each := range order {
		name := each.column[strings.LastIndex(each.column, ".")+1:]
		at := -1
		for j, column := range got.columns {
			if column == name {
				at = j
			}
		}
		if at < 0 {
			return Page[T]{}, core.Errorf(core.Invalid, "the page is ordered by %s, which its rows lack: select it", each.column)
		}
		values[i] = last[at]
	}
	return Page[T]{rows, cursorText(values, digest)}, nil
}

// Update sets columns of the rows the conditions match, and says how many changed.
func (q *Query[T]) Update(ctx context.Context, set Set) (Result, error) {
	if err := q.refuseEveryRow("an update"); err != nil {
		return Result{}, err
	}
	if len(set) == 0 {
		return Result{}, core.Errorf(core.Invalid, "an update sets at least one column")
	}
	names := make([]string, 0, len(set))
	for name := range set {
		names = append(names, name)
	}
	sort.Strings(names)
	var texts []string
	var values []any
	for _, name := range names {
		column, err := identOf(name)
		if err != nil {
			return Result{}, err
		}
		if piece, ok := set[name].(Piece); ok {
			if piece.err != nil {
				return Result{}, piece.err
			}
			texts = append(texts, column+" = "+piece.text)
			values = append(values, piece.values...)
			continue
		}
		cell, err := fieldOf(set[name], name)
		if err != nil {
			return Result{}, err
		}
		texts = append(texts, column+" = ?")
		values = append(values, cell)
	}
	where := conditionsText(q.parts.conditions)
	text := "update " + mustIdent(q.parts.table) + " set " + strings.Join(texts, ", ") + " where " + where.text
	return q.runner.exec(ctx, Piece{text: text, values: append(values, where.values...), err: where.err})
}

// Delete deletes the rows the conditions match, and says how many it deleted.
func (q *Query[T]) Delete(ctx context.Context) (Result, error) {
	if err := q.refuseEveryRow("a delete"); err != nil {
		return Result{}, err
	}
	where := conditionsText(q.parts.conditions)
	return q.runner.exec(ctx, Piece{text: "delete from " + mustIdent(q.parts.table) + " where " + where.text, values: where.values, err: where.err})
}

// refuseEveryRow refuses an update or a delete of every row, which SQL says plainly.
func (q *Query[T]) refuseEveryRow(what string) error {
	if q.parts.err != nil {
		return q.parts.err
	}
	some := false
	for _, each := range q.parts.conditions {
		some = some || each.piece.text != "1"
	}
	if !some {
		return core.Errorf(core.Invalid, "%s without a condition: every row is said in SQL, delete from ...", what)
	}
	if len(q.parts.joins) > 0 {
		return core.Errorf(core.Invalid, "%s of a join: write it in SQL", what)
	}
	return nil
}

// read reads the rows a query answered, each included query's rows read
// into the field of its name.
func (q *Query[T]) read(got answered) ([]T, error) {
	if err := checkColumns(got.columns); err != nil {
		return nil, err
	}
	includes := map[string]included{}
	for _, each := range q.parts.includes {
		includes[each.name] = each
	}
	out := make([]T, 0, len(got.rows))
	for _, values := range got.rows {
		if len(includes) > 0 {
			values = append([]any{}, values...)
			for i, column := range got.columns {
				if each, ok := includes[column]; ok {
					rows, err := includedRows(each, values[i])
					if err != nil {
						return nil, err
					}
					values[i] = rows
				}
			}
		}
		row, err := readRow[T](got.columns, values)
		if err != nil {
			return nil, err
		}
		out = append(out, row)
	}
	return out, nil
}

// includedValue is the rows an included query gave a row, which a slice field reads.
type includedValue struct {
	columns []string
	rows    [][]any
}

func init() {
	readIncluded = func(into reflect.Value, value any, column string) (bool, error) {
		listed, ok := value.(includedValue)
		if !ok {
			return false, nil
		}
		t := into.Type()
		switch {
		case t.Kind() == reflect.Slice:
			slice := reflect.MakeSlice(t, len(listed.rows), len(listed.rows))
			for i, row := range listed.rows {
				if err := readInto(slice.Index(i), listed.columns, row); err != nil {
					return true, err
				}
			}
			into.Set(slice)
		case t.Kind() == reflect.Interface:
			rows := make([]Row, len(listed.rows))
			for i, row := range listed.rows {
				m := Row{}
				for j, name := range listed.columns {
					m[name] = row[j]
				}
				rows[i] = m
			}
			into.Set(reflect.ValueOf(rows))
		default:
			return true, core.Errorf(core.Invalid, "the included %s reads into a slice, not %s", column, t)
		}
		return true, nil
	}
}

func includedRows(each included, value any) (includedValue, error) {
	if value == nil {
		return includedValue{each.columns, nil}, nil
	}
	text, ok := value.(string)
	if !ok {
		return includedValue{}, core.Errorf(core.Corrupt, "the included %q came back as no text", each.name)
	}
	rows, err := literalRows(text)
	return includedValue{each.columns, rows}, err
}

var (
	quotedName = `("(?:[^"]|"")+"|[A-Za-z_][\w$]*)`
	aliased    = regexp.MustCompile(`(?i)\s+as\s+` + quotedName + `\s*$`)
	columnName = regexp.MustCompile(`^(?:` + quotedName + `\.)?` + quotedName + `$`)
)

// namesOf is the names of the columns a select's text gives, in their order:
// a column's own, or what follows its as.
func namesOf(selected, describe string) ([]string, error) {
	bare := func(quoted string) string {
		if strings.HasPrefix(quoted, `"`) {
			return strings.ReplaceAll(quoted[1:len(quoted)-1], `""`, `"`)
		}
		return quoted
	}
	var names []string
	for _, item := range itemsOf(selected) {
		found := ""
		if m := aliased.FindStringSubmatch(item); m != nil {
			found = m[1]
		} else if m := columnName.FindStringSubmatch(item); m != nil {
			found = m[2]
		}
		if found == "" {
			return nil, core.Errorf(core.Invalid, "%s: the column %q needs a name: write it as ... as name", describe, item)
		}
		for _, n := range names {
			if n == bare(found) {
				return nil, core.Errorf(core.Invalid, "%s: two columns named %s", describe, n)
			}
		}
		names = append(names, bare(found))
	}
	return names, nil
}

// itemsOf is a select's items: its text cut at the commas outside quotes and parentheses.
func itemsOf(selected string) []string {
	var items []string
	depth, start := 0, 0
	for at := 0; at < len(selected); at++ {
		switch c := selected[at]; {
		case c == '\'' || c == '"' || c == '`':
			end := strings.IndexByte(selected[at+1:], c)
			if end < 0 {
				at = len(selected)
			} else {
				at += end + 1
			}
		case c == '(':
			depth++
		case c == ')':
			depth--
		case c == ',' && depth == 0:
			items = append(items, strings.TrimSpace(selected[start:at]))
			start = at + 1
		}
	}
	return append(items, strings.TrimSpace(selected[start:]))
}

// includedText is an included query as one value of its row: its rows as SQL
// literals, (1,'a'),(2,NULL), in its order. SQLite's quote() writes an
// integer, a float, a text and bytes so that each reads back as it is kept,
// where JSON would lose bytes and a float's last digits.
func includedText(each included) Piece {
	child := each.parts
	type alias struct {
		order ordered
		name  string
	}
	aliases := make([]alias, len(child.order))
	for i, o := range child.order {
		aliases[i] = alias{o, fmt.Sprintf(`"$o%d"`, i+1)}
	}
	texts := []string{child.selected.text}
	values := append([]any{}, child.selected.values...)
	for _, a := range aliases {
		texts = append(texts, a.order.piece.text+" as "+a.name)
		values = append(values, a.order.piece.values...)
	}
	inner := render(child.with(func(p *parts) { p.selected = &Piece{text: strings.Join(texts, ", "), values: values} }))
	literals := make([]string, len(each.columns))
	for i, column := range each.columns {
		literals[i] = literalOf(column)
	}
	order := ""
	if len(aliases) > 0 {
		by := make([]string, len(aliases))
		for i, a := range aliases {
			by[i] = a.name + " " + string(a.order.direction)
		}
		order = " order by " + strings.Join(by, ", ")
	}
	row := strings.Join(literals, " || ',' || ")
	return Piece{
		text:   "(select group_concat('(' || " + row + " || ')', ','" + order + ") from (" + inner.text + "))",
		values: inner.values,
		err:    inner.err,
	}
}

// literalOf is a column as an SQL literal that keeps it whole: quote() cuts a
// text at a zero byte, so it goes as its bytes.
func literalOf(column string) string {
	c := `"` + strings.ReplaceAll(column, `"`, `""`) + `"`
	return "case when typeof(" + c + ") = 'text' and instr(cast(" + c + " as blob), x'00') > 0 " +
		"then 'T' || quote(cast(" + c + " as blob)) else quote(" + c + ") end"
}

// literalRows is the rows an included query gave, read from their literals:
// (1,'it”s',NULL),(2,X'00ff',1.5).
func literalRows(text string) ([][]any, error) {
	r := &literals{text: text}
	var rows [][]any
	for !r.done() {
		if err := r.expect('('); err != nil {
			return nil, err
		}
		var row []any
		for {
			value, err := r.literal()
			if err != nil {
				return nil, err
			}
			row = append(row, value)
			if r.next() != ',' {
				break
			}
			r.at++
		}
		if err := r.expect(')'); err != nil {
			return nil, err
		}
		rows = append(rows, row)
		if !r.done() {
			if err := r.expect(','); err != nil {
				return nil, err
			}
		}
	}
	return rows, nil
}

type literals struct {
	text string
	at   int
}

func (r *literals) done() bool { return r.at >= len(r.text) }

func (r *literals) next() byte {
	if r.done() {
		return 0
	}
	return r.text[r.at]
}

func (r *literals) expect(c byte) error {
	if r.next() != c {
		return r.broken()
	}
	r.at++
	return nil
}

var integer = regexp.MustCompile(`^-?\d+$`)

func (r *literals) literal() (any, error) {
	switch r.next() {
	case '\'':
		return r.quoted()
	case 'X':
		return r.hex()
	case 'T':
		r.at++
		bytes, err := r.hex()
		return string(bytes), err
	}
	if strings.HasPrefix(r.text[r.at:], "NULL") {
		r.at += 4
		return nil, nil
	}
	end := r.at
	for end < len(r.text) && r.text[end] != ',' && r.text[end] != ')' {
		end++
	}
	word := r.text[r.at:end]
	r.at = end
	if integer.MatchString(word) {
		n, err := strconv.ParseInt(word, 10, 64)
		if err != nil {
			return nil, r.broken()
		}
		return n, nil
	}
	f, err := strconv.ParseFloat(word, 64)
	if err != nil {
		return nil, r.broken()
	}
	return f, nil
}

// quoted is a text in single quotes, a quote inside it doubled.
func (r *literals) quoted() (string, error) {
	var value strings.Builder
	r.at++
	for {
		end := strings.IndexByte(r.text[r.at:], '\'')
		if end < 0 {
			return "", r.broken()
		}
		value.WriteString(r.text[r.at : r.at+end])
		r.at += end + 1
		if r.next() != '\'' {
			return value.String(), nil
		}
		value.WriteByte('\'')
		r.at++
	}
}

// hex is bytes as X'00ff'.
func (r *literals) hex() ([]byte, error) {
	if !strings.HasPrefix(r.text[r.at:], "X'") {
		return nil, r.broken()
	}
	end := strings.IndexByte(r.text[r.at+2:], '\'')
	if end < 0 {
		return nil, r.broken()
	}
	bytes, err := hex.DecodeString(r.text[r.at+2 : r.at+2+end])
	if err != nil {
		return nil, r.broken()
	}
	r.at += end + 3
	return bytes, nil
}

func (r *literals) broken() error {
	stop := min(r.at+20, len(r.text))
	return core.Errorf(core.Corrupt, "included rows that do not read at %d: %s", r.at, r.text[r.at:stop])
}

func conditionsText(conditions []kept) Piece {
	pieces := make([]Piece, len(conditions))
	for i, each := range conditions {
		pieces[i] = each.piece
		if each.bare {
			pieces[i].bare = true
		}
	}
	return joinedWith(pieces, "and")
}

// render is the query as one statement.
func render(p parts) Piece {
	if p.err != nil {
		return Piece{err: p.err}
	}
	var values []any
	source := mustIdent(p.table)
	if p.alias != "" {
		source += " as " + mustIdent(p.alias)
	}
	columns := "*"
	switch {
	case p.selected != nil:
		columns = p.selected.text
		values = append(values, p.selected.values...)
	case len(p.joins) > 0 || len(p.includes) > 0:
		name := p.alias
		if name == "" {
			name = p.table
		}
		columns = mustIdent(name) + ".*"
	}
	for _, each := range p.includes {
		listed := includedText(each)
		if listed.err != nil {
			return listed
		}
		columns += ", " + listed.text + " as " + mustIdent(each.name)
		values = append(values, listed.values...)
	}
	text := "select " + columns + " from " + source
	for _, join := range p.joins {
		text += " " + join.text
		values = append(values, join.values...)
	}
	if len(p.conditions) > 0 {
		where := conditionsText(p.conditions)
		if where.err != nil {
			return where
		}
		text += " where " + where.text
		values = append(values, where.values...)
	}
	if len(p.groups) > 0 {
		text += " group by " + strings.Join(p.groups, ", ")
	}
	if len(p.having) > 0 {
		having := conditionsText(p.having)
		if having.err != nil {
			return having
		}
		text += " having " + having.text
		values = append(values, having.values...)
	}
	if len(p.order) > 0 {
		by := make([]string, len(p.order))
		for i, each := range p.order {
			by[i] = each.piece.text + " " + string(each.direction)
			values = append(values, each.piece.values...)
		}
		text += " order by " + strings.Join(by, ", ")
	}
	if p.limit != nil {
		text += " limit cast(? as integer)"
		values = append(values, *p.limit)
	}
	if p.offset != nil {
		if p.limit == nil {
			text += " limit -1"
		}
		text += " offset cast(? as integer)"
		values = append(values, *p.offset)
	}
	return Piece{text: text, values: values}
}

// keysetOf is the condition that starts a page after a row's order values.
// SQLite orders a null before every value, first in ascending order and last
// in descending, and a comparison with a null is never true: equal is `is`,
// and past a null is every value ascending and nothing descending.
func keysetOf(order []ordered, after []any) Piece {
	var alternatives []string
	var values []any
	for at, each := range order {
		column := each.piece.text
		var value any
		if at < len(after) {
			value = after[at]
		}
		if value == nil && each.direction == Desc {
			continue
		}
		var equal []string
		for _, earlier := range order[:at] {
			equal = append(equal, earlier.piece.text+" is ?")
		}
		values = append(values, after[:min(at, len(after))]...)
		past := column + " is not null"
		switch {
		case value == nil:
		case each.direction == Desc:
			past = "(" + column + " < ? or " + column + " is null)"
			values = append(values, value)
		default:
			past = column + " > ?"
			values = append(values, value)
		}
		if len(equal) == 0 {
			alternatives = append(alternatives, past)
		} else {
			alternatives = append(alternatives, "("+strings.Join(append(equal, past), " and ")+")")
		}
	}
	if len(alternatives) == 0 {
		return Piece{text: "0"}
	}
	return Piece{text: "(" + strings.Join(alternatives, " or ") + ")", values: values}
}

// jsonable is a value as a cursor carries it: bytes as {"b": hex}.
func jsonable(value any) any {
	if bytes, ok := value.([]byte); ok {
		return map[string]string{"b": hex.EncodeToString(bytes)}
	}
	return value
}

// digestOf is a short digest of a query, which a cursor carries so that
// another query refuses it: FNV-1a of its text and its values' JSON, as
// every SDK writes it.
func digestOf(statement Piece) string {
	items := make([]any, len(statement.values))
	for i, v := range statement.values {
		items[i] = jsonable(v)
	}
	text := statement.text + "\x00" + pythonJSON(items)
	hash := uint32(0x811C9DC5)
	for i := 0; i < len(text); i++ {
		hash = (hash ^ uint32(text[i])) * 0x01000193
	}
	return strconv.FormatUint(uint64(hash), 16)
}

// pythonJSON is JSON as Python's json.dumps writes it by default: ", " and ": "
// between items, which the digest of every SDK reads alike.
func pythonJSON(value any) string {
	text, _ := json.Marshal(value)
	var out strings.Builder
	inString := false
	for i := 0; i < len(text); i++ {
		c := text[i]
		out.WriteByte(c)
		switch {
		case c == '"' && (i == 0 || text[i-1] != '\\'):
			inString = !inString
		case !inString && (c == ',' || c == ':'):
			out.WriteByte(' ')
		}
	}
	return out.String()
}

// cursorText is a page's cursor of its last row's order values: an infinity,
// which JSON has no number for, as {"r": "+Inf"}.
func cursorText(values []any, digest string) string {
	items := make([]any, len(values))
	for i, v := range values {
		if f, ok := v.(float64); ok && math.IsInf(f, 0) {
			items[i] = map[string]string{"r": strconv.FormatFloat(f, 'g', -1, 64)}
			continue
		}
		items[i] = jsonable(v)
	}
	text, _ := json.Marshal(map[string]any{"q": digest, "v": items})
	return base64.RawURLEncoding.EncodeToString(text)
}

func cursorOf(cursor, digest string) ([]any, error) {
	text, err := base64.RawURLEncoding.DecodeString(strings.TrimRight(cursor, "="))
	var read struct {
		Q string `json:"q"`
		V []any  `json:"v"`
	}
	decoder := json.NewDecoder(strings.NewReader(string(text)))
	decoder.UseNumber()
	if err != nil || decoder.Decode(&read) != nil {
		return nil, core.Errorf(core.Invalid, "a cursor that is not one: pass the Next of a page")
	}
	if read.Q != digest || read.V == nil {
		return nil, core.Errorf(core.Invalid, "a cursor of another query: a page's Next reads only the query that made it")
	}
	out := make([]any, len(read.V))
	for i, v := range read.V {
		switch item := v.(type) {
		case map[string]any:
			if r, ok := item["r"]; ok {
				f, err := strconv.ParseFloat(fmt.Sprint(r), 64)
				if err != nil {
					return nil, core.Errorf(core.Invalid, "a cursor that is not one: pass the Next of a page")
				}
				out[i] = f
				continue
			}
			bytes, _ := hex.DecodeString(fmt.Sprint(item["b"]))
			out[i] = bytes
		case json.Number:
			if n, err := item.Int64(); err == nil {
				out[i] = n
			} else {
				f, _ := item.Float64()
				out[i] = f
			}
		default:
			out[i] = item
		}
	}
	return out, nil
}

// Insert inserts a row and gives it back as the file keeps it: the defaults
// and the generated columns the database filled.
func (t *Table[T]) Insert(ctx context.Context, row T) (T, error) {
	var zero T
	rows, err := t.InsertMany(ctx, []T{row})
	if err != nil || len(rows) == 0 {
		return zero, err
	}
	return rows[0], nil
}

// InsertMany inserts rows of one shape as one write, all of them or none,
// and gives them back as the file keeps them.
func (t *Table[T]) InsertMany(ctx context.Context, rows []T) ([]T, error) {
	if len(rows) == 0 {
		return nil, nil
	}
	if t.parts.err != nil {
		return nil, t.parts.err
	}
	given := make([]any, len(rows))
	for i, row := range rows {
		given[i] = row
	}
	statement := insertOf(t.name, given)
	if statement.err != nil {
		return nil, statement.err
	}
	got, err := t.runner.rows(ctx, Piece{text: statement.text + " returning *", values: statement.values}, "all")
	if err != nil {
		return nil, err
	}
	return t.read(got)
}

// UpsertOption is what an upsert says: the columns whose taken values make
// it an update, the columns that update sets, and the condition the row that
// holds them must meet.
type UpsertOption interface{ upsert(*upsertOptions) }

type upsertOptions struct {
	conflict, update []string
	where            Condition
}

type conflictOption []string

// Conflict names the columns whose values, when taken, make an upsert an update.
func Conflict(columns ...string) UpsertOption { return conflictOption(columns) }

func (c conflictOption) upsert(o *upsertOptions) { o.conflict = c }

type updateOption []string

// Update names the columns an upsert's update sets from the row.
func Update(columns ...string) UpsertOption { return updateOption(columns) }

func (u updateOption) upsert(o *upsertOptions) { o.update = u }

type whereOption struct{ c Condition }

// Where is the condition the row that holds an upsert's key must meet, its
// owner's, so that an id from a request changes nothing of another's row.
func Where(c Condition) UpsertOption { return whereOption{c} }

func (w whereOption) upsert(o *upsertOptions) { o.where = w.c }

// Upsert inserts a row, or, when the columns Conflict names are taken, sets
// the columns Update names on the row that holds them, when that row meets
// Where: its Changes is 0 when Where kept it.
func (t *Table[T]) Upsert(ctx context.Context, row T, opts ...UpsertOption) (Result, error) {
	var o upsertOptions
	for _, opt := range opts {
		opt.upsert(&o)
	}
	if t.parts.err != nil {
		return Result{}, t.parts.err
	}
	if len(o.conflict) == 0 || len(o.update) == 0 {
		return Result{}, core.Errorf(core.Invalid, "an upsert names its Conflict columns and the columns its Update sets")
	}
	insert := insertOf(t.name, []any{row})
	if insert.err != nil {
		return Result{}, insert.err
	}
	keys := make([]string, len(o.conflict))
	for i, key := range o.conflict {
		keys[i] = mustIdent(key)
	}
	sets := make([]string, len(o.update))
	for i, name := range o.update {
		sets[i] = mustIdent(name) + " = excluded." + mustIdent(name)
	}
	text := insert.text + " on conflict (" + strings.Join(keys, ", ") + ") do update set " + strings.Join(sets, ", ")
	values := insert.values
	if o.where != nil {
		// in an upsert's update, a column is the row's that holds the key
		condition := o.where.condition()
		if condition.err != nil {
			return Result{}, condition.err
		}
		text += " where " + condition.text
		values = append(values, condition.values...)
	}
	// returning, as every SDK writes it: a row comes back when one was written
	got, err := t.runner.rows(ctx, Piece{text: text + " returning *", values: values}, "one")
	return Result{Changes: int64(len(got.rows))}, err
}

// insertOf is an insert of rows of the same columns, the first row's.
func insertOf(table string, rows []any) Piece {
	names, _, err := rowFields(rows[0])
	if err != nil {
		return Piece{err: err}
	}
	if len(names) == 0 {
		return Piece{err: core.Errorf(core.Invalid, "a row to insert with no column")}
	}
	var values []any
	tuples := make([]string, len(rows))
	marks := strings.TrimSuffix(strings.Repeat("?, ", len(names)), ", ")
	for i, row := range rows {
		rowNames, fields, err := rowFields(row)
		if err != nil {
			return Piece{err: err}
		}
		for _, name := range rowNames {
			if _, ok := indexOf(names, name); !ok {
				return Piece{err: core.Errorf(core.Invalid, "row %d has %s, which the first row has not: insert rows of one shape", i+1, name)}
			}
		}
		for _, name := range names {
			cell, err := fieldOf(fields[name], name)
			if err != nil {
				return Piece{err: err}
			}
			values = append(values, cell)
		}
		tuples[i] = "(" + marks + ")"
	}
	columns := make([]string, len(names))
	for i, name := range names {
		columns[i] = mustIdent(name)
	}
	return Piece{
		text:   "insert into " + mustIdent(table) + " (" + strings.Join(columns, ", ") + ") values " + strings.Join(tuples, ", "),
		values: values,
	}
}

func indexOf(names []string, name string) (int, bool) {
	for i, n := range names {
		if n == name {
			return i, true
		}
	}
	return -1, false
}
