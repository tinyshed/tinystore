package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

func openGroupTestFile(t *testing.T) *File {
	t.Helper()
	return openGroupTestFileWith(t, Config{Readers: 1})
}

func openGroupTestFileWith(t *testing.T, config Config) *File {
	t.Helper()
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "group.db"), config)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := file.Close(); closeErr != nil {
			t.Error(closeErr)
		}
	})
	if err = file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict;`)); err != nil {
		t.Fatal(err)
	}
	return file
}

// holdWriter keeps the writer busy until the returned function is called, so
// that grouped writes queue behind it
func holdWriter(t *testing.T, file *File) (release func()) {
	t.Helper()
	held, done := make(chan struct{}), make(chan struct{})
	holding := make(chan struct{})
	go func() {
		defer close(done)
		_ = file.Update(t.Context(), func(*sql.Tx) error {
			close(holding)
			<-held
			return nil
		})
	}()
	<-holding
	return func() {
		close(held)
		<-done
	}
}

// waitQueued waits until the group holds count writes
func waitQueued(t *testing.T, file *File, count int) {
	t.Helper()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(time.Millisecond) {
		file.writes.mu.Lock()
		queued := len(file.writes.queue)
		file.writes.mu.Unlock()
		if queued == count {
			return
		}
	}
	t.Fatalf("the group never held %d writes", count)
}

func insertGrouped(ctx context.Context, file *File, n int, fail error) error {
	return file.UpdateGrouped(ctx, 8, func(w Writer) error {
		if _, err := w.ExecContext(ctx, `insert into example values(?)`, n); err != nil {
			return err
		}
		return fail
	})
}

func countRows(t *testing.T, file *File) int {
	t.Helper()
	var count int
	err := file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from example`).Scan(&count)
	})
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// writes that queue while the writer is busy commit together, in one
// transaction and one fsync, and a write that fails is rolled back to its
// savepoint without failing the others
func TestGroupedWritesShareACommitAndFailAlone(t *testing.T) {
	file := openGroupTestFile(t)
	release := holdWriter(t, file)
	before := file.commits.Load()

	const writers = 64
	refused := errors.New("refused")
	errs := make([]error, writers)
	var wg sync.WaitGroup
	for n := range writers {
		var fail error
		if n == 3 {
			fail = refused
		}
		wg.Go(func() { errs[n] = insertGrouped(t.Context(), file, n, fail) })
	}
	waitQueued(t, file, writers)
	release()
	wg.Wait()

	for n, err := range errs {
		if (n == 3) != errors.Is(err, refused) || (n != 3 && err != nil) {
			t.Fatalf("write %d: %v", n, err)
		}
	}
	if rows := countRows(t, file); rows != writers-1 {
		t.Fatalf("%d rows, want %d: the refused write's insert must roll back alone", rows, writers-1)
	}
	if commits := file.commits.Load() - before; commits != 2 {
		t.Fatalf("%d commits after the held one, want the held one and one group", commits)
	}
}

// a caller whose context ends while its write waits leaves without writing; a
// write whose caller gives up once it has started finishes with its group
func TestACallerCancelledBeforeItsTurnWritesNothing(t *testing.T) {
	file := openGroupTestFile(t)
	release := holdWriter(t, file)

	leader := make(chan error, 1)
	go func() { leader <- insertGrouped(t.Context(), file, 1, nil) }()
	waitQueued(t, file, 1)

	ctx, cancel := context.WithCancel(t.Context())
	waiting := make(chan error, 1)
	go func() { waiting <- insertGrouped(ctx, file, 2, nil) }()
	waitQueued(t, file, 2)
	cancel()
	if err := <-waiting; !errors.Is(err, context.Canceled) {
		t.Fatalf("a cancelled waiting write returned %v", err)
	}
	release()
	if err := <-leader; err != nil {
		t.Fatal(err)
	}

	started, stop := context.WithCancel(t.Context())
	err := file.UpdateGrouped(started, 8, func(w Writer) error {
		stop()
		_, execErr := w.ExecContext(t.Context(), `insert into example values(3)`)
		return execErr
	})
	if err != nil {
		t.Fatalf("a write cancelled after it started: %v", err)
	}
	if rows := countRows(t, file); rows != 2 {
		t.Fatalf("%d rows, want the leader's and the started write's", rows)
	}
}

// a write heavier than a group's bound commits alone
func TestAHeavyGroupedWriteCommitsAlone(t *testing.T) {
	file := openGroupTestFile(t)
	release := holdWriter(t, file)
	before := file.commits.Load()

	var wg sync.WaitGroup
	errs := make([]error, 3)
	for n, bytes := range []int{8, groupBytes, 8} {
		wg.Go(func() {
			errs[n] = file.UpdateGrouped(t.Context(), bytes, func(w Writer) error {
				_, err := w.ExecContext(t.Context(), `insert into example values(?)`, n)
				return err
			})
		})
		waitQueued(t, file, n+1)
	}
	release()
	wg.Wait()
	if err := errors.Join(errs...); err != nil {
		t.Fatal(err)
	}
	if commits := file.commits.Load() - before; commits != 4 {
		t.Fatalf("%d commits after the held one, want the held one and three: before, alone, after", commits)
	}
}

// a transaction holding the writer past a group's hold fails none of the
// writes queued behind it: the hold counts from when a group holds the writer,
// and the engine is told once that the leader has waited
func TestALongTransactionFailsNoGroupedWriteBehindIt(t *testing.T) {
	var waited []string
	var mu sync.Mutex
	file := openGroupTestFileWith(t, Config{
		Readers: 1, GroupHold: 50 * time.Millisecond, Patience: 20 * time.Millisecond,
		Waited: func(label string) {
			mu.Lock()
			defer mu.Unlock()
			waited = append(waited, label)
		},
	})
	release := holdWriter(t, file)

	const writers = 8
	errs := make([]error, writers)
	var wg sync.WaitGroup
	for n := range writers {
		wg.Go(func() {
			errs[n] = file.UpdateGroupedAs(t.Context(), fmt.Sprint("write ", n), 8, func(w Writer) error {
				_, err := w.ExecContext(t.Context(), `insert into example values(?)`, n)
				return err
			})
		})
		waitQueued(t, file, n+1)
	}
	time.Sleep(200 * time.Millisecond)
	release()
	wg.Wait()

	if err := errors.Join(errs...); err != nil {
		t.Fatalf("writes behind a transaction four holds long: %v", err)
	}
	if rows := countRows(t, file); rows != writers {
		t.Fatalf("%d rows, want %d", rows, writers)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(waited) != 1 || waited[0] != "write 0" {
		t.Fatalf("the engine was told %q, want the leader's label once", waited)
	}
}

// a leader whose caller leaves while it waits for the writer writes nothing,
// and the write behind it leads and commits
func TestALeaderWhoseCallerLeavesHandsTheLeadOn(t *testing.T) {
	file := openGroupTestFile(t)
	release := holdWriter(t, file)

	ctx, cancel := context.WithCancel(t.Context())
	leader := make(chan error, 1)
	go func() { leader <- insertGrouped(ctx, file, 1, nil) }()
	waitQueued(t, file, 1)
	follower := make(chan error, 1)
	go func() { follower <- insertGrouped(t.Context(), file, 2, nil) }()
	waitQueued(t, file, 2)

	cancel()
	if err := <-leader; !errors.Is(err, context.Canceled) {
		t.Fatalf("a leader whose caller left: %v", err)
	}
	release()
	if err := <-follower; err != nil {
		t.Fatalf("the write behind it: %v", err)
	}
	var n int
	err := file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select group_concat(n) from example`).Scan(&n)
	})
	if err != nil || n != 2 {
		t.Fatalf("the file holds %d, %v; want the follower's 2 alone", n, err)
	}
}
