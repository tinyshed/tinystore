package sqldb

import (
	"bytes"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// One answers found, and two rows are an error; Scalar always answers, and
// no row is one; All of no rows is an empty slice
func TestOneAnswersFoundAndScalarAlwaysAnswers(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	if _, found, err := One[note](ctx, db, `select * from notes`); found || err != nil {
		t.Fatalf("One of no row: %t, %v", found, err)
	}
	if _, err := Scalar[int](ctx, db, `select id from notes`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("Scalar of no row: %v", err)
	}
	listed, err := All[note](ctx, db, `select * from notes`)
	if err != nil || listed == nil || len(listed) != 0 {
		t.Fatalf("All of no rows: %#v, %v", listed, err)
	}

	for _, title := range []string{"a", "b"} {
		if _, err = db.Exec(ctx, `insert into notes (title) values (?)`, title); err != nil {
			t.Fatal(err)
		}
	}
	if _, _, err = One[note](ctx, db, `select * from notes`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("One of two rows: %v", err)
	}
	if _, err = Scalar[note](ctx, db, `select * from notes limit 1`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("Scalar of a struct: %v", err)
	}
	if _, err = Scalar[int](ctx, db, `select id, title from notes limit 1`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("Scalar of two columns: %v", err)
	}
	ids, err := All[int64](ctx, db, `select id from notes order by id`)
	if err != nil || fmt.Sprint(ids) != "[1 2]" {
		t.Fatalf("All of one column: %v, %v", ids, err)
	}
	partly, found, err := One[note](ctx, db, `select title from notes where id = 2`)
	if err != nil || !found || partly != (note{Title: "b"}) {
		t.Fatalf("a row of fewer columns than fields: %+v, %v", partly, err)
	}
}

// Each reads one snapshot, a write during it unseen, and holds one row of the
// store's memory at a time where All would hold them all
func TestEachHoldsOneSnapshotAndOneRow(t *testing.T) {
	const rowBytes, rows = 256 << 10, 16
	store := openStoreWith(t, t.TempDir(), tinystore.Options{Manual: true, Memory: 2 << 20})
	db, err := Open(t.Context(), store, "app", notesMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	body := strings.Repeat("x", rowBytes)
	for range rows {
		if _, err = db.Exec(ctx, `insert into notes (title, body) values ('long', ?)`, body); err != nil {
			t.Fatal(err)
		}
	}

	if _, err = All[note](ctx, db, `select * from notes`); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("All of 4 MiB in a store of 2: %v", err)
	}
	read, most := 0, int64(0)
	for row, err := range Each[note](ctx, db, `select * from notes order by id`) {
		if err != nil {
			t.Fatal(err)
		}
		if read++; read == 1 {
			if _, err = db.Exec(ctx, `insert into notes (title) values ('during')`); err != nil {
				t.Fatal(err)
			}
		}
		most = max(most, store.Memory().Used)
		if len(row.Body) != rowBytes {
			t.Fatalf("a row of %d bytes", len(row.Body))
		}
	}
	if read != rows || most > 2*rowBytes {
		t.Fatalf("Each read %d rows of %d, holding at most %d bytes", read, rows, most)
	}
	for range Each[note](ctx, db, `select * from notes`) {
		break
	}
	if used := store.Memory().Used; used != 0 {
		t.Fatalf("%d bytes held after the reads", used)
	}
}

// a snapshot held past its bound ends the Each or the View holding it, which
// says to page by key
func TestASnapshotHeldPastItsBoundSaysSo(t *testing.T) {
	db, err := open(t.Context(), openStore(t, t.TempDir()), "app", notesMigrations, nil,
		tuning{snapshot: 50 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	ctx := t.Context()
	for range 3 {
		if _, err = db.Exec(ctx, `insert into notes (title) values ('slow')`); err != nil {
			t.Fatal(err)
		}
	}
	var last error
	for _, err := range Each[note](ctx, db, `select * from notes`) {
		time.Sleep(40 * time.Millisecond)
		last = err
	}
	if !errors.Is(last, tinystore.ErrLimit) || !strings.Contains(last.Error(), "page by key") {
		t.Fatalf("an Each held past its bound ended with %v", last)
	}
	err = db.View(ctx, func(tx *Tx) error {
		time.Sleep(80 * time.Millisecond)
		_, readErr := Scalar[int](ctx, tx, `select count(*) from notes`)
		return readErr
	})
	if !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a View held past its bound: %v", err)
	}
}

// All refuses past its own bound, which the store's memory need not reach
func TestAllPastItsBoundRefuses(t *testing.T) {
	h := &held{bound: 100}
	if err := h.take(60); err != nil {
		t.Fatal(err)
	}
	if err := h.take(60); !errors.Is(err, tinystore.ErrLimit) || !strings.Contains(err.Error(), "Each reads them") {
		t.Fatalf("past the bound: %v", err)
	}
}

// a statement is compiled once on a connection, which keeps it for the next
// call, whether it reads or writes
func TestAStatementIsCompiledOnceAConnection(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	if _, err := db.Exec(ctx, `insert into notes (title) values (?)`, "first"); err != nil {
		t.Fatal(err)
	}
	if _, _, err := One[note](ctx, db, `select * from notes where id = ?`, 1); err != nil {
		t.Fatal(err)
	}
	reads, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}
	readPrepares := db.file.ReaderPrepares()
	for i := range 50 {
		if _, _, err = One[note](ctx, db, `select * from notes where id = ?`, 1); err != nil {
			t.Fatal(err)
		}
		if _, err = db.Exec(ctx, `insert into notes (title) values (?)`, fmt.Sprint(i)); err != nil {
			t.Fatal(err)
		}
	}
	writes, err := db.file.WriterCounters(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if compiled := db.file.ReaderPrepares() - readPrepares; compiled != 0 {
		t.Fatalf("50 reads compiled %d statements more", compiled)
	}
	if compiled := writes.Prepared - reads.Prepared; compiled != 0 {
		t.Fatalf("50 writes compiled %d statements more", compiled)
	}
}

// Query reads rows without a struct: each column as SQLite returned it, and
// the columns' names even when no row comes
func TestQueryReadsRowsAsSQLiteReturnsThem(t *testing.T) {
	db := openNotes(t)
	ctx := t.Context()
	if _, err := db.Exec(ctx, `insert into notes (title, body) values ('first', 'hello')`); err != nil {
		t.Fatal(err)
	}

	rows, err := Query(ctx, db, `select id, title, 1.5 as half, x'00ff' as raw, null as missing from notes`)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(rows.Columns, ",") != "id,title,half,raw,missing" || len(rows.Values) != 1 {
		t.Fatalf("columns %v, %d rows", rows.Columns, len(rows.Values))
	}
	row := rows.Values[0]
	if row[0] != int64(1) || row[1] != "first" || row[2] != 1.5 || !bytes.Equal(row[3].([]byte), []byte{0, 0xff}) ||
		row[4] != nil {
		t.Fatalf("a row read as %#v", row)
	}

	empty, err := Query(ctx, db, `select title from notes where id = ?`, 99)
	if err != nil || strings.Join(empty.Columns, ",") != "title" || len(empty.Values) != 0 {
		t.Fatalf("no row: %+v, %v", empty, err)
	}

	returned, err := ExecQuery(ctx, db, `insert into notes (title) values (?) returning id, title`, "second")
	if err != nil || len(returned.Values) != 1 || returned.Values[0][1] != "second" {
		t.Fatalf("a write's returning: %+v, %v", returned, err)
	}
	if _, err := Query(ctx, db, `delete from notes`); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a write through Query: %v", err)
	}
}
