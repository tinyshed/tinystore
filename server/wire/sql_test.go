package wire_test

import (
	"errors"
	"math"
	"reflect"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/server/wire"
)

func TestSQLMessagesReadBackAsTheyWereWritten(t *testing.T) {
	values := []any{nil, int64(-7), 2.5, "text", []byte{0, 0xff}, true}
	messages := []struct {
		written interface{ Append([]byte) []byte }
		read    interface{ Decode([]byte) error }
	}{
		{wire.SQLDatabase{Name: "app", Migrations: []wire.SQLMigration{
			{Name: "0001_notes.sql", Text: "create table notes (id integer primary key) strict;"},
		}}, &wire.SQLDatabase{}},
		{wire.SQLStatement{
			Handle: 3, SQL: "select ?, :who", Args: values, Named: map[string]any{"who": "ada", "n": int64(1)},
			Write: true, Rows: true,
		}, &wire.SQLStatement{}},
		{wire.SQLDone{Changes: 2, LastID: 1 << 40}, &wire.SQLDone{}},
		{wire.SQLColumns{Columns: []string{"id", "title"}}, &wire.SQLColumns{}},
		{wire.SQLRow{Values: values}, &wire.SQLRow{}},
		{wire.SQLStatements{Handle: 4, Read: true, Statements: []wire.SQLStatement{
			{SQL: "select 1"}, {SQL: "insert into t values (?)", Args: []any{int64(1)}, Rows: true},
		}}, &wire.SQLStatements{}},
		{wire.SQLResults{Results: []wire.SQLResult{
			{SQLDone: wire.SQLDone{Changes: 1, LastID: 9}},
			{Columns: []string{"id"}, Rows: [][]any{{int64(9)}, {nil}}},
			{Columns: []string{}, Rows: [][]any{}},
		}}, &wire.SQLResults{}},
	}
	for _, m := range messages {
		if err := m.read.Decode(m.written.Append(nil)); err != nil {
			t.Errorf("%T: %v", m.written, err)
			continue
		}
		if got := reflect.ValueOf(m.read).Elem().Interface(); !reflect.DeepEqual(got, m.written) {
			t.Errorf("%T read as %+v", m.written, got)
		}
	}
}

// a value is one of SQLite's five, or a bool an argument may be; a float
// keeps its bits, and TEXT that is not UTF-8 travels as bin
func TestAnSQLValueIsOneOfSQLitesFive(t *testing.T) {
	nan := math.Float64frombits(0x7ff8000000000001)
	row := wire.SQLRow{Values: []any{math.Copysign(0, -1), nan, "\xff\x00"}}
	var read wire.SQLRow
	if err := read.Decode(row.Append(nil)); err != nil {
		t.Fatal(err)
	}
	if bits := math.Float64bits(read.Values[0].(float64)); bits != 1<<63 {
		t.Errorf("-0 read as %016x", bits)
	}
	if bits := math.Float64bits(read.Values[1].(float64)); bits != 0x7ff8000000000001 {
		t.Errorf("a NaN whose payload is 1 read as %016x", bits)
	}
	if text, ok := read.Values[2].([]byte); !ok || string(text) != "\xff\x00" {
		t.Errorf("TEXT that is not UTF-8 read as %#v", read.Values[2])
	}

	for _, refused := range [][]byte{
		wire.AppendUint(nil, 1<<63), // past int64, which SQLite would keep as a REAL
		wire.AppendArray(nil, 0),
		wire.AppendMap(nil, 0),
	} {
		d := wire.NewDecoder(refused)
		d.SQLValue()
		if !errors.Is(d.End(), wire.ErrMessage) {
			t.Errorf("%x taken as an SQL value", refused)
		}
	}
}

// a time the driver read from a column declared as one travels as SQLite
// spells it, since the text it was read from is gone
func TestATimeTravelsAsSQLiteSpellsIt(t *testing.T) {
	at := time.Date(2024, 1, 2, 3, 4, 5, 600_000_000, time.FixedZone("", 3*60*60))
	d := wire.NewDecoder(wire.AppendSQLValue(nil, at))
	if text := d.SQLValue(); text != "2024-01-02 03:04:05.6+03:00" {
		t.Errorf("a time written as %v", text)
	}
	defer func() {
		if recover() == nil {
			t.Error("a value of no SQL type written without a word")
		}
	}()
	wire.AppendSQLValue(nil, struct{}{})
}
