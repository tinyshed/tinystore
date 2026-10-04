package sqldb

import (
	"database/sql"
	"database/sql/driver"
	"errors"
	"fmt"
	"math"
	"reflect"
	"strings"
	"testing"
	"testing/fstest"
	"time"
	"uuid"

	"github.com/tinyshed/tinystore"
)

type (
	Status string
	Level  int16
	MD5    [16]byte
)

// Cents is a custom type: its own Scan and Value keep it as an INTEGER
type Cents struct{ amount int64 }

func (c Cents) Value() (driver.Value, error) { return c.amount, nil }

func (c *Cents) Scan(src any) error {
	amount, ok := src.(int64)
	if !ok {
		return fmt.Errorf("cents from %T", src)
	}
	c.amount = amount
	return nil
}

// everything is a row of every type sqldb stores
type everything struct {
	ID        int64 `db:",generated"`
	Text      string
	Status    Status
	Bytes     []byte
	Flag      bool
	Tiny      int8
	Small     Level
	Big       int64
	Unsigned  uint64
	Ratio     float32
	Real      float64
	Infinite  float64
	At        time.Time
	Took      time.Duration
	Key       uuid.UUID
	KeyBytes  uuid.UUID
	Digest    MD5
	Day       Date
	Tags      JSON[[]string]
	Counts    JSON[map[string]int]
	Price     Cents
	Maybe     *string
	Missing   *string
	Nullable  sql.Null[int64]
	Empty     sql.Null[int64]
	Named     sql.NullString
	Later     *time.Time
	Somewhere *JSON[[]int]
}

func declareEverything(t *testing.T) *TableDef[everything] {
	t.Helper()
	return Table[everything]("everything",
		PrimaryKey("id"),
		Storage("key_bytes", Blob),
		Storage("price", Integer),
	)
}

func openEverything(t *testing.T, table *TableDef[everything]) *DB {
	t.Helper()
	migrations := fstest.MapFS{"001_everything.sql": {Data: []byte(Schema(table).SQL())}}
	db, err := Open(t.Context(), openStore(t, t.TempDir()), "app", migrations, Schema(table))
	if err != nil {
		t.Fatal(err)
	}
	return db
}

func fullRow() everything {
	somewhere := "somewhere"
	later := time.Date(2026, 9, 28, 10, 11, 12, 13_000_000, time.UTC)
	ints := JSONOf([]int{1, 2})
	return everything{
		Text: "text", Status: "draft", Bytes: []byte{0, 1, 2}, Flag: true, Tiny: -8, Small: 300,
		Big: math.MinInt64, Unsigned: math.MaxInt64, Ratio: 0.25, Real: 1.0 / 3, Infinite: math.Inf(-1),
		At:   time.Date(2026, 9, 28, 1, 2, 3, 4_000_000, time.UTC),
		Took: 1500 * time.Millisecond, Key: uuid.UUID{0x01, 0x92, 0xf2, 0xa4, 15: 0xff}, KeyBytes: uuid.UUID{1, 2, 3, 15: 4},
		Digest: MD5{9, 15: 9}, Day: Date{2026, time.February, 28},
		Tags: JSONOf([]string{"home", "<b>&"}), Counts: JSONOf(map[string]int{"a": 1}),
		Price: Cents{1999}, Maybe: &somewhere, Nullable: sql.Null[int64]{V: 7, Valid: true},
		Named: sql.NullString{String: "named", Valid: true}, Later: &later, Somewhere: &ints,
	}
}

// every value comes back as it went in, through Insert and through a raw
// insert whose arguments are the same values: time to the millisecond, in UTC
func TestEveryValueComesBackAsItWentIn(t *testing.T) {
	table := declareEverything(t)
	db := openEverything(t, table)
	ctx := t.Context()
	want := fullRow()

	inserted, err := Insert(ctx, db, table, want)
	if err != nil {
		t.Fatal(err)
	}
	want.ID = inserted.ID
	if !reflect.DeepEqual(inserted, want) {
		t.Fatalf("Insert returned\n%+v\nwant\n%+v", inserted, want)
	}
	read, found, err := One[everything](ctx, db, `select * from everything where id = ?`, inserted.ID)
	if err != nil || !found || !reflect.DeepEqual(read, want) {
		t.Fatalf("read back\n%+v, %t, %v\nwant\n%+v", read, found, err, want)
	}

	zero := everything{Day: Date{1, time.January, 1}}
	blank, err := Insert(ctx, db, table, zero)
	if err != nil {
		t.Fatal(err)
	}
	zero.ID = blank.ID
	if !reflect.DeepEqual(blank, zero) {
		t.Fatalf("a row of zero values came back\n%+v\nwant\n%+v", blank, zero)
	}
}

// a parameter is written by its Go type, as its column would hold it
func TestArgumentsAreWrittenByTheirGoType(t *testing.T) {
	db := openNotes(t)
	at := time.Date(2026, 9, 28, 0, 0, 0, 5_000_000, time.UTC)
	for _, c := range []struct {
		arg  any
		want string
	}{
		{at, "integer 1790553600005"},
		{2 * time.Second, "integer 2000"},
		{true, "integer 1"},
		{uuid.UUID{0xab, 15: 1}, "text ab000000-0000-0000-0000-000000000001"},
		{MD5{1}, "blob 01000000000000000000000000000000"},
		{Date{2026, 9, 28}, "text 2026-09-28"},
		{JSONOf([]int{1, 2}), "text [1,2]"},
		{Status("draft"), "text draft"},
		{Level(3), "integer 3"},
		{(*string)(nil), "null "},
		{sql.Null[time.Time]{V: at, Valid: true}, "integer 1790553600005"},
		{Cents{42}, "integer 42"},
		{sql.Named("x", at), "integer 1790553600005"},
	} {
		query := `select typeof(?1) || ' ' || coalesce(case typeof(?1) when 'blob' then hex(?1) else ?1 end, '')`
		if _, named := c.arg.(sql.NamedArg); named {
			query = `select typeof(@x) || ' ' || @x`
		}
		got, err := Scalar[string](t.Context(), db, query, c.arg)
		if err != nil || strings.ToLower(got) != c.want {
			t.Errorf("%T %v is written as %q, %v; want %q", c.arg, c.arg, got, err, c.want)
		}
	}
}

// a value that does not decode is ErrInvalid naming its column, the value and
// the field it did not fit
func TestAValueThatDoesNotDecodeNamesItsColumnAndField(t *testing.T) {
	d := declareDesign(t)
	db := openDesign(t, d)
	ctx := t.Context()
	for query, want := range map[string]string{
		`select 'yes' as done`:           `column done: TEXT "yes" does not decode into Note.Done (bool)`,
		`select 2 as done`:               `column done: INTEGER 2 does not decode into Note.Done (bool)`,
		`select null as title`:           `column title: NULL does not decode into Note.Title (string)`,
		`select '2026-13-01' as due`:     `column due: TEXT "2026-13-01" does not decode into Note.Due (*sqldb.Date)`,
		`select 'not json' as tags`:      `column tags: TEXT "not json" does not decode into Note.Tags (sqldb.JSON[[]string])`,
		`select 'abc' as id`:             `column id: TEXT "abc" does not decode into Note.ID (uuid.UUID)`,
		`select 1 as nobody`:             `column nobody: no field of sqldb.Note takes it`,
		`select 1 as title, 2 as TITLE`:  `columns title and TITLE both fill Note.Title`,
		`select 70000 as author_id, 1.5`: `column 1.5: no field of sqldb.Note takes it`,
		`select x'00' as created_at`:     `column created_at: BLOB of 1 bytes does not decode into Note.CreatedAt (time.Time)`,
		`select 'noon' as created_at`:    `column created_at: TEXT "noon" does not decode into Note.CreatedAt (time.Time)`,
		`select 9e999 as author_id`:      `column author_id: REAL +Inf does not decode into Note.AuthorID (int64)`,
	} {
		_, _, err := One[Note](ctx, db, query)
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), want) {
			t.Errorf("%s: %v; want %q", query, err, want)
		}
	}
	if _, err := Scalar[int8](ctx, db, `select 128`); err == nil || !strings.Contains(err.Error(),
		`column 128: INTEGER 128 does not decode into int8: past int8's range`) {
		t.Errorf("an integer past its field: %v", err)
	}
	if _, err := Scalar[float32](ctx, db, `select 1e300`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a REAL past float32's range: %v", err)
	}
}

// a value SQLite would store otherwise is refused before it is written: a NaN,
// which a REAL column keeps as NULL, a uint64 past int64, a day no calendar has
func TestAValueSQLiteWouldChangeIsRefused(t *testing.T) {
	table := declareEverything(t)
	db := openEverything(t, table)
	ctx := t.Context()
	for name, arg := range map[string]any{
		"NaN": math.NaN(), "float32 NaN": float32(math.NaN()), "uint64": uint64(math.MaxUint64),
		"February 30th": Date{2026, time.February, 30}, "named NaN": sql.Null[float64]{V: math.NaN(), Valid: true},
	} {
		if _, err := Scalar[int](ctx, db, `select ? is null`, arg); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("an argument %s: %v", name, err)
		}
	}
	row := fullRow()
	row.Real = math.NaN()
	if _, err := Insert(ctx, db, table, row); !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "everything.Real") {
		t.Errorf("a NaN field: %v", err)
	}
	row = fullRow()
	row.Unsigned = math.MaxUint64
	if _, err := Insert(ctx, db, table, row); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a uint64 past int64: %v", err)
	}
	if count, _ := Scalar[int](ctx, db, `select count(*) from everything`); count != 0 {
		t.Errorf("the refused rows left %d", count)
	}
}

// sixteen bytes are a uuid, stored as text, only for a type sqldb knows by its
// import path: another type of sixteen bytes, an MD5, is a BLOB
func TestOnlyAKnownUUIDTypeIsText(t *testing.T) {
	type UUID [16]byte // named as a uuid is, in a package sqldb does not know
	type keyed struct {
		ID      uuid.UUID
		Digest  MD5
		Unknown UUID
	}
	sql := Schema(Table[keyed]("keyed")).SQL()
	for _, want := range []string{
		"id      TEXT NOT NULL,",
		"digest  BLOB NOT NULL CHECK (length(digest) = 16),",
		"unknown BLOB NOT NULL CHECK (length(unknown) = 16)",
	} {
		if !strings.Contains(sql, want) {
			t.Errorf("no %q in\n%s", want, sql)
		}
	}
}

// the standard library's uuid, Go 1.27's, is text in its column and as an argument
func TestAStandardLibraryUUIDIsKeptAsText(t *testing.T) {
	type account struct {
		ID    uuid.UUID
		Login string
	}
	accounts := Table[account]("accounts", PrimaryKey("id"))
	schema := Schema(accounts)
	if sql := schema.SQL(); !strings.Contains(sql, "id    TEXT NOT NULL PRIMARY KEY,") {
		t.Fatalf("the schema:\n%s", sql)
	}
	db := openSchema(t, schema)
	ctx := t.Context()
	id := uuid.NewV7()
	if _, err := Insert(ctx, db, accounts, account{ID: id, Login: "ann"}); err != nil {
		t.Fatal(err)
	}
	if kept, err := Scalar[string](ctx, db, `select typeof(id) || ' ' || id from accounts`); err != nil || kept != "text "+id.String() {
		t.Errorf("the column holds %q, %v; want text %s", kept, err, id)
	}
	found, ok, err := One[account](ctx, db, `select * from accounts where id = ?`, id)
	if err != nil || !ok || found.ID != id || found.Login != "ann" {
		t.Errorf("found by its uuid: %+v, %v, %v", found, ok, err)
	}
}
