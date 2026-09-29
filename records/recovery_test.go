package records

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"os"
	"os/exec"
	"testing"
	"time"

	sqlite3 "modernc.org/sqlite"

	"github.com/tinyshed/tinystore"
)

// An abrupt exit loses no record an Append returned for, whenever it comes.
// The head is rows of the file, and a seal is one transaction that the next
// open finds whole or not at all.
func TestAnAbruptExitKeepsEveryAppendedRecord(t *testing.T) {
	want := sortedByTime(frontendRecords(3 * maxSegmentRecords))
	for _, moment := range []string{"appended", "publishing", "sealed"} {
		t.Run(moment, func(t *testing.T) {
			dir := t.TempDir()
			command := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestAbruptExitHelper$")
			command.Env = append(os.Environ(), "TINYSTORE_RECORDS_CRASH_DIR="+dir, "TINYSTORE_RECORDS_CRASH_AT="+moment)
			if output, err := command.CombinedOutput(); err != nil {
				t.Fatalf("child failed: %v\n%s", err, output)
			}

			s := openTestStore(t, dir, Options{}, tinystore.Options{})
			s.dropCrashTrigger(t)
			sealed := s.counts(t).segments
			sameRecords(t, want, s.readAll(t, Query{}))
			s.headStateMatchesItsRows(t)
			s.maintain(t)
			sameRecords(t, want, s.readAll(t, Query{}))
			wantSealed := 0
			if moment == "sealed" {
				wantSealed = 3
			}
			if sealed != wantSealed {
				t.Fatalf("%d segments sealed before exiting when %s, want %d", sealed, moment, wantSealed)
			}
		})
	}
}

// crashTrigger ends the process inside a publication, once the segment row and
// the first two blocks are inserted and before its head rows are deleted.
const crashTrigger = `create trigger crash after insert on blocks when new.id = 2 begin select exit_now(); end`

// TestAbruptExitHelper runs in the child: it appends three segments' worth,
// then exits without closing anything, at the moment the parent names
func TestAbruptExitHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_RECORDS_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	sqlite3.MustRegisterScalarFunction("exit_now", 0, func(*sqlite3.FunctionContext, []driver.Value) (driver.Value, error) {
		os.Exit(0)
		return nil, nil
	})
	store, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true, Clock: func() time.Time { return testNow }})
	if err != nil {
		t.Fatal(err)
	}
	logs, err := Open(t.Context(), store, Options{})
	if err != nil {
		t.Fatal(err)
	}
	fixture := frontendRecords(3 * maxSegmentRecords)
	for from := 0; from < len(fixture); from += maxBlockRecords {
		if err = logs.Append(t.Context(), fixture[from:from+maxBlockRecords]...); err != nil {
			t.Fatal(err)
		}
	}
	switch os.Getenv("TINYSTORE_RECORDS_CRASH_AT") {
	case "publishing":
		err = logs.file.Update(t.Context(), func(tx *sql.Tx) error {
			_, execErr := tx.ExecContext(t.Context(), crashTrigger)
			return execErr
		})
		if err == nil {
			_, err = logs.Maintain(context.Background())
		}
		t.Fatalf("the publication finished: %v", err)
	case "sealed":
		if _, err = logs.Maintain(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	os.Exit(0)
}

func (s *testStore) dropCrashTrigger(t *testing.T) {
	t.Helper()
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		_, execErr := tx.ExecContext(t.Context(), `drop trigger if exists crash`)
		return execErr
	})
	if err != nil {
		t.Fatal(err)
	}
}

// headStateMatchesItsRows checks that each head's state counts exactly the
// rows it has, as a transaction that writes one writes the other
func (s *testStore) headStateMatchesItsRows(t *testing.T) {
	t.Helper()
	const mismatched = `select count(*) from (
		select stream, late, sum(count) as count, sum(input) as input from heads group by stream, late
	) as rows full outer join head_state as state using (stream, late)
	where rows.count is not state.count or rows.input is not state.input`
	var wrong int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), mismatched).Scan(&wrong)
	})
	if err != nil || wrong != 0 {
		t.Fatalf("%d heads whose state does not count their rows, %v", wrong, err)
	}
}
