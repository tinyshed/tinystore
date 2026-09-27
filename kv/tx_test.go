package kv

import (
	"bytes"
	"context"
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// a sign-in link: the code is taken and the session written in one
// transaction, or neither happens
func TestATransactionWritesEveryBucketOrNone(t *testing.T) {
	state := openTestState(t, t.TempDir())
	codes := openTestBucket[int64](t, state, "codes", DefaultTTL(15*time.Minute))
	sessions := openTestBucket[string](t, state, "sessions")
	if err := codes.Set(t.Context(), "digest", 42); err != nil {
		t.Fatal(err)
	}

	refused := errors.New("refused")
	err := state.Tx(t.Context(), func(tx *Tx) error {
		userID, found, err := codes.WithTx(tx).Take(t.Context(), "digest")
		if err != nil || !found {
			return errors.Join(err, errors.New("no code"))
		}
		if err = sessions.WithTx(tx).Of(userID).Set(t.Context(), "token", "phone"); err != nil {
			return err
		}
		return refused
	})
	if !errors.Is(err, refused) {
		t.Fatalf("the transaction: %v", err)
	}
	if _, found, _ := codes.Get(t.Context(), "digest"); !found {
		t.Fatal("a rolled back transaction took the code")
	}

	var inside *Tx
	err = state.Tx(t.Context(), func(tx *Tx) error {
		inside = tx
		userID, _, takeErr := codes.WithTx(tx).Take(t.Context(), "digest")
		if takeErr != nil {
			return takeErr
		}
		return sessions.WithTx(tx).Of(userID).Set(t.Context(), "token", "phone")
	})
	if err != nil {
		t.Fatal(err)
	}
	if value, found, _ := sessions.Of(42).Get(t.Context(), "token"); !found || value != "phone" {
		t.Fatal("the committed transaction wrote no session")
	}
	if err = sessions.WithTx(inside).Set(t.Context(), "late", "x"); !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("a transaction used after it ended: %v", err)
	}
}

// a call inside Tx waits for none of the store's memory, which the writes
// waiting for its writer hold: it takes what is free and is ErrLimit at once
// past it. A call inside View holds a reader the reads holding memory wait
// for, and waits for none either.
func TestTxAndViewWaitForNoMemoryTheCallsWaitingForThemHold(t *testing.T) {
	budget := int64(pageHeld(3))
	state := openTestStateWith(t, t.TempDir(), tinystore.Options{Memory: budget})
	values := openTestBucket[[]byte](t, state, "values")
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	written := make(chan error, 1)
	err := state.Tx(ctx, func(tx *Tx) error {
		go func() { written <- values.Set(ctx, "waiting", bytes.Repeat([]byte{1}, maxValue)) }()
		for state.runtime.Memory().Used < int64(maxValue) {
			time.Sleep(time.Millisecond)
		}
		if _, err := values.WithTx(tx).Scan(ctx, Query{Limit: 1}); err != nil {
			t.Errorf("a page that fits what is free: %v", err)
		}
		if _, err := values.WithTx(tx).Scan(ctx, Query{Limit: 3}); !errors.Is(err, tinystore.ErrLimit) {
			t.Errorf("a page of the whole budget while a write holds some: %v", err)
		}
		return nil
	})
	if err != nil || ctx.Err() != nil {
		t.Fatalf("the transaction: %v, %v", err, ctx.Err())
	}
	if err = <-written; err != nil {
		t.Fatalf("the write waiting for the transaction: %v", err)
	}

	taken, err := state.runtime.Reserve(ctx, budget)
	if err != nil {
		t.Fatal(err)
	}
	err = state.View(ctx, func(tx *Tx) error {
		_, scanErr := values.WithTx(tx).Scan(ctx, Query{Limit: 1})
		return scanErr
	})
	if !errors.Is(err, tinystore.ErrLimit) || ctx.Err() != nil {
		t.Fatalf("a page inside View while the memory is taken: %v", err)
	}
	taken.Release()
	if usage := state.runtime.Memory(); usage.Used != 0 {
		t.Fatalf("%d bytes are held after every call returned", usage.Used)
	}
}

// a View reads one snapshot and refuses to write
func TestAViewReadsOneSnapshotAndRefusesWrites(t *testing.T) {
	state := openTestState(t, t.TempDir())
	flags := openTestBucket[bool](t, state, "flags")
	if err := flags.Set(t.Context(), "new-editor", true); err != nil {
		t.Fatal(err)
	}
	err := state.View(t.Context(), func(tx *Tx) error {
		for read := range 2 {
			on, _, err := flags.WithTx(tx).Get(t.Context(), "new-editor")
			if err != nil || !on {
				return errors.Join(err, errors.New("the view saw a write made after its first read"))
			}
			if read == 0 {
				if err = flags.Set(t.Context(), "new-editor", false); err != nil {
					return err
				}
			}
		}
		return flags.WithTx(tx).Set(t.Context(), "new-editor", true)
	})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a write inside a View: %v", err)
	}
}
