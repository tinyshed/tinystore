package search

import (
	"bytes"
	"encoding/json"
	"math"
	"reflect"
	"sort"

	"tinyshed.org/tinystore/internal/core"
	"tinyshed.org/tinystore/internal/wire"
	"tinyshed.org/tinystore/sqldb"
)

// Where keeps a search's hits to the documents whose filters pass, as
// sqldb's where reads a map: each field equal to its value, one of a slice,
// within a Range, or nil for none, every field named passing. Or and And
// group them. A search's Where holds 8 conditions and 1000 values in its
// slices at most. For a table's index, a value may be a statement of its
// database, sqldb.SQL("select user_id from members where course_id = ?", id):
// one of the values its rows give, read in the search's snapshot, 100,000 at
// most, counting toward no bound on values; on the table's key no filter is
// needed.
//
//	search.Where{"category": []string{"shoes", "boots"}, "price": search.Range{Gte: 10, Lt: 100}}
type Where map[string]any

// Range is a field's number within bounds: nil for a bound not given.
type Range struct {
	Gt, Gte, Lt, Lte any
}

// Group is conditions joined: Or's, And's.
type Group struct {
	join  string
	parts []Where
}

// Or is conditions any of which passes.
func Or(parts ...Where) Group { return Group{"or", parts} }

// And is conditions every one of which passes.
func And(parts ...Where) Group { return Group{"and", parts} }

func (w Where) search(q *wire.SearchQuery) {
	q.Statements = nil
	q.Where = whereText(w.conditions(&q.Statements))
}

func (g Group) search(q *wire.SearchQuery) {
	q.Statements = nil
	q.Where = whereText(g.conditions(&q.Statements))
}

// whereText is conditions as the wire takes them, a JSON list joined by and;
// "" for one that does not write, which the search refuses.
func whereText(conditions []any, err error) *string {
	text := ""
	if err == nil {
		var out bytes.Buffer
		encoder := json.NewEncoder(&out)
		encoder.SetEscapeHTML(false)
		if encoder.Encode(conditions) == nil {
			text = string(bytes.TrimSpace(out.Bytes()))
		}
	}
	return &text
}

func (g Group) conditions(statements *[]wire.SearchStatement) ([]any, error) {
	parts := make([]any, len(g.parts))
	for i, part := range g.parts {
		inner, err := part.conditions(statements)
		if err != nil {
			return nil, err
		}
		parts[i] = map[string]any{"and": inner}
	}
	return []any{map[string]any{g.join: parts}}, nil
}

// conditions are the fields' conditions, in the order of their names.
//
//	{"eq":[path,value]}  {"in":[path,[values]]}  {"missing":path}  {"range":[path,{"gte":n}]}
//
// and a statement {"in":[path,{"statement":n}]}, the nth of statements.
func (w Where) conditions(statements *[]wire.SearchStatement) ([]any, error) {
	paths := make([]string, 0, len(w))
	for path := range w {
		paths = append(paths, path)
	}
	sort.Strings(paths)
	out := make([]any, 0, len(paths))
	for _, path := range paths {
		condition, err := conditionOf(path, w[path], statements)
		if err != nil {
			return nil, err
		}
		out = append(out, condition)
	}
	return out, nil
}

func conditionOf(path string, value any, statements *[]wire.SearchStatement) (any, error) {
	switch v := value.(type) {
	case sqldb.Piece:
		text, values, err := sqldb.CoreStatement(v)
		if err != nil {
			return nil, err
		}
		*statements = append(*statements, wire.SearchStatement{Text: text, Values: values})
		return map[string]any{"in": []any{path, map[string]any{"statement": len(*statements) - 1}}}, nil
	case nil:
		return map[string]any{"missing": path}, nil
	case Range:
		bounds := map[string]any{}
		for name, bound := range map[string]any{"gt": v.Gt, "gte": v.Gte, "lt": v.Lt, "lte": v.Lte} {
			if bound == nil {
				continue
			}
			n, err := scalar(path, bound)
			if err != nil {
				return nil, err
			}
			bounds[name] = n
		}
		return map[string]any{"range": []any{path, bounds}}, nil
	}
	if list := reflect.ValueOf(value); list.Kind() == reflect.Slice && list.Type().Elem().Kind() != reflect.Uint8 {
		values := make([]any, list.Len())
		for i := range values {
			n, err := scalar(path, list.Index(i).Interface())
			if err != nil {
				return nil, err
			}
			values[i] = n
		}
		return map[string]any{"in": []any{path, values}}, nil
	}
	n, err := scalar(path, value)
	if err != nil {
		return nil, err
	}
	return map[string]any{"eq": []any{path, n}}, nil
}

func scalar(path string, value any) (any, error) {
	v := reflect.ValueOf(value)
	switch v.Kind() {
	case reflect.String, reflect.Bool, reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return value, nil
	case reflect.Float32, reflect.Float64:
		if math.IsNaN(v.Float()) || math.IsInf(v.Float(), 0) {
			return nil, core.Errorf(core.Invalid, "a where of %s: %v, which is no number JSON writes", path, value)
		}
		return value, nil
	}
	return nil, core.Errorf(core.Invalid, "a where of %s: %v, where text, a number or a bool is", path, value)
}
