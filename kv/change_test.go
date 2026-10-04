package kv_test

import (
	"bytes"
	"context"
	"errors"
	"path/filepath"
	"sync"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
	"github.com/tinyshed/tinystore/sqldb"
)

type changeUser struct {
	ID    int64
	Email string
}

var (
	changeUsers      = sqldb.Table[changeUser]("users", sqldb.PrimaryKey("id"), sqldb.Unique("email"))
	changeSchema     = sqldb.Schema(changeUsers)
	changeMigrations = fstest.MapFS{"0001_users.sql": {Data: []byte(
		`create table users (id integer primary key, email text not null) strict;
		create unique index users_email on users (email);`)}}
)

// changeClock is a store's clock a test moves
type changeClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *changeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *changeClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

// openChangeStore opens a store whose kv store lives in its database's file
func openChangeStore(t *testing.T, dir string, clock *changeClock, options ...kv.BucketOption) (
	*tinystore.Store, *sqldb.DB, *kv.Store, *kv.Bucket[string],
) {
	t.Helper()
	ctx := t.Context()
	store, err := tinystore.Open(ctx, dir, tinystore.Options{Manual: true, Clock: clock.Now})
	if err != nil {
		t.Fatal(err)
	}
	db, err := sqldb.Open(ctx, store, "app", changeMigrations, changeSchema)
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	state, err := kv.Open(ctx, store, kv.Options{In: db})
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	sessions, err := kv.OpenBucket[string](ctx, state, "sessions", options...)
	if err != nil {
		t.Fatal(errors.Join(err, store.Close(ctx)))
	}
	return store, db, state, sessions
}

// A key a batch writes commits with the batch's rows or not at all, in the
// database's own file, which reopens with the key still there; a deletion and
// a clear go the same way.
func TestABucketInADatabaseCommitsWithItsRows(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, db, _, sessions := openChangeStore(t, dir, clock)

	err := db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`insert into users (id, email) values (1, 'ada@example.com')`)
		b.Add(sessions.Written(ctx, "K7Q2", "user 1", kv.TTL(time.Hour)))
		return nil
	})
	if err != nil {
		t.Fatalf("a user and their session: %v", err)
	}
	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Add(sessions.Written(ctx, "P9X4", "user 2"))
		b.Exec(`insert into users (id, email) values (2, 'ada@example.com')`)
		return nil
	})
	var broken *sqldb.ConstraintError
	if !errors.As(err, &broken) {
		t.Fatalf("a user that breaks a constraint, with their session: %v", err)
	}
	if err = store.Close(ctx); err != nil {
		t.Fatal(err)
	}

	store, db, _, sessions = openChangeStore(t, dir, clock)
	defer store.Close(context.WithoutCancel(ctx))
	if value, found, getErr := sessions.Get(ctx, "K7Q2"); getErr != nil || !found || value != "user 1" {
		t.Fatalf("the kept session after a reopen: %q, %v: %v", value, found, getErr)
	}
	if _, found, getErr := sessions.Get(ctx, "P9X4"); getErr != nil || found {
		t.Fatalf("the session of a broken batch: %v: %v", found, getErr)
	}
	if found, _ := filepath.Glob(filepath.Join(dir, "kv.db*")); len(found) != 0 {
		t.Fatalf("a store In a database made %v", found)
	}

	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`delete from users where id = 1`)
		b.Add(sessions.Deleted(ctx, "K7Q2"))
		return nil
	})
	if _, found, getErr := sessions.Get(ctx, "K7Q2"); err != nil || getErr != nil || found {
		t.Fatalf("a session deleted with its user: %v, %v, %v", found, err, getErr)
	}
	if err = sessions.Set(ctx, "Z1A8", "user 3"); err != nil {
		t.Fatal(err)
	}
	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`delete from users`)
		b.Add(sessions.Cleared(ctx))
		return nil
	})
	if _, found, getErr := sessions.Get(ctx, "Z1A8"); err != nil || getErr != nil || found {
		t.Fatalf("a bucket cleared with the users: %v, %v, %v", found, err, getErr)
	}
}

// A Sliding key of a store In a database is renewed by its reads, and the
// renewals flush into the database's file.
func TestSlidingExpiryWorksInADatabase(t *testing.T) {
	ctx := t.Context()
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, _, state, sessions := openChangeStore(t, t.TempDir(), clock, kv.Sliding(time.Hour))
	defer store.Close(context.WithoutCancel(ctx))

	if err := sessions.Set(ctx, "K7Q2", "user 1"); err != nil {
		t.Fatal(err)
	}
	for range 3 {
		clock.advance(50 * time.Minute)
		if _, found, err := sessions.Get(ctx, "K7Q2"); err != nil || !found {
			t.Fatalf("a session read within its term: %v: %v", found, err)
		}
		if _, err := state.Maintain(ctx); err != nil {
			t.Fatal(err)
		}
	}
	clock.advance(61 * time.Minute)
	if _, found, err := sessions.Get(ctx, "K7Q2"); err != nil || found {
		t.Fatalf("a session unread past its term: %v: %v", found, err)
	}
}

// A bucket whose store lives in kv.db has no place in a database's batch, and
// one inside a Tx makes no change.
func TestAChangeOfABucketElsewhereIsRefused(t *testing.T) {
	ctx := t.Context()
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, db, _, _ := openChangeStore(t, t.TempDir(), clock)
	defer store.Close(context.WithoutCancel(ctx))

	apart, err := kv.Open(ctx, store, kv.Options{})
	if err != nil {
		t.Fatal(err)
	}
	elsewhere, err := kv.OpenBucket[string](ctx, apart, "elsewhere")
	if err != nil {
		t.Fatal(err)
	}
	err = db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`insert into users (id, email) values (1, 'ada@example.com')`)
		b.Add(elsewhere.Written(ctx, "K7Q2", "user 1"))
		return nil
	})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a key of kv.db in the database's batch: %v", err)
	}
	if users, readErr := sqldb.Scalar[int](ctx, db, `select count(*) from users`); readErr != nil || users != 0 {
		t.Fatalf("%d users after a batch whose key had no place: %v", users, readErr)
	}

	err = apart.Tx(ctx, func(tx *kv.Tx) error {
		return db.Batch(ctx, func(b *sqldb.Batch) error {
			b.Add(elsewhere.WithTx(tx).Written(ctx, "K7Q2", "user 1"))
			return nil
		})
	})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a change of a bucket inside a Tx: %v", err)
	}
}

// A database holds one kv store, and a guest opens none in it.
func TestABatchHoldsEachKeysMemoryOnce(t *testing.T) {
	ctx := t.Context()
	store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{Manual: true, Memory: 2 << 20})
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close(context.Background())
	db, err := sqldb.Open(ctx, store, "app", changeMigrations, changeSchema)
	if err != nil {
		t.Fatal(err)
	}
	state, err := kv.Open(ctx, store, kv.Options{In: db})
	if err != nil {
		t.Fatal(err)
	}
	values, err := kv.OpenBucket[[]byte](ctx, state, "values")
	if err != nil {
		t.Fatal(err)
	}
	value := bytes.Repeat([]byte("v"), 700<<10)
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	err = db.Batch(bounded, func(b *sqldb.Batch) error {
		b.Add(values.Written(bounded, "one", value))
		b.Add(values.Written(bounded, "two", value))
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if memory := store.Memory(); memory.Used != 0 || memory.Peak > memory.Capacity {
		t.Fatalf("the batch left memory held: %+v", memory)
	}
	for _, key := range []string{"one", "two"} {
		got, found, err := values.Get(ctx, key)
		if err != nil || !found || !bytes.Equal(got, value) {
			t.Fatalf("%s: %v, %v", key, found, err)
		}
	}
}

func TestABatchOfKeysDoesNotWaitForItsOwnWriteSlots(t *testing.T) {
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, db, _, values := openChangeStore(t, t.TempDir(), clock)
	defer store.Close(context.Background())
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	err := db.Batch(ctx, func(b *sqldb.Batch) error {
		for key := range 2049 {
			b.Add(values.Written(ctx, key, "kept"))
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if value, found, err := values.Get(t.Context(), 2048); err != nil || !found || value != "kept" {
		t.Fatalf("the last key of the batch: %q, %v: %v", value, found, err)
	}
}

func TestAChangeRefusesANilKeyAsItsOrdinaryCallDoes(t *testing.T) {
	ctx := t.Context()
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, db, _, values := openChangeStore(t, t.TempDir(), clock)
	defer store.Close(context.Background())
	for _, makeChange := range []func() kv.Change{
		func() kv.Change { return values.Written(ctx, nil, "kept") },
		func() kv.Change { return values.Deleted(ctx, nil) },
	} {
		err := db.Batch(ctx, func(b *sqldb.Batch) error {
			b.Exec("insert into users values (7, 'ann@example.com')")
			b.Add(makeChange())
			return nil
		})
		if !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("a change of nil: %v", err)
		}
	}
	count, err := sqldb.Scalar[int](ctx, db, "select count(*) from users")
	if err != nil || count != 0 {
		t.Fatalf("a refused batch wrote %d users: %v", count, err)
	}
}

func TestADatabaseHoldsOneKVStore(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	clock := &changeClock{now: time.Unix(1_800_000_000, 0)}
	store, db, _, _ := openChangeStore(t, dir, clock)
	defer store.Close(context.WithoutCancel(ctx))

	if _, err := kv.Open(ctx, store, kv.Options{In: db}); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("a second kv store in the database: %v", err)
	}
	guest, err := tinystore.Open(ctx, dir, tinystore.Options{Manual: true, Guest: true})
	if err != nil {
		t.Fatal(err)
	}
	defer guest.Close(context.WithoutCancel(ctx))
	guestDB, err := sqldb.Open(ctx, guest, "app", changeMigrations, changeSchema)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = kv.Open(ctx, guest, kv.Options{In: guestDB}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a guest's kv store in a database: %v", err)
	}
}
