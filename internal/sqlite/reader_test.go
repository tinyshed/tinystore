package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"testing"
)

func openReaderTestFile(t *testing.T) *File {
	t.Helper()
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "read.db"), 1)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := file.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	if err = file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict; insert into example values(1);`)); err != nil {
		t.Fatal(err)
	}
	return file
}

func TestPreparedReadsKeepOneSnapshotAndRefreshTheNext(t *testing.T) {
	file := openReaderTestFile(t)
	const query = `select n from example limit cast(? as integer)`
	var original *sql.Stmt
	for pass := range 2 {
		if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
			for read := range 2 {
				var value int
				if err := QueryRow(t.Context(), reader, query, 1).Scan(&value); err != nil {
					return err
				}
				if value != pass+1 {
					t.Fatalf("snapshot %d read %d: got %d", pass, read, value)
				}
				if pass == 0 && read == 0 {
					if err := file.Update(t.Context(), func(tx *sql.Tx) error {
						_, err := tx.ExecContext(t.Context(), `update example set n=2`)
						return err
					}); err != nil {
						return err
					}
				}
			}
			connection := reader.(*readConnection)
			if pass == 0 {
				original = connection.statements[query]
			} else if original != connection.statements[query] {
				t.Fatal("reprepared an unchanged query")
			}
			return nil
		}); err != nil {
			t.Fatal(err)
		}
	}
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		var value int
		return QueryRow(t.Context(), reader, `delete from example returning n`).Scan(&value)
	}); err == nil {
		t.Fatal("prepared reader wrote")
	}
}

func TestPreparedReadCacheIsBoundedAndRebindsValues(t *testing.T) {
	file := openReaderTestFile(t)
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		for i := range 3 * readerStatements {
			query := fmt.Sprintf(`select ? + %d`, i)
			for _, argument := range []int{7, 13} {
				var value int
				if err := QueryRow(t.Context(), reader, query, argument).Scan(&value); err != nil {
					return err
				}
				if value != argument+i {
					t.Fatalf("stale binding: got %d, want %d", value, argument+i)
				}
			}
			if len(reader.(*readConnection).statements) > readerStatements {
				t.Fatal("unbounded prepared statements")
			}
		}
		var value int
		if err := QueryRow(t.Context(), reader, `select n from example where n=?`, 99).Scan(&value); !errors.Is(err, sql.ErrNoRows) {
			t.Fatalf("empty result: %v", err)
		}
		return QueryRow(t.Context(), reader, `select n from example where n=?`, 1).Scan(&value)
	}); err != nil {
		t.Fatal(err)
	}
}

func TestCancelledPreparedReadReleasesItsConnection(t *testing.T) {
	file := openReaderTestFile(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	err := file.ViewPrepared(ctx, func(reader Reader) error {
		var value int
		if err := QueryRow(ctx, reader, `select n from example`).Scan(&value); err != nil {
			return err
		}
		waiting, stopWaiting := context.WithCancel(ctx)
		stopWaiting()
		if err := file.ViewPrepared(waiting, func(Reader) error {
			t.Fatal("entered an occupied reader slot")
			return nil
		}); !errors.Is(err, context.Canceled) {
			t.Fatalf("reader admission: %v", err)
		}
		cancel()
		return QueryRow(ctx, reader, `select n from example`).Scan(&value)
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled read: %v", err)
	}
	if err = file.ViewPrepared(t.Context(), func(reader Reader) error {
		var value int
		return QueryRow(t.Context(), reader, `select n from example`).Scan(&value)
	}); err != nil {
		t.Fatalf("read after cancellation: %v", err)
	}
}
