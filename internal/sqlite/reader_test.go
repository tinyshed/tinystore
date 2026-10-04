package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"runtime"
	"runtime/metrics"
	"testing"
	"time"
)

func openReaderTestFile(t *testing.T) *File {
	t.Helper()
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "read.db"), Config{Readers: 1})
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

// kept is the statement a connection keeps for query, nil for none
func (r *preparedConnection) kept(query string) *sql.Stmt {
	if kept := r.statements[query]; kept != nil {
		return kept.statement
	}
	return nil
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
				original = connection.kept(query)
			} else if original != connection.kept(query) {
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
		for i := range 3 * keptStatements {
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
			if len(reader.(*readConnection).statements) > keptStatements {
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
		original := connection.kept(hot)
		for i := range keptStatements - 1 {
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
		if connection.kept(hot) != original || len(connection.statements) != keptStatements {
			t.Fatal("recently used read program was evicted")
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestSequentialReadsReuseOneWarmConnection(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "read.db"), Config{Readers: 4})
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
		statement = first.kept(query)
		return callbackError
	}); !errors.Is(err, callbackError) {
		t.Fatal(err)
	}
	if err := file.ViewPrepared(t.Context(), func(reader Reader) error {
		connection := reader.(*readConnection)
		if connection != first || connection.kept(query) != statement {
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
	file, err := Open(ctx, filepath.Join(b.TempDir(), "read.db"), Config{Readers: 4})
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

// each statement of a Lookup is its own snapshot: a commit between two of them
// is seen by the second, where a View keeps the first one's
func TestALookupReadsEachStatementFromItsOwnSnapshot(t *testing.T) {
	file := openReaderTestFile(t)
	insert := func() error {
		return file.Update(t.Context(), func(tx *sql.Tx) error {
			_, err := tx.ExecContext(t.Context(), `insert into example values(2)`)
			return err
		})
	}
	countTwice := func(reader Reader) ([2]int, error) {
		var counts [2]int
		for i := range counts {
			if err := QueryRow(t.Context(), reader, `select count(*) from example`).Scan(&counts[i]); err != nil {
				return counts, err
			}
			if err := insert(); err != nil {
				return counts, err
			}
		}
		return counts, nil
	}

	var looked, viewed [2]int
	err := file.Lookup(t.Context(), func(reader Reader) (err error) {
		looked, err = countTwice(reader)
		return err
	})
	if err == nil {
		err = file.ViewPrepared(t.Context(), func(reader Reader) (err error) {
			viewed, err = countTwice(reader)
			return err
		})
	}
	if err != nil || looked != [2]int{1, 2} || viewed != [2]int{3, 3} {
		t.Fatalf("a Lookup counted %v and a View %v, %v; want [1 2] and [3 3]", looked, viewed, err)
	}
}

// a connection keeps as many statements as its file was opened with, the one
// used longest ago closed first
func TestAConnectionKeepsTheStatementsItsFileWasOpenedWith(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "kept.db"), Config{Readers: 1, Statements: 128})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := file.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	err = file.Lookup(t.Context(), func(reader Reader) error {
		connection := reader.(*readConnection)
		var value int
		for i := range 129 {
			if readErr := QueryRow(t.Context(), reader, fmt.Sprintf(`select %d`, i)).Scan(&value); readErr != nil {
				return readErr
			}
			if i == 0 {
				continue
			}
			if readErr := QueryRow(t.Context(), reader, `select 0`).Scan(&value); readErr != nil {
				return readErr
			}
		}
		if len(connection.statements) != 128 || connection.kept(`select 0`) == nil || connection.kept(`select 1`) != nil {
			t.Fatalf("%d statements kept; the one used throughout kept %t, the oldest other %t",
				len(connection.statements), connection.kept(`select 0`) != nil, connection.kept(`select 1`) != nil)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

// A statement by key whose context can end starts no goroutine, where
// database/sql and the driver would each start one to watch the context.
func TestAStatementByKeyStartsNoGoroutine(t *testing.T) {
	file := openReaderTestFile(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	read := func() error {
		return file.Lookup(ctx, func(reader Reader) error {
			var n int
			return QueryRowByKey(ctx, reader, `select n from example where rowid = ?`, 1).Scan(&n)
		})
	}
	if err := read(); err != nil {
		t.Fatal(err)
	}
	runtime.GC() // The collector's workers start here rather than during the reads.

	const reads = 200
	before := goroutinesStarted(t)
	for range reads {
		if err := read(); err != nil {
			t.Fatal(err)
		}
	}
	if started := goroutinesStarted(t) - before; started > reads/10 {
		t.Fatalf("%d reads by key started %d goroutines", reads, started)
	}
}

func goroutinesStarted(t *testing.T) uint64 {
	t.Helper()
	sample := []metrics.Sample{{Name: "/sched/goroutines-created:goroutines"}}
	metrics.Read(sample)
	if sample[0].Value.Kind() != metrics.KindUint64 {
		t.Fatalf("the runtime does not count %s", sample[0].Name)
	}
	return sample[0].Value.Uint64()
}

// A statement by key whose context has ended does not run.
func TestAStatementByKeyWhoseContextEndedDoesNotRun(t *testing.T) {
	file := openReaderTestFile(t)
	ended, end := context.WithCancel(t.Context())
	end()
	err := file.UpdatePrepared(t.Context(), func(w Writer) error {
		var n int
		return QueryRowByKey(ended, w, `insert into example values(2) returning n`).Scan(&n)
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("a write by key whose context had ended: %v", err)
	}
	if rows := countOf(t, file, `select count(*) from example`); rows != 1 {
		t.Fatalf("%d rows after a write whose context had ended; want 1", rows)
	}
}

// A statement that walks a range ends at its deadline, however long the rest
// of the walk would take.
func TestARangeReadEndsAtItsDeadline(t *testing.T) {
	file := openReaderTestFile(t)
	ctx, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	const walk = `with recursive n(i) as (select 1 union all select i + 1 from n where i < 1000000000)
		select count(*) from n`
	err := file.Lookup(ctx, func(reader Reader) error {
		var count int
		return QueryRow(ctx, reader, walk).Scan(&count)
	})
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a walk of a billion rows with 50 ms to run: %v", err)
	}
}

// a burst of reads opens a reader each; those beyond one close once idle, and
// the next read opens a reader as the first was: it refuses to write
func TestAnIdleReaderClosesAndTheNextReadOpensIt(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "idle.db"), Config{Readers: 4, ReaderIdle: 30 * time.Millisecond})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = file.Close() })
	if err = file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict;`)); err != nil {
		t.Fatal(err)
	}

	held, release := make(chan struct{}), make(chan struct{})
	done := make(chan error, 4)
	for range 4 {
		go func() {
			done <- file.View(t.Context(), func(tx *sql.Tx) error {
				held <- struct{}{}
				<-release
				return nil
			})
		}()
	}
	for range 4 {
		<-held
	}
	close(release)
	for range 4 {
		if err = <-done; err != nil {
			t.Fatal(err)
		}
	}
	if idle := file.IdleReaders(); idle != 4 {
		t.Fatalf("%d readers idle after a burst of four", idle)
	}
	// a sweep takes readers off the idle list before it closes them
	unused := func() bool { return file.IdleReaders() == 1 && file.reader.Stats().OpenConnections == 1 }
	for deadline := time.Now().Add(5 * time.Second); !unused() && time.Now().Before(deadline); {
		time.Sleep(10 * time.Millisecond)
	}
	if idle, open := file.IdleReaders(), file.reader.Stats().OpenConnections; idle != 1 || open != 1 {
		t.Fatalf("%d readers idle, %d open, after they were unused; want one", idle, open)
	}

	err = file.Lookup(t.Context(), func(r Reader) error {
		rows, writeErr := r.QueryContext(t.Context(), `insert into example values (1) returning n`)
		if writeErr != nil {
			return writeErr
		}
		return EachRow(rows, "an insert", func(*sql.Rows) error { return nil })
	})
	if err == nil {
		t.Fatal("a reader opened again wrote")
	}
}
