package sqldb

import (
	"fmt"
	"reflect"
	"strings"
	"sync"
	"unicode"
)

// model is a struct's columns, in the order of its fields
type model struct {
	typ      reflect.Type
	fields   []field
	byColumn map[string]int // a column's name in lower case → its field
	err      error          // a tag sqldb cannot read, or two fields of one column
}

type field struct {
	column    string
	name      string // Note.CreatedAt, as errors name it
	typ       reflect.Type
	index     []int
	value     valueType
	valueErr  error // why sqldb cannot store the field's type
	generated bool
}

var models sync.Map

// modelOf is a struct's columns: CreatedAt is created_at, a db tag names a
// column otherwise or leaves the field out, and an embedded struct's fields
// count as the embedding one's:
//
//	type User struct {
//		ID        int64  `db:",generated"`   id, filled by the database
//		Name      string `db:"display_name"` display_name
//		CreatedAt time.Time                  created_at
//		Password  string `db:"-"`            no column
//	}
func modelOf(t reflect.Type) *model {
	if cached, ok := models.Load(t); ok {
		if found, isModel := cached.(*model); isModel {
			return found
		}
	}
	var found []candidate
	err := collectFields(t, t.Name(), nil, 0, &found)
	built := &model{typ: t, err: err}
	if err == nil {
		built.err = built.resolve(found)
	}
	models.Store(t, built)
	return built
}

type candidate struct {
	field
	depth int
}

func collectFields(t reflect.Type, owner string, index []int, depth int, found *[]candidate) error {
	for i := range t.NumField() {
		declared := t.Field(i)
		column, generated, err := parseTag(declared.Tag.Get("db"))
		if err != nil {
			return fmt.Errorf("%s.%s: %w", owner, declared.Name, err)
		}
		at := append(append([]int(nil), index...), i)
		switch {
		case column == "-":
			continue
		case promotes(declared, column):
			if generated {
				return fmt.Errorf("%s.%s: generated applies to a column, not an embedded struct", owner, declared.Name)
			}
			embedded := declared.Type
			if embedded.Kind() == reflect.Pointer {
				embedded = embedded.Elem()
			}
			if err = collectFields(embedded, owner, at, depth+1, found); err != nil {
				return err
			}
			continue
		case !declared.IsExported():
			continue
		}
		if column == "" {
			column = snakeCase(declared.Name)
		}
		value, valueErr := classify(declared.Type)
		*found = append(*found, candidate{depth: depth, field: field{
			column: column, name: owner + "." + declared.Name, typ: declared.Type, index: at,
			value: value, valueErr: valueErr, generated: generated,
		}})
	}
	return nil
}

// promotes says that an embedded field's own fields are columns: a struct, or
// a pointer to one, that is not itself one value as time.Time is
func promotes(declared reflect.StructField, column string) bool {
	if !declared.Anonymous || column != "" {
		return false
	}
	embedded := declared.Type
	if embedded.Kind() == reflect.Pointer {
		embedded = embedded.Elem()
	}
	return isRecord(embedded)
}

// isRecord is a struct whose fields are columns, unlike time.Time, a
// sql.Null, sqldb.JSON or a type with its own Scan, which are one column each
func isRecord(t reflect.Type) bool {
	if t.Kind() != reflect.Struct {
		return false
	}
	_, err := classify(t)
	return err != nil
}

// parseTag reads a db tag: "name", "-", ",generated" or "name,generated"
func parseTag(tag string) (column string, generated bool, err error) {
	column, options, _ := strings.Cut(tag, ",")
	for option := range strings.SplitSeq(options, ",") {
		switch option {
		case "":
		case "generated":
			generated = true
		default:
			return "", false, fmt.Errorf("the db tag %q has an option sqldb does not know, %q", tag, option)
		}
	}
	return column, generated, nil
}

// resolve keeps, of the fields that share a column's name, the one embedded
// least deeply, as Go promotes fields; two at one depth are an error
func (m *model) resolve(found []candidate) error {
	shallowest := map[string]int{}
	for _, c := range found {
		name := strings.ToLower(c.column)
		if depth, seen := shallowest[name]; !seen || c.depth < depth {
			shallowest[name] = c.depth
		}
	}
	m.byColumn = make(map[string]int, len(shallowest))
	for _, c := range found {
		name := strings.ToLower(c.column)
		if c.depth != shallowest[name] {
			continue
		}
		if other, taken := m.byColumn[name]; taken {
			return fmt.Errorf("%s and %s are both the column %s", m.fields[other].name, c.name, c.column)
		}
		m.byColumn[name] = len(m.fields)
		m.fields = append(m.fields, c.field)
	}
	return nil
}

// snakeCase is a field's column name:
//
//	ID → id    CreatedAt → created_at    UserID → user_id    HTTPCode → http_code
func snakeCase(name string) string {
	runes := []rune(name)
	out := make([]rune, 0, len(runes)+4)
	for i, r := range runes {
		if unicode.IsUpper(r) {
			afterLower := i > 0 && unicode.IsLower(runes[i-1])
			beforeLower := i > 0 && i+1 < len(runes) && unicode.IsLower(runes[i+1])
			if afterLower || beforeLower {
				out = append(out, '_')
			}
			r = unicode.ToLower(r)
		}
		out = append(out, r)
	}
	return string(out)
}

// settable is the field at index inside root, allocating on the way the
// embedded structs that a nil pointer stands for
func settable(root reflect.Value, index []int) (reflect.Value, error) {
	v := root
	for i, step := range index {
		if i > 0 && v.Kind() == reflect.Pointer {
			if v.IsNil() {
				if !v.CanSet() {
					return reflect.Value{}, fmt.Errorf("sqldb cannot set the embedded pointer to unexported %s",
						v.Type().Elem())
				}
				v.Set(reflect.New(v.Type().Elem()))
			}
			v = v.Elem()
		}
		v = v.Field(step)
	}
	return v, nil
}

// readable is the field at index inside root, or false when a nil embedded
// pointer stands for its struct
func readable(root reflect.Value, index []int) (reflect.Value, bool) {
	v := root
	for i, step := range index {
		if i > 0 && v.Kind() == reflect.Pointer {
			if v.IsNil() {
				return reflect.Value{}, false
			}
			v = v.Elem()
		}
		v = v.Field(step)
	}
	return v, true
}
