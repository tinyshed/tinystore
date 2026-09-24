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

func TestPreparedReadCacheKeepsRecentlyUsedProgram(t *testing.T) {
	file := openReaderTestFile(t)
	const hot = `select n from example`
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		connection := reader.(*readConnection)
		var value int
		if err := QueryRow(t.Context(), reader, hot).Scan(&value); err != nil {
			return err
		}
		original := connection.statements[hot]
		for i := range readerStatements - 1 {
			query := fmt.Sprintf(`select %d`, i)
			if err := QueryRow(t.Context(), reader, query).Scan(&value); err != nil {
				return err
			}
		}
		if err := QueryRow(t.Context(), reader, hot).Scan(&value); err != nil {
			return err
		}
		if err := QueryRow(t.Context(), reader, `select 999`).Scan(&value); err != nil {
			return err
		}
		if connection.statements[hot] != original || len(connection.statements) != readerStatements {
			t.Fatal("recently used read program was evicted")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestSequentialReadsReuseOneWarmConnection(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "read.db"), 4)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := file.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict; insert into example values(1);`)); err != nil {
		t.Fatal(err)
	}
	var first *readConnection
	for range 4 {
		if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
			connection := reader.(*readConnection)
			if first == nil {
				first = connection
			} else if connection != first {
				t.Fatal("sequential read acquired a cold connection")
			}
			var value int
			return QueryRow(t.Context(), reader, `select n from example`).Scan(&value)
		}); err != nil {
			t.Fatal(err)
		}
	}
	if got := file.reader.Stats().OpenConnections; got != 1 {
		t.Fatalf("opened %d reader connections for sequential reads", got)
	}
}

func TestApplicationErrorKeepsPreparedReader(t *testing.T) {
	file := openReaderTestFile(t)
	const query = `select n from example`
	callbackError := errors.New("application rejected result")
	var first *readConnection
	var statement *sql.Stmt
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		first = reader.(*readConnection)
		var value int
		if err := QueryRow(t.Context(), reader, query).Scan(&value); err != nil {
			return err
		}
		statement = first.statements[query]
		return callbackError
	}); !errors.Is(err, callbackError) {
		t.Fatal(err)
	}
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		connection := reader.(*readConnection)
		if connection != first || connection.statements[query] != statement {
			t.Fatal("application error discarded a healthy reader")
		}
		var value int
		return QueryRow(t.Context(), reader, query).Scan(&value)
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

func TestEachRowStopsAtTheFirstErrorAndClosesTheRows(t *testing.T) {
	file := openReaderTestFile(t)
	stop := errors.New("stop")
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		rows, err := reader.QueryContext(t.Context(), `select value from json_each('[1,2,3]')`)
		if err != nil {
			return err
		}
		defer rows.Close()
		visited := 0
		err = EachRow(rows, "numbers", func(*sql.Rows) error {
			visited++
			return stop
		})
		if !errors.Is(err, stop) || visited != 1 || err.Error() != "read numbers: stop" {
			t.Fatalf("visited %d rows: %v", visited, err)
		}
		if rows.Next() {
			t.Fatal("rows stayed open after EachRow")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func BenchmarkPreparedReadAfterApplicationError(b *testing.B) {
	ctx := b.Context()
	file, err := Open(ctx, filepath.Join(b.TempDir(), "read.db"), 4)
	if err != nil {
		b.Fatal(err)
	}
	b.Cleanup(func() {
		if err := file.Close(); err != nil {
			b.Error(err)
		}
	})
	if err := file.Migrate(ctx, 1234, testMigrations(`create table example(n integer) strict; insert into example values(1);`)); err != nil {
		b.Fatal(err)
	}
	callbackError := errors.New("application rejected result")
	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		err := file.ViewPrepared(ctx, func(reader Reader) error {
			var value int
			if err := QueryRow(ctx, reader, `select n from example limit cast(? as integer)`, 1).Scan(&value); err != nil {
				return err
			}
			if value != 1 {
				b.Fatalf("value %d", value)
			}
			return callbackError
		})
		if !errors.Is(err, callbackError) {
			b.Fatal(err)
		}
	}
}
