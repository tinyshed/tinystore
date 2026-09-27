package sqldb

import (
	"fmt"
	"os"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestExecsMeasured inserts a message a call into the application's database
// from 1 to 512 goroutines for three seconds each, through Exec, whose writes
// share a commit, and through a Tx a call, which is what Exec was before: a
// commit, and its fsync, each. Skipped unless TINYSTORE_SPIKE=1;
// TINYSTORE_SQL_DIR puts its files on a volume of their own.
func TestExecsMeasured(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") != "1" {
		t.Skip("a measurement: set TINYSTORE_SPIKE=1")
	}
	const insert = `insert into notes (title, body) values (?, ?)`
	for _, workers := range []int{1, 8, 64, 512} {
		for _, way := range []string{"a Tx each", "Exec"} {
			db := openMeasuredNotes(t)
			var next atomic.Int64
			calls, elapsed := loadFor(3*time.Second, workers, func() error {
				n := next.Add(1)
				body := fmt.Sprint("see you at nine by the station, message ", n)
				if way == "Exec" {
					_, err := db.Exec(t.Context(), insert, fmt.Sprint("note ", n), body)
					return err
				}
				return db.Tx(t.Context(), func(tx *Tx) error {
					_, err := tx.Exec(t.Context(), insert, fmt.Sprint("note ", n), body)
					return err
				})
			})
			t.Logf("%3d goroutines  %-9s  %8.0f inserts a second", workers, way, float64(calls)/elapsed.Seconds())
		}
	}
}

func openMeasuredNotes(t *testing.T) *DB {
	t.Helper()
	dir := t.TempDir()
	if root := os.Getenv("TINYSTORE_SQL_DIR"); root != "" {
		var err error
		if dir, err = os.MkdirTemp(root, "execs-"); err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = os.RemoveAll(dir) })
	}
	db, err := Open(t.Context(), openStore(t, dir), "app", notesMigrations)
	if err != nil {
		t.Fatal(err)
	}
	return db
}

// loadFor calls call from workers goroutines until length has passed, and
// returns how many calls returned nil and how long they took
func loadFor(length time.Duration, workers int, call func() error) (int64, time.Duration) {
	var calls atomic.Int64
	started := time.Now()
	deadline := started.Add(length)
	var running sync.WaitGroup
	for range workers {
		running.Go(func() {
			for time.Now().Before(deadline) {
				if call() == nil {
					calls.Add(1)
				}
			}
		})
	}
	running.Wait()
	return calls.Load(), time.Since(started)
}
