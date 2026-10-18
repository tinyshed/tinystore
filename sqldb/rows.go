package sqldb

import (
	"encoding/json"
	"fmt"
	"math"
	"reflect"
	"sort"
	"strings"
	"sync"
	"time"
	"unicode"

	"github.com/tinyshed/tinystore/internal/core"
)

// SQLite's values as the program's types, and back. A row's names are its
// columns' names, matched with a field's db tag, its json tag, or its name
// in snake case: AuthorID reads author_id. A field typed bool reads 0 and 1,
// a time.Time unix milliseconds, and a slice, a map or a struct JSON text;
// a type with a Scan method scans its value. A value goes as SQLite keeps
// it, and one it would change is refused before it leaves: a NaN, an
// integer past int64.

// Row is a row as SQLite gave it, by its columns' names.
type Row = map[string]any

var timeType = reflect.TypeFor[time.Time]()

// scanner is a type that reads a value of the database itself, as database/sql's Scanner does.
type scanner interface{ Scan(src any) error }

// cellOf is a value as it leaves, refusing one that would arrive changed,
// before anything is sent.
func cellOf(value any, where string) (any, error) {
	switch v := value.(type) {
	case nil, bool, string, []byte:
		return v, nil
	case int64:
		return v, nil
	case float64:
		if math.IsNaN(v) {
			return nil, core.Errorf(core.Invalid, "%s is NaN, which SQLite would keep as NULL", where)
		}
		return v, nil
	case time.Time:
		return v.UnixMilli(), nil
	case fmt.Stringer:
		if r := reflect.ValueOf(value); r.Kind() == reflect.Array || r.Kind() == reflect.Struct {
			return v.String(), nil
		}
	}
	r := reflect.ValueOf(value)
	switch r.Kind() {
	case reflect.Pointer:
		if r.IsNil() {
			return nil, nil
		}
		return cellOf(r.Elem().Interface(), where)
	case reflect.Bool:
		return r.Bool(), nil
	case reflect.String:
		return r.String(), nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return r.Int(), nil
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		if r.Uint() > math.MaxInt64 {
			return nil, core.Errorf(core.Invalid, "%s, %d, is past what an int64 holds", where, r.Uint())
		}
		return int64(r.Uint()), nil
	case reflect.Float32, reflect.Float64:
		return cellOf(r.Float(), where)
	case reflect.Slice:
		if r.Type().Elem().Kind() == reflect.Uint8 {
			return r.Bytes(), nil
		}
		return nil, core.Errorf(core.Invalid, "%s is a slice: a list for in (...) is sqldb.List(...), a JSON value sqldb.JSON(...)", where)
	}
	return nil, core.Errorf(core.Invalid, "%s is a %T: a JSON value is sqldb.JSON(...)", where, value)
}

// fieldOf is a row's field as it is written: a slice, a map or a struct as its JSON.
func fieldOf(value any, name string) (any, error) {
	r := reflect.ValueOf(value)
	for r.Kind() == reflect.Pointer && !r.IsNil() {
		r = r.Elem()
	}
	switch {
	case !r.IsValid():
		return nil, nil
	case r.Type() == timeType:
	case r.Kind() == reflect.Slice && r.Type().Elem().Kind() != reflect.Uint8, r.Kind() == reflect.Map,
		r.Kind() == reflect.Struct && !isStringer(r):
		text, err := json.Marshal(r.Interface())
		if err != nil {
			return nil, core.Wrap(core.Invalid, err, "%s: a value JSON cannot write", name)
		}
		return string(text), nil
	}
	return cellOf(r.Interface(), name)
}

func isStringer(r reflect.Value) bool {
	_, ok := r.Interface().(fmt.Stringer)
	return ok
}

// sliceOf gives a slice's items, for in (...), and false for any other value and for bytes.
func sliceOf(value any) ([]any, bool) {
	r := reflect.ValueOf(value)
	if r.Kind() != reflect.Slice && r.Kind() != reflect.Array || r.Type().Elem().Kind() == reflect.Uint8 {
		return nil, false
	}
	items := make([]any, r.Len())
	for i := range items {
		items[i] = r.Index(i).Interface()
	}
	return items, true
}

// snake is a field's name as a column names it: AuthorID is author_id, TS ts.
func snake(name string) string {
	runes := []rune(name)
	var out strings.Builder
	for i, c := range runes {
		if i > 0 && unicode.IsUpper(c) {
			before := runes[i-1]
			lowerAfter := i+1 < len(runes) && unicode.IsLower(runes[i+1])
			if unicode.IsLower(before) || unicode.IsDigit(before) || (unicode.IsUpper(before) && lowerAfter) {
				out.WriteByte('_')
			}
		}
		out.WriteRune(unicode.ToLower(c))
	}
	return out.String()
}

// columnOf is the column a struct field reads, "" for one it skips.
func columnOf(field reflect.StructField) string {
	if !field.IsExported() {
		return ""
	}
	for _, tag := range []string{"db", "json"} {
		if name, _, _ := strings.Cut(field.Tag.Get(tag), ","); name != "" {
			if name == "-" {
				return ""
			}
			return name
		}
	}
	return snake(field.Name)
}

// fields are a struct type's fields by the columns they read.
type fields struct {
	byColumn map[string][]int
	order    []string
}

var fieldsCache sync.Map

func fieldsOf(t reflect.Type) *fields {
	if found, ok := fieldsCache.Load(t); ok {
		return found.(*fields)
	}
	f := &fields{byColumn: map[string][]int{}}
	var walk func(t reflect.Type, index []int)
	walk = func(t reflect.Type, index []int) {
		for i := range t.NumField() {
			field := t.Field(i)
			at := append(append([]int{}, index...), i)
			if field.Anonymous && field.Type.Kind() == reflect.Struct && field.Tag.Get("db") == "" {
				walk(field.Type, at)
				continue
			}
			if column := columnOf(field); column != "" {
				if _, taken := f.byColumn[column]; !taken {
					f.byColumn[column] = at
					f.order = append(f.order, column)
				}
			}
		}
	}
	walk(t, nil)
	fieldsCache.Store(t, f)
	return f
}

// rowFields is a row the program gives, as its fields by column in order: a
// struct's, or a map's own sorted.
func rowFields(row any) ([]string, map[string]any, error) {
	r := reflect.ValueOf(row)
	for r.Kind() == reflect.Pointer && !r.IsNil() {
		r = r.Elem()
	}
	switch r.Kind() {
	case reflect.Map:
		values := map[string]any{}
		var names []string
		for _, key := range r.MapKeys() {
			name := fmt.Sprint(key.Interface())
			values[name] = r.MapIndex(key).Interface()
			names = append(names, name)
		}
		sort.Strings(names)
		return names, values, nil
	case reflect.Struct:
		f := fieldsOf(r.Type())
		values := map[string]any{}
		for _, column := range f.order {
			values[column] = r.FieldByIndex(f.byColumn[column]).Interface()
		}
		return f.order, values, nil
	}
	return nil, nil, core.Errorf(core.Invalid, "a row is a struct or a map, not a %T", row)
}

// readRow reads a row into a value of type T.
func readRow[T any](columns []string, values []any) (T, error) {
	var out T
	err := readInto(reflect.ValueOf(&out).Elem(), columns, values)
	return out, err
}

func readInto(into reflect.Value, columns []string, values []any) error {
	t := into.Type()
	switch {
	case t.Kind() == reflect.Map && t.Key().Kind() == reflect.String:
		m := reflect.MakeMapWithSize(t, len(columns))
		for i, column := range columns {
			if i < len(values) {
				v := reflect.New(t.Elem()).Elem()
				if err := readValue(v, values[i], column); err != nil {
					return err
				}
				m.SetMapIndex(reflect.ValueOf(column).Convert(t.Key()), v)
			}
		}
		into.Set(m)
		return nil
	case t.Kind() == reflect.Struct && t != timeType:
		f := fieldsOf(t)
		for i, column := range columns {
			index, ok := f.byColumn[column]
			if !ok || i >= len(values) {
				continue
			}
			if err := readValue(into.FieldByIndex(index), values[i], column); err != nil {
				return err
			}
		}
		return nil
	case t.Kind() == reflect.Pointer && t.Elem().Kind() == reflect.Struct:
		into.Set(reflect.New(t.Elem()))
		return readInto(into.Elem(), columns, values)
	}
	if len(values) == 1 {
		return readValue(into, values[0], columns[0])
	}
	return core.Errorf(core.Invalid, "a row reads into a struct or a map, not %s", t)
}

// readIncluded reads the rows an included query gave a row, which query.go sets.
var readIncluded func(into reflect.Value, value any, column string) (bool, error)

// readValue reads one value SQLite gave into a field.
func readValue(into reflect.Value, value any, column string) error {
	if done, err := readIncluded(into, value, column); done {
		return err
	}
	if into.CanAddr() {
		if s, ok := into.Addr().Interface().(scanner); ok {
			if err := s.Scan(value); err != nil {
				return core.Wrap(core.Invalid, err, "column %s: %v does not scan", column, value)
			}
			return nil
		}
	}
	if value == nil {
		into.Set(reflect.Zero(into.Type()))
		return nil
	}
	t := into.Type()
	refused := func() error {
		return core.Errorf(core.Invalid, "column %s: %v, a %T, does not read as %s", column, value, value, t)
	}
	switch {
	case t.Kind() == reflect.Interface:
		into.Set(reflect.ValueOf(value))
		return nil
	case t.Kind() == reflect.Pointer:
		v := reflect.New(t.Elem())
		if err := readValue(v.Elem(), value, column); err != nil {
			return err
		}
		into.Set(v)
		return nil
	case t == timeType:
		switch v := value.(type) {
		case int64:
			into.Set(reflect.ValueOf(time.UnixMilli(v).UTC()))
		case string:
			parsed, err := time.Parse(time.RFC3339Nano, v)
			if err != nil {
				return refused()
			}
			into.Set(reflect.ValueOf(parsed))
		default:
			return refused()
		}
		return nil
	}
	switch t.Kind() {
	case reflect.String:
		switch v := value.(type) {
		case string:
			into.SetString(v)
		case []byte:
			into.SetString(string(v))
		default:
			return refused()
		}
	case reflect.Bool:
		n, ok := value.(int64)
		if !ok || (n != 0 && n != 1) {
			return refused()
		}
		into.SetBool(n == 1)
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		n, ok := value.(int64)
		if !ok || into.OverflowInt(n) {
			return refused()
		}
		into.SetInt(n)
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		n, ok := value.(int64)
		if !ok || n < 0 || into.OverflowUint(uint64(n)) {
			return refused()
		}
		into.SetUint(uint64(n))
	case reflect.Float32, reflect.Float64:
		switch v := value.(type) {
		case float64:
			into.SetFloat(v)
		case int64:
			into.SetFloat(float64(v))
		default:
			return refused()
		}
	case reflect.Slice:
		if t.Elem().Kind() == reflect.Uint8 {
			switch v := value.(type) {
			case []byte:
				into.SetBytes(append([]byte{}, v...))
			case string:
				into.SetBytes([]byte(v))
			default:
				return refused()
			}
			return nil
		}
		return readJSON(into, value, column, refused)
	case reflect.Map, reflect.Struct, reflect.Array:
		return readJSON(into, value, column, refused)
	default:
		return refused()
	}
	return nil
}

func readJSON(into reflect.Value, value any, column string, refused func() error) error {
	var text []byte
	switch v := value.(type) {
	case string:
		text = []byte(v)
	case []byte:
		text = v
	default:
		return refused()
	}
	if err := json.Unmarshal(text, into.Addr().Interface()); err != nil {
		return core.Wrap(core.Invalid, err, "column %s: JSON that does not read as %s", column, into.Type())
	}
	return nil
}

// checkColumns refuses two columns of one name, which one row cannot hold.
func checkColumns(columns []string) error {
	seen := map[string]bool{}
	for _, column := range columns {
		if seen[column] {
			return core.Errorf(core.Invalid, "two columns named %s: name them apart with as", column)
		}
		seen[column] = true
	}
	return nil
}
