package sqlite

import (
	"bytes"
	"database/sql"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"testing/fstest"
)

func TestSnapshotCopiesWhileTheWriterWrites(t *testing.T) {
	dir := t.TempDir()
	f, err := Open(t.Context(), filepath.Join(dir, "x.db"), Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	scripts := fstest.MapFS{"0001.sql": {Data: []byte(`create table t (v integer) strict;`)}}
	if err = f.Migrate(t.Context(), 1, scripts); err != nil {
		t.Fatal(err)
	}
	insert := func() error {
		return f.Update(t.Context(), func(tx *sql.Tx) error {
			_, execErr := tx.ExecContext(t.Context(), `insert into t values (1)`)
			return execErr
		})
	}
	for range 100 {
		if err = insert(); err != nil {
			t.Fatal(err)
		}
	}

	var writes sync.WaitGroup
	writes.Go(func() {
		for range 100 {
			if insertErr := insert(); insertErr != nil {
				t.Error(insertErr)
				return
			}
		}
	})
	into := filepath.Join(dir, "copy", "x.db")
	applied, err := f.Snapshot(t.Context(), into)
	writes.Wait()
	if err != nil || applied != 1 {
		t.Fatalf("snapshot: %d migrations, %v", applied, err)
	}

	copied, err := Open(t.Context(), into, Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	defer copied.Close()
	var rows int
	if err = copied.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from t`).Scan(&rows)
	}); err != nil || rows < 100 || rows > 200 {
		t.Fatalf("the copy holds %d rows, %v; want one moment between 100 and 200", rows, err)
	}
	if _, err = f.Snapshot(t.Context(), into); err == nil {
		t.Fatal("a snapshot overwrote an existing file")
	}
}

// a copy is read as it was written: a reader of it changes no byte of it
func TestACopyIsReadWithoutWritingIt(t *testing.T) {
	dir := t.TempDir()
	f, err := Open(t.Context(), filepath.Join(dir, "x.db"), Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	scripts := fstest.MapFS{"0001.sql": {Data: []byte(`create table t (v integer) strict; insert into t values (7);`)}}
	if err = f.Migrate(t.Context(), 1, scripts); err != nil {
		t.Fatal(err)
	}
	into := filepath.Join(dir, "copy", "x.db")
	if _, err = f.Snapshot(t.Context(), into); err != nil {
		t.Fatal(err)
	}
	before, err := os.ReadFile(into)
	if err != nil {
		t.Fatal(err)
	}

	var value int
	err = ReadCopy(t.Context(), into, func(r Reader) error {
		return QueryRow(t.Context(), r, `select v from t`).Scan(&value)
	})
	if err != nil || value != 7 {
		t.Fatalf("the copy read %d: %v", value, err)
	}
	after, err := os.ReadFile(into)
	if err != nil || !bytes.Equal(before, after) {
		t.Fatalf("reading the copy changed it: %v", err)
	}
	if entries, _ := os.ReadDir(filepath.Dir(into)); len(entries) != 1 {
		t.Fatalf("reading the copy left %d files beside it", len(entries))
	}
}
