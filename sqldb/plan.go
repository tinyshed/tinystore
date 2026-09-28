package sqldb

import (
	"database/sql"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/tinyshed/tinystore"
)

// plan is how the rows of a query decode into T: the field each column fills,
// found once for a type and its columns rather than every row
type plan struct {
	columns []string
	fields  []*field  // nil when T is one value, filled whole
	whole   valueType // T's own, when it is one value
	into    string    // T, as errors name it
}

// the column lists a type keeps plans for, the newest first
const plansKept = 16

// plans holds, for each type, its recent plans, replaced whole so that a
// read looks them up without a lock
var plans sync.Map // reflect.Type → *atomic.Pointer[[]*plan]

func planFor(t reflect.Type, columns []string) (*plan, error) {
	cached, _ := plans.LoadOrStore(t, new(atomic.Pointer[[]*plan]))
	kept, isList := cached.(*atomic.Pointer[[]*plan])
	if !isList {
		return makePlan(t, columns)
	}
	if list := kept.Load(); list != nil {
		for _, known := range *list {
			if slices.Equal(known.columns, columns) {
				return known, nil
			}
		}
	}
	made, err := makePlan(t, columns)
	if err != nil {
		return nil, err
	}
	list := []*plan{made}
	if old := kept.Load(); old != nil {
		list = append(list, (*old)[:min(len(*old), plansKept-1)]...)
	}
	kept.Store(&list)
	return made, nil
}

func makePlan(t reflect.Type, columns []string) (*plan, error) {
	made := &plan{columns: slices.Clone(columns), into: t.String()}
	if !isRecord(t) {
		return made, made.planWhole(t)
	}
	return made, made.planFields(modelOf(t))
}

// planWhole reads the one column a query returns into a T that is one value
func (p *plan) planWhole(t reflect.Type) error {
	if len(p.columns) != 1 {
		return fmt.Errorf("%w: %d columns into one %s", tinystore.ErrInvalid, len(p.columns), t)
	}
	value, err := classify(t)
	if err != nil {
		return fmt.Errorf("%w: column %s: %w", tinystore.ErrInvalid, p.columns[0], err)
	}
	p.whole = value
	return nil
}

// planFields finds the field each column fills: a column no field takes is an
// error, and a field no column fills keeps its zero value
func (p *plan) planFields(m *model) error {
	if m.err != nil {
		return fmt.Errorf("%w: %w", tinystore.ErrInvalid, m.err)
	}
	p.fields = make([]*field, len(p.columns))
	filled := make(map[int]string, len(p.columns))
	for i, column := range p.columns {
		index, found := m.byColumn[strings.ToLower(column)]
		if !found {
			return fmt.Errorf("%w: column %s: no field of %s takes it", tinystore.ErrInvalid, column, m.typ)
		}
		if other, twice := filled[index]; twice {
			return fmt.Errorf("%w: columns %s and %s both fill %s",
				tinystore.ErrInvalid, other, column, m.fields[index].name)
		}
		if err := m.fields[index].valueErr; err != nil {
			return fmt.Errorf("%w: column %s: %s: %w", tinystore.ErrInvalid, column, m.fields[index].name, err)
		}
		filled[index] = column
		p.fields[i] = &m.fields[index]
	}
	return nil
}

// rowReader decodes the rows of one query into T, reusing its holders from
// row to row; a row is scanned, weighed, then decoded
type rowReader[T any] struct {
	plan *plan
	raw  []any
	dest []any
}

func newRowReader[T any](rows *sql.Rows) (*rowReader[T], error) {
	columns, err := rows.Columns()
	if err != nil {
		return nil, err
	}
	found, err := planFor(reflect.TypeFor[T](), columns)
	if err != nil {
		return nil, err
	}
	reader := &rowReader[T]{plan: found, raw: make([]any, len(columns)), dest: make([]any, len(columns))}
	for i := range reader.raw {
		reader.dest[i] = &reader.raw[i]
	}
	return reader, nil
}

// scan takes the row's values from SQLite and says how many bytes they hold
func (r *rowReader[T]) scan(rows *sql.Rows) (int64, error) {
	if err := rows.Scan(r.dest...); err != nil {
		return 0, err
	}
	weight := sizeOf(reflect.TypeFor[T]())
	for _, raw := range r.raw {
		switch value := raw.(type) {
		case string:
			weight += int64(len(value))
		case []byte:
			weight += int64(len(value))
		default:
			weight += 8
		}
	}
	return weight, nil
}

// sizeOf is what a value of t holds itself, its strings and slices apart
func sizeOf(t reflect.Type) int64 {
	return int64(t.Size()) //nolint:gosec // no Go type is 2^63 bytes
}

// decode puts the values scan took into value
func (r *rowReader[T]) decode(value *T) error {
	root := reflect.ValueOf(value).Elem()
	if r.plan.fields == nil {
		return r.explain(0, r.plan.whole.decode(root, r.raw[0]), nil)
	}
	for i, target := range r.plan.fields {
		v, err := settable(root, target.index)
		if err == nil {
			err = target.value.decode(v, r.raw[i])
		}
		if err = r.explain(i, err, target); err != nil {
			return err
		}
	}
	return nil
}

// explain names the column and the field a value did not decode into:
//
//	column done: TEXT "yes" does not decode into Note.Done (bool)
func (r *rowReader[T]) explain(i int, err error, target *field) error {
	if err == nil {
		return nil
	}
	into := r.plan.into
	if target != nil {
		into = fmt.Sprintf("%s (%s)", target.name, target.typ)
	}
	detail := ""
	if !errors.Is(err, errMismatch) && !errors.Is(err, errNull) {
		detail = ": " + err.Error()
	}
	return fmt.Errorf("%w: column %s: %s does not decode into %s%s",
		tinystore.ErrInvalid, r.plan.columns[i], describe(r.raw[i]), into, detail)
}
