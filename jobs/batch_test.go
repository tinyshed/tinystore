package jobs_test

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/sqldb"
)

type batchNote struct {
	ID   int64
	Body string
}

var (
	batchNotes      = sqldb.Table[batchNote]("notes", sqldb.PrimaryKey("id"))
	batchSchema     = sqldb.Schema(batchNotes)
	batchMigrations = fstest.MapFS{"0001_notes.sql": {Data: []byte(
		`create table notes (id integer primary key, body text not null) strict;`)}}
)

// openBatchStore opens a store whose queues live in its database's file
func openBatchStore(t *testing.T, dir string) (*tinystore.Store, *sqldb.DB, *jobs.Queue[int64]) {
	t.Helper()
	ctx := t.Context()
	store, err := tinystore.Open(ctx, dir, tinystore.Options{Manual: true})
	if err != nil {
		t.Fatal(err)
	}
	db, err := sqldb.Open(ctx, store, "app", batchMigrations, batchSchema)
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	queues, err := jobs.Open(ctx, store, jobs.Options{In: db})
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	index, err := jobs.OpenQueue[int64](ctx, queues, "index")
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	return store, db, index
}

// A job a batch adds commits with the batch's rows or not at all, in the
// database's own file, which reopens with its schema as the program declared
// it and the job still there.
func TestAJobInABatchCommitsWithItsRows(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	store, db, index := openBatchStore(t, dir)

	err := db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`insert into notes (id, body) values (1, 'kept')`)
		b.Add(index.Enqueued(ctx, 1))
		return nil
	})
	if err != nil {
		t.Fatalf("a note and its job: %v", err)
	}
	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Add(index.Enqueued(ctx, 2))
		b.Exec(`insert into notes (id, body) values (2, null)`)
		return nil
	})
	var broken *sqldb.ConstraintError
	if !errors.As(err, &broken) {
		t.Fatalf("a note that breaks a constraint, with its job: %v", err)
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}

	store, db, index = openBatchStore(t, dir)
	defer store.Close(context.WithoutCancel(ctx))
	notes, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil || notes != 1 {
		t.Fatalf("%d notes after a batch kept and one broken: %v", notes, err)
	}
	var handled []int64
	err = index.Work(ctx, func(_ context.Context, job jobs.Job[int64]) error {
		handled = append(handled, job.Value)
		return nil
	}, jobs.UntilIdle())
	if err != nil || len(handled) != 1 || handled[0] != 1 {
		t.Fatalf("the jobs handled %v: %v", handled, err)
	}
	if found, _ := filepath.Glob(filepath.Join(dir, "jobs.db*")); len(found) != 0 {
		t.Fatalf("a store In a database made %v", found)
	}
}

// A queue whose store lives in jobs.db has no place in a database's batch, and
// one database holds one store's queues.
func TestAJobGoesOnlyInTheDatabaseItsQueueLivesIn(t *testing.T) {
	ctx := t.Context()
	store, db, _ := openBatchStore(t, t.TempDir())
	defer store.Close(context.WithoutCancel(ctx))

	if _, err := jobs.Open(ctx, store, jobs.Options{In: db}); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("a second store in the database: %v", err)
	}
	apart, err := jobs.Open(ctx, store, jobs.Options{})
	if err != nil {
		t.Fatal(err)
	}
	elsewhere, err := jobs.OpenQueue[int64](ctx, apart, "elsewhere")
	if err != nil {
		t.Fatal(err)
	}
	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`insert into notes (id, body) values (1, 'not without its job')`)
		b.Add(elsewhere.Enqueued(ctx, 1))
		return nil
	})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a job for jobs.db in the database's batch: %v", err)
	}
	if notes, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`); err != nil || notes != 0 {
		t.Fatalf("%d notes after a batch whose job had no place: %v", notes, err)
	}
}

// A job added inside a transaction commits with the rows the transaction
// wrote, and goes when it rolls back; a view, which writes nothing, refuses it.
func TestAJobInATxCommitsWithItsRows(t *testing.T) {
	ctx := t.Context()
	store, db, index := openBatchStore(t, t.TempDir())
	defer store.Close(context.WithoutCancel(ctx))

	kept := index.Enqueued(ctx, 1)
	err := db.Tx(ctx, func(tx *sqldb.Tx) error {
		if _, err := tx.Exec(ctx, `insert into notes (id, body) values (1, 'kept')`); err != nil {
			return err
		}
		return tx.Add(ctx, kept)
	})
	if err != nil {
		t.Fatalf("a note and its job in a transaction: %v", err)
	}
	errChanged := errors.New("changed its mind")
	dropped := index.Enqueued(ctx, 2)
	err = db.Tx(ctx, func(tx *sqldb.Tx) error {
		if _, execErr := tx.Exec(ctx, `insert into notes (id, body) values (2, 'dropped')`); execErr != nil {
			return execErr
		}
		if addErr := tx.Add(ctx, dropped); addErr != nil {
			return addErr
		}
		return errChanged
	})
	if !errors.Is(err, errChanged) {
		t.Fatalf("a transaction that rolled back: %v", err)
	}
	viewed := index.Enqueued(ctx, 3)
	err = db.View(ctx, func(tx *sqldb.Tx) error { return tx.Add(ctx, viewed) })
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a job added in a view: %v", err)
	}

	notes, err := sqldb.Scalar[int](ctx, db, `select count(*) from notes`)
	if err != nil || notes != 1 {
		t.Fatalf("%d notes after a transaction kept and one rolled back: %v", notes, err)
	}
	var handled []int64
	err = index.Work(ctx, func(_ context.Context, job jobs.Job[int64]) error {
		handled = append(handled, job.Value)
		return nil
	}, jobs.UntilIdle())
	if err != nil || len(handled) != 1 || handled[0] != 1 {
		t.Fatalf("the jobs handled %v: %v", handled, err)
	}
	// every change let go of its queue's turn and memory: the queue takes more
	if used := store.Memory().Used; used != 0 {
		t.Fatalf("%d bytes still held after every change ended", used)
	}
	if err = index.Enqueue(ctx, 4); err != nil {
		t.Fatal(err)
	}
}

// a guest of a store whose queues live in its database writes the database's
// rows, and opens none of the queues nor writes their tables, whose state the
// owner keeps in memory
func TestAGuestOpensNoQueuesOfADatabase(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	owner, _, index := openBatchStore(t, dir)
	defer owner.Close(context.Background())
	if err := index.Enqueue(ctx, 1); err != nil {
		t.Fatal(err)
	}
	guest, err := tinystore.Open(ctx, dir, tinystore.Options{Guest: true})
	if err != nil {
		t.Fatal(err)
	}
	defer guest.Close(context.Background())
	db, err := sqldb.Open(ctx, guest, "app", batchMigrations, batchSchema)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = jobs.Open(ctx, guest, jobs.Options{In: db}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a guest's queues: %v", err)
	}
	if _, err = jobs.Open(ctx, guest, jobs.Options{}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a guest's jobs.db: %v", err)
	}
	if _, err = db.Exec(ctx, `delete from _tinystore_jobs`); !errors.Is(err, tinystore.ErrInvalid) ||
		!strings.Contains(err.Error(), "none of the store's own tables") {
		t.Fatalf("a guest emptied the queues: %v", err)
	}
	if _, err = db.Exec(ctx, `insert into notes (body) values ('from the guest')`); err != nil {
		t.Fatal(err)
	}
}
