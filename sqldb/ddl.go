package sqldb

import (
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"reflect"
	"strconv"
	"strings"

	"github.com/tinyshed/tinystore/sqldb/internal/catalog"
)

// ddl is the table's CREATE TABLE and its indexes, as Schema prints them:
//
//	CREATE TABLE users (
//	    id           INTEGER PRIMARY KEY,
//	    email        TEXT NOT NULL,
//	    display_name TEXT NOT NULL
//	) STRICT;
//	CREATE UNIQUE INDEX users_email ON users (email);
func (t *table) ddl() string {
	width := 0
	for _, c := range t.columns {
		width = max(width, len(catalog.Quote(c.name)))
	}
	lines := make([]string, 0, len(t.columns)+len(t.checks)+1)
	for _, c := range t.columns {
		lines = append(lines, fmt.Sprintf("    %-*s %s", width, catalog.Quote(c.name), t.definition(c)))
	}
	if len(t.primaryKey) > 1 {
		lines = append(lines, "    PRIMARY KEY ("+quoteAll(t.primaryKey)+")")
	}
	for _, check := range t.checks {
		lines = append(lines, "    CHECK ("+check+")")
	}

	var out strings.Builder
	fmt.Fprintf(&out, "CREATE TABLE %s (\n%s\n) STRICT;\n", catalog.Quote(t.name), strings.Join(lines, ",\n"))
	for _, ix := range t.indexes {
		out.WriteString(ix.ddl(t.name))
	}
	return out.String()
}

func (ix *index) ddl(table string) string {
	unique := ""
	if ix.unique {
		unique = "UNIQUE "
	}
	return fmt.Sprintf("CREATE %sINDEX %s ON %s (%s);\n",
		unique, catalog.Quote(ix.name), catalog.Quote(table), quoteAll(ix.columns))
}

// definition is what follows a column's name in its table:
//
//	TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags))
func (t *table) definition(c *column) string {
	parts := []string{c.storage.String()}
	numbered := c.generated && t.rowid(c)
	if !c.nullable && !numbered {
		parts = append(parts, "NOT NULL")
	}
	if len(t.primaryKey) == 1 && t.primaryKey[0] == c.name {
		parts = append(parts, "PRIMARY KEY")
	}
	if r := c.reference; r != nil {
		refers := fmt.Sprintf("REFERENCES %s (%s)", catalog.Quote(r.table), catalog.Quote(r.column))
		if r.action != 0 {
			refers += " ON DELETE " + r.action.String()
		}
		parts = append(parts, refers)
	}
	if c.def != "" {
		parts = append(parts, "DEFAULT "+c.def)
	}
	if check := c.typeCheck(); check != "" {
		parts = append(parts, "CHECK ("+check+")")
	}
	return strings.Join(parts, " ")
}

// typeCheck keeps what a STRICT column cannot: a bool 0 or 1, JSON valid, a
// date one, bytes of their length
func (c *column) typeCheck() string {
	name := catalog.Quote(c.name)
	switch {
	case c.logical == kindBool:
		return name + " IN (0, 1)"
	case c.logical == kindJSON:
		return "json_valid(" + name + ")"
	case c.logical == kindDate:
		return name + " IS date(" + name + ")"
	case c.length > 0:
		return fmt.Sprintf("length(%s) = %d", name, c.length)
	}
	return ""
}

// insertQuery is what Insert runs: the written columns, and RETURNING what the
// database generates, unless that is the rowid alone, which the result carries
//
//	INSERT INTO notes (title, done) VALUES (?, ?)
//	INSERT INTO tickets (title) VALUES (?) RETURNING id, code
func insertQuery(table string, written, returned []*column) string {
	query := "INSERT INTO " + catalog.Quote(table) + " DEFAULT VALUES"
	if len(written) > 0 {
		query = fmt.Sprintf("INSERT INTO %s (%s) VALUES (%s)", catalog.Quote(table), quoteAll(namesOf(written)),
			strings.TrimSuffix(strings.Repeat("?, ", len(written)), ", "))
	}
	if len(returned) > 0 {
		query += " RETURNING " + quoteAll(namesOf(returned))
	}
	return query
}

func namesOf(columns []*column) []string {
	names := make([]string, len(columns))
	for i, c := range columns {
		names[i] = c.name
	}
	return names
}

// literal is value as a DEFAULT spells it, once it is the column's type:
//
//	false → 0    []string{} → '[]'    "it's" → 'it''s'    []byte{1, 2} → X'0102'
func (c *column) literal(value any) (string, error) {
	given := reflect.ValueOf(value)
	if value == nil || (given.Kind() == reflect.Pointer && given.IsNil()) {
		if !c.nullable {
			return "", errors.New("NULL, and the column cannot be NULL")
		}
		return "NULL", nil
	}
	v, err := c.asField(given)
	if err != nil {
		return "", err
	}
	encoded, err := c.field.value.encodeInner(v, c.logical == kindUUID && c.storage == Blob)
	if err != nil {
		return "", err
	}
	return sqlLiteral(encoded)
}

// asField is a default given as a value of the field's type, or of the type
// a pointer, a sql.Null or a sqldb.JSON of it holds, or one convertible to it
// without changing kind, such as an untyped constant
func (c *column) asField(given reflect.Value) (reflect.Value, error) {
	inner := c.field.value.inner
	if given.Kind() == reflect.Pointer && given.Type().Elem() == inner {
		given = given.Elem()
	}
	switch {
	case given.Type() == inner:
		return given, nil
	case c.logical == kindJSON && inner.Kind() == reflect.Struct && given.Type() == inner.Field(0).Type:
		wrapped := reflect.New(inner).Elem()
		wrapped.Field(0).Set(given)
		return wrapped, nil
	}
	offered, err := classify(given.Type())
	if err == nil && sameKind(offered.kind, c.logical) && offered.wrap == bare && given.CanConvert(inner) {
		converted := given.Convert(inner)
		if fits(given, converted) {
			return converted, nil
		}
		return reflect.Value{}, fmt.Errorf("%v does not fit %s", given, inner)
	}
	return reflect.Value{}, fmt.Errorf("a %s, and the field is %s", given.Type(), c.field.typ)
}

// sameKind lets an integer default stand for a real column, as SQL's does
func sameKind(offered, wanted kind) bool {
	return offered == wanted || (offered == kindInteger && wanted == kindReal)
}

// fits says that converting a number lost nothing: it converts back to
// itself, and no sign turned
//
//	300 → uint8 44 → 44, refused    -1 → uint 18446744073709551615, refused
func fits(given, converted reflect.Value) bool {
	switch {
	case given.CanInt() && converted.CanUint() && given.Int() < 0:
		return false
	case given.CanUint() && converted.CanInt() && converted.Int() < 0:
		return false
	}
	return converted.Convert(given.Type()).Equal(given)
}

func sqlLiteral(value any) (string, error) {
	switch v := value.(type) {
	case nil:
		return "NULL", nil
	case int64:
		return strconv.FormatInt(v, 10), nil
	case float64:
		switch {
		case math.IsInf(v, 1):
			return "1e999", nil
		case math.IsInf(v, -1):
			return "-1e999", nil
		}
		return strconv.FormatFloat(v, 'g', -1, 64), nil
	case bool:
		return strconv.FormatInt(integerOf(v), 10), nil
	case string:
		return "'" + strings.ReplaceAll(v, "'", "''") + "'", nil
	case []byte:
		return "X'" + strings.ToUpper(hex.EncodeToString(v)) + "'", nil
	}
	return "", fmt.Errorf("sqldb cannot spell %T as a default", value)
}

func quoteAll(names []string) string {
	quoted := make([]string, len(names))
	for i, name := range names {
		quoted[i] = catalog.Quote(name)
	}
	return strings.Join(quoted, ", ")
}
