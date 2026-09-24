package sqldb

import (
	"database/sql"
	"fmt"
	"reflect"
	"sync"
	"time"
	"unicode"
)

// scanRow puts one row into T. A struct takes each column by its `db` tag, or
// by its field name in snake_case; anything else takes the only column:
//
//	type User struct {
//		ID        int64 `db:"id"`
//		CreatedAt int64          ← created_at
//	}
func scanRow[T any](rows *sql.Rows) (T, error) {
	var value T
	columns, err := rows.Columns()
	if err != nil {
		return value, err
	}

	target := reflect.ValueOf(&value).Elem()
	if !isRecord(target.Type()) {
		if len(columns) != 1 {
			return value, fmt.Errorf("%w: %d columns into one %s", ErrShape, len(columns), target.Type())
		}
		return value, rows.Scan(&value)
	}

	fields := fieldsOf(target.Type())
	destinations := make([]any, len(columns))
	for i, column := range columns {
		index, found := fields[column]
		if !found {
			return value, fmt.Errorf("%w: no field of %s takes column %q", ErrShape, target.Type(), column)
		}
		destinations[i] = target.FieldByIndex(index).Addr().Interface()
	}
	return value, rows.Scan(destinations...)
}

var (
	scannerType = reflect.TypeFor[sql.Scanner]()
	timeType    = reflect.TypeFor[time.Time]()
	fieldCache  sync.Map // reflect.Type → map[column][]int
)

// isRecord is a struct whose fields are columns, unlike time.Time or
// sql.NullString, which are one column each
func isRecord(t reflect.Type) bool {
	return t.Kind() == reflect.Struct && t != timeType && !reflect.PointerTo(t).Implements(scannerType)
}

// fieldsOf maps column names to fields, embedded structs' fields included
func fieldsOf(t reflect.Type) map[string][]int {
	if cached, ok := fieldCache.Load(t); ok {
		if fields, ok := cached.(map[string][]int); ok {
			return fields
		}
	}
	fields := map[string][]int{}
	for _, field := range reflect.VisibleFields(t) {
		if !field.IsExported() || field.Anonymous {
			continue
		}
		name := field.Tag.Get("db")
		if name == "-" {
			continue
		}
		if name == "" {
			name = snakeCase(field.Name)
		}
		fields[name] = field.Index
	}
	fieldCache.Store(t, fields)
	return fields
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
