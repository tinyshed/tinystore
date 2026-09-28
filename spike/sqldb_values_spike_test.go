package spike

import (
	"context"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"testing"
	"time"
	"unicode"
)

// sqldbNote is the model docs/sqldb.md declares, a [16]byte standing for a
// known uuid type
type sqldbNote struct {
	ID        [16]byte
	AuthorID  int64
	Title     string
	Done      bool
	Tags      []string // sqldb.JSON[[]string] in the engine
	Due       *sqldbDate
	CreatedAt time.Time
}

type sqldbDate struct {
	Year  int
	Month time.Month
	Day   int
}

var errSQLDBShape = errors.New("a value that does not fit its field")

func sqldbParseUUID(text string, id *[16]byte) error {
	if len(text) != 36 || text[8] != '-' || text[13] != '-' || text[18] != '-' || text[23] != '-' {
		return fmt.Errorf("%w: uuid %q", errSQLDBShape, text)
	}
	digits := text[0:8] + text[9:13] + text[14:18] + text[19:23] + text[24:36]
	_, err := hex.Decode(id[:], []byte(digits))
	return err
}

func sqldbParseDate(text string) (sqldbDate, error) {
	day, err := time.Parse(time.DateOnly, text)
	if err != nil {
		return sqldbDate{}, err
	}
	return sqldbDate{day.Year(), day.Month(), day.Day()}, nil
}

// decoding by hand: the floor any mapper is measured against
func sqldbDecodeByHand(rows *sql.Rows) (sqldbNote, error) {
	var (
		id, title, tags string
		author, done    int64
		created         int64
		due             sql.NullString
		note            sqldbNote
	)
	if err := rows.Scan(&id, &author, &title, &done, &tags, &due, &created); err != nil {
		return note, err
	}
	note.AuthorID, note.Title, note.Done = author, title, done == 1
	note.CreatedAt = time.UnixMilli(created).UTC()
	if err := sqldbParseUUID(id, &note.ID); err != nil {
		return note, err
	}
	if err := json.Unmarshal([]byte(tags), &note.Tags); err != nil {
		return note, err
	}
	if due.Valid {
		date, err := sqldbParseDate(due.String)
		if err != nil {
			return note, err
		}
		note.Due = &date
	}
	return note, nil
}

// sqldbColumnOf is a field's column name: CreatedAt → created_at, as sqldb
// names them
func sqldbColumnOf(field string) string {
	var out strings.Builder
	runes := []rune(field)
	for i, r := range runes {
		if unicode.IsUpper(r) {
			afterLower := i > 0 && unicode.IsLower(runes[i-1])
			beforeLower := i > 0 && i+1 < len(runes) && unicode.IsLower(runes[i+1])
			if afterLower || beforeLower {
				out.WriteByte('_')
			}
			r = unicode.ToLower(r)
		}
		out.WriteRune(r)
	}
	return out.String()
}

var sqldbFieldCache sync.Map // reflect.Type → map[column]int

func sqldbFields(t reflect.Type) map[string]int {
	if cached, ok := sqldbFieldCache.Load(t); ok {
		fields, _ := cached.(map[string]int)
		return fields
	}
	fields := map[string]int{}
	for i := range t.NumField() {
		fields[sqldbColumnOf(t.Field(i).Name)] = i
	}
	sqldbFieldCache.Store(t, fields)
	return fields
}

// sqldbHolderFor is what a column is scanned into before it becomes its
// field's value
func sqldbHolderFor(field reflect.Type) any {
	switch field {
	case reflect.TypeFor[time.Time](), reflect.TypeFor[int64](), reflect.TypeFor[bool]():
		return new(int64)
	case reflect.TypeFor[*sqldbDate]():
		return new(sql.NullString)
	}
	return new(string)
}

// sqldbConvert sets a field from what its column was scanned into
func sqldbConvert(field reflect.Value, holder any) error {
	switch dst := field.Addr().Interface().(type) {
	case *time.Time:
		ms, _ := holder.(*int64)
		*dst = time.UnixMilli(*ms).UTC()
	case *int64:
		value, _ := holder.(*int64)
		*dst = *value
	case *bool:
		value, _ := holder.(*int64)
		*dst = *value == 1
	case *string:
		value, _ := holder.(*string)
		*dst = *value
	case *[16]byte:
		value, _ := holder.(*string)
		return sqldbParseUUID(*value, dst)
	case *[]string:
		value, _ := holder.(*string)
		return json.Unmarshal([]byte(*value), dst)
	case **sqldbDate:
		value, _ := holder.(*sql.NullString)
		if !value.Valid {
			*dst = nil
			return nil
		}
		date, err := sqldbParseDate(value.String)
		*dst = &date
		return err
	default:
		return fmt.Errorf("%w: %s", errSQLDBShape, field.Type())
	}
	return nil
}

// decoding with reflection every row: the columns asked of the rows, a holder
// made for each, and each field found by name, as a mapper without a plan does
func sqldbDecodeEachRow(rows *sql.Rows) (sqldbNote, error) {
	var note sqldbNote
	columns, err := rows.Columns()
	if err != nil {
		return note, err
	}
	target := reflect.ValueOf(&note).Elem()
	fields := sqldbFields(target.Type())
	holders := make([]any, len(columns))
	for i, column := range columns {
		holders[i] = sqldbHolderFor(target.Field(fields[column]).Type())
	}
	if err = rows.Scan(holders...); err != nil {
		return note, err
	}
	for i, column := range columns {
		if err = sqldbConvert(target.Field(fields[column]), holders[i]); err != nil {
			return note, err
		}
	}
	return note, nil
}

// sqldbPlan decodes the rows of one query into T: its holders made once, each
// column's field and conversion found once
type sqldbPlan struct {
	fields  []int
	holders []any
}

func sqldbPlanFor(rows *sql.Rows, t reflect.Type) (*sqldbPlan, error) {
	columns, err := rows.Columns()
	if err != nil {
		return nil, err
	}
	fields := sqldbFields(t)
	plan := &sqldbPlan{fields: make([]int, len(columns)), holders: make([]any, len(columns))}
	for i, column := range columns {
		index, ok := fields[column]
		if !ok {
			return nil, fmt.Errorf("%w: no field takes %s", errSQLDBShape, column)
		}
		plan.fields[i] = index
		plan.holders[i] = sqldbHolderFor(t.Field(index).Type)
	}
	return plan, nil
}

func (p *sqldbPlan) decode(rows *sql.Rows) (sqldbNote, error) {
	var note sqldbNote
	if err := rows.Scan(p.holders...); err != nil {
		return note, err
	}
	target := reflect.ValueOf(&note).Elem()
	for i, field := range p.fields {
		if err := sqldbConvert(target.Field(field), p.holders[i]); err != nil {
			return note, err
		}
	}
	return note, nil
}

// TestSQLDBDecoding reads every one of 200,000 notes into sqldbNote three
// ways, twice each, and reports a row's time and allocations: by hand, with
// reflection every row, and with a plan made once for the query
func TestSQLDBDecoding(t *testing.T) {
	sqldbMeasuring(t)
	const count = 200_000
	path := filepath.Join(sqldbDir(t), "app.db")
	file := sqldbOpen(t, path, 1)
	sqldbCreate(t, file, count)
	pool := kvReaderPool(t, path, "cache_size(-65536)")
	ctx := t.Context()

	ways := []struct {
		name   string
		decode func(rows *sql.Rows) (func(*sql.Rows) (sqldbNote, error), error)
	}{
		{"by hand", func(*sql.Rows) (func(*sql.Rows) (sqldbNote, error), error) { return sqldbDecodeByHand, nil }},
		{"reflection every row", func(*sql.Rows) (func(*sql.Rows) (sqldbNote, error), error) {
			return sqldbDecodeEachRow, nil
		}},
		{"a plan made once", func(rows *sql.Rows) (func(*sql.Rows) (sqldbNote, error), error) {
			plan, err := sqldbPlanFor(rows, reflect.TypeFor[sqldbNote]())
			if err != nil {
				return nil, err
			}
			return plan.decode, nil
		}},
		{"scan alone, no conversion", func(*sql.Rows) (func(*sql.Rows) (sqldbNote, error), error) {
			var (
				id, title, tags string
				author, done    int64
				created         int64
				due             sql.NullString
			)
			return func(rows *sql.Rows) (sqldbNote, error) {
				return sqldbNote{}, rows.Scan(&id, &author, &title, &done, &tags, &due, &created)
			}, nil
		}},
	}
	for range 2 {
		for _, way := range ways {
			var before, after runtime.MemStats
			runtime.GC()
			runtime.ReadMemStats(&before)
			began := time.Now()
			read, err := sqldbDecodeEvery(ctx, pool, way.decode)
			if err != nil {
				t.Fatal(err)
			}
			elapsed := time.Since(began)
			runtime.ReadMemStats(&after)
			t.Logf("%-26s %7.0f ns a row  %5.1f allocations a row  %d rows", way.name,
				float64(elapsed.Nanoseconds())/float64(read), float64(after.Mallocs-before.Mallocs)/float64(read), read)
		}
	}
}

// sqldbDecodeEvery reads every note through the decoder a way makes for the
// query's rows, and says how many it read
func sqldbDecodeEvery(
	ctx context.Context, pool *sql.DB, decoder func(*sql.Rows) (func(*sql.Rows) (sqldbNote, error), error),
) (read int, err error) {
	rows, err := pool.QueryContext(ctx, `select id, author_id, title, done, tags, due, created_at from notes`)
	if err != nil {
		return 0, err
	}
	defer func() { err = errors.Join(err, rows.Close()) }()
	decode, err := decoder(rows)
	if err != nil {
		return 0, err
	}
	for rows.Next() {
		if _, err = decode(rows); err != nil {
			return read, err
		}
		read++
	}
	return read, rows.Err()
}

// the arguments of an insert of note, by hand and by reflection
func sqldbArgsByHand(note *sqldbNote) []any {
	tags, _ := json.Marshal(note.Tags)
	var due any
	if note.Due != nil {
		due = fmt.Sprintf("%04d-%02d-%02d", note.Due.Year, note.Due.Month, note.Due.Day)
	}
	done := 0
	if note.Done {
		done = 1
	}
	return []any{sqldbUUIDText(note.ID), note.AuthorID, note.Title, done, string(tags), due, note.CreatedAt.UnixMilli()}
}

// sqldbEncode is one field's value as its column holds it
func sqldbEncode(field reflect.Value) (any, error) {
	switch value := field.Interface().(type) {
	case [16]byte:
		return sqldbUUIDText(value), nil
	case bool:
		if value {
			return 1, nil
		}
		return 0, nil
	case []string:
		encoded, err := json.Marshal(value)
		return string(encoded), err
	case *sqldbDate:
		if value == nil {
			return nil, nil
		}
		return fmt.Sprintf("%04d-%02d-%02d", value.Year, value.Month, value.Day), nil
	case time.Time:
		return value.UnixMilli(), nil
	default:
		return value, nil
	}
}

func sqldbArgsByReflection(note *sqldbNote) ([]any, error) {
	target := reflect.ValueOf(note).Elem()
	args := make([]any, target.NumField())
	for i := range args {
		value, err := sqldbEncode(target.Field(i))
		if err != nil {
			return nil, err
		}
		args[i] = value
	}
	return args, nil
}

// TestSQLDBEncoding turns a note into an insert's arguments a million times,
// by hand and by reflection over its fields, and reports a note's time and
// allocations
func TestSQLDBEncoding(t *testing.T) {
	sqldbMeasuring(t)
	due := sqldbDate{2026, time.September, 28}
	note := &sqldbNote{
		ID: sqldbUUID(7), AuthorID: 42, Title: "a note about the station at nine", Done: true,
		Tags: []string{"home", "errands"}, Due: &due, CreatedAt: time.UnixMilli(sqldbEpoch),
	}
	const count = 1_000_000
	for range 2 {
		for _, way := range []struct {
			name   string
			encode func() error
		}{
			{"by hand", func() error { _ = sqldbArgsByHand(note); return nil }},
			{"reflection over fields", func() error { _, err := sqldbArgsByReflection(note); return err }},
		} {
			var before, after runtime.MemStats
			runtime.GC()
			runtime.ReadMemStats(&before)
			began := time.Now()
			for range count {
				if err := way.encode(); err != nil {
					t.Fatal(err)
				}
			}
			elapsed := time.Since(began)
			runtime.ReadMemStats(&after)
			t.Logf("%-24s %5.0f ns a note  %4.1f allocations a note", way.name,
				float64(elapsed.Nanoseconds())/count, float64(after.Mallocs-before.Mallocs)/count)
		}
	}
}
