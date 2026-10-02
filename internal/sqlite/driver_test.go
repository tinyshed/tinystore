package sqlite

import (
	"bytes"
	"context"
	"database/sql"
	"database/sql/driver"
	"errors"
	"reflect"
	"strings"
	"testing"
)

func openMemory(t *testing.T) *sql.DB {
	t.Helper()
	db, err := OpenDB(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = db.Close() })
	db.SetMaxOpenConns(1)
	return db
}

// a value comes back as SQLite keeps it, whatever its column declares or its
// text looks like: no time is read into text, no flag into an integer
func TestAValueComesBackAsSQLiteKeepsIt(t *testing.T) {
	db := openMemory(t)
	if _, err := db.ExecContext(t.Context(), `create table kept (at datetime, done boolean, stamp timestamp, body blob);
		insert into kept values ('2024-01-02', 1, '2026-10-02T10:00:00.123Z', x'00ff'),
			(1700000000, 0, 2460000.5, x'')`); err != nil {
		t.Fatal(err)
	}
	rows, err := db.QueryContext(t.Context(), `select at, done, stamp, body, '2026-10-02T10:00:00.123Z', null
		from kept order by rowid`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	var got [][]any
	for rows.Next() {
		row := make([]any, 6)
		into := make([]any, len(row))
		for i := range row {
			into[i] = &row[i]
		}
		if err = rows.Scan(into...); err != nil {
			t.Fatal(err)
		}
		got = append(got, row)
	}
	want := [][]any{
		{"2024-01-02", int64(1), "2026-10-02T10:00:00.123Z", []byte{0, 0xff}, "2026-10-02T10:00:00.123Z", nil},
		{int64(1700000000), int64(0), 2460000.5, []byte(nil), "2026-10-02T10:00:00.123Z", nil},
	}
	if err = rows.Err(); err != nil || !reflect.DeepEqual(got, want) {
		t.Fatalf("rows came back as\n%#v, %v\nwant\n%#v", got, err, want)
	}
}

// a blob scanned again into the slice that held the last row's leaves those
// bytes as they were, as database/sql's own copy would
func TestABlobScannedAgainLeavesTheLastOnesBytes(t *testing.T) {
	db := openMemory(t)
	rows, err := db.QueryContext(t.Context(), `select x'0102030405' union all select x'0909'`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	var reused []byte
	var kept [][]byte
	for rows.Next() {
		if err = rows.Scan(&reused); err != nil {
			t.Fatal(err)
		}
		kept = append(kept, reused)
	}
	if err = rows.Err(); err != nil || len(kept) != 2 ||
		!bytes.Equal(kept[0], []byte{1, 2, 3, 4, 5}) || !bytes.Equal(kept[1], []byte{9, 9}) {
		t.Fatalf("the rows kept %x, %v", kept, err)
	}
}

// a call prepares one statement: what follows it may be space, semicolons and
// comments, and anything more is refused rather than left unrun
func TestACallPreparesOneStatement(t *testing.T) {
	db := openMemory(t)
	for _, c := range []struct {
		sql     string
		refused string
	}{
		{"select ?; -- a note", ""},
		{"select ?;;\n/* a comment left open", ""},
		{"select ? -- ;\n; select 2", "multiple statements"},
		{"select ?; select 2", "multiple statements"},
		{"select ?; /* */ -", "multiple statements"},
		{"-- only a comment ?", "no statement"},
	} {
		var one int64
		err := db.QueryRowContext(t.Context(), c.sql, 1).Scan(&one)
		switch {
		case c.refused == "" && (err != nil || one != 1):
			t.Errorf("%q: %d, %v", c.sql, one, err)
		case c.refused != "" && (err == nil || !strings.Contains(err.Error(), c.refused)):
			t.Errorf("%q: %v, want it refused as %s", c.sql, err, c.refused)
		}
	}
}

func TestATransactionBeginsAsItsPoolSays(t *testing.T) {
	for name, want := range map[string]string{
		":memory:":                                   "BEGIN DEFERRED",
		"file:/data/a.db?mode=rw":                    "BEGIN DEFERRED",
		"file:/data/a.db?_txlock=deferred":           "BEGIN DEFERRED",
		"file:/data/a.db?_txlock=immediate&mode=rwc": "BEGIN IMMEDIATE",
		"file:/data/a.db?_txlock=exclusive":          "BEGIN EXCLUSIVE",
	} {
		if got, err := beginOf(name); err != nil || got != want {
			t.Errorf("%s begins with %q, %v; want %q", name, got, err, want)
		}
	}
	if _, err := beginOf("file:/data/a.db?_txlock=concurrent"); err == nil {
		t.Error("an unknown lock taken")
	}
}

// Next, which database/sql before Go 1.27 calls, reads what Scan reads
func TestNextReadsTheRowAsScanDoes(t *testing.T) {
	db := openMemory(t)
	conn, err := db.Conn(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	var row []driver.Value
	if err = conn.Raw(func(driverConn any) (readErr error) {
		row, readErr = firstRow(driverConn, `select 1, 1.5, 'text', x'00ff', null`)
		return readErr
	}); err != nil {
		t.Fatal(err)
	}
	if want := []driver.Value{int64(1), 1.5, "text", []byte{0, 0xff}, nil}; !reflect.DeepEqual(row, want) {
		t.Errorf("Next read %#v, want %#v", row, want)
	}
}

// firstRow reads query's first row through the driver's own Next
func firstRow(driverConn any, query string) (_ []driver.Value, err error) {
	prepared, err := driverConn.(driver.ConnPrepareContext).PrepareContext(context.Background(), query)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, prepared.Close()) }()
	read, err := prepared.(driver.StmtQueryContext).QueryContext(context.Background(), nil)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, read.Close()) }()
	row := make([]driver.Value, len(read.Columns()))
	return row, read.Next(row)
}
