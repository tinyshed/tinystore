package tinystoretest_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/kv"
	"tinyshed.org/tinystore/tinystoretest"
)

func TestATestsStoreRunsInThisProcessOnItsClock(t *testing.T) {
	if ways.Library() == "" {
		t.Skip("the core's library is not built")
	}
	ctx := context.Background()
	// no Library nor Embedded: Open opens in this process unless asked otherwise, the library TINYSTORE_LIBRARY names
	store := tinystoretest.Open(t, tinystore.Clock(time.Date(2026, 11, 19, 9, 0, 0, 0, time.UTC)))
	codes := ways.Must(kv.OpenBucket[int](store, "codes"))
	ways.Must(0, codes.Set(ctx, "K7Q2", 42, kv.TTL(15*time.Minute)))
	ways.Must(0, store.Clock().Advance(ctx, time.Hour))
	if _, found := ways.Must2(codes.Get(ctx, "K7Q2")); found {
		t.Fatal("the key expired on the store's clock, in this process")
	}
}

func TestAClockIsAPrivateOrAnEmbeddedStoresAndNeverBoth(t *testing.T) {
	ctx := context.Background()
	_, err := tinystore.Open(ctx, t.TempDir(), tinystore.Clock(time.Now()))
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a sidecar's clock: %v", err)
	}
	_, err = tinystore.Open(ctx, t.TempDir(), tinystore.Private(), tinystore.Embedded())
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("private and embedded: %v", err)
	}
}

func TestATestsStoreAskedToBePrivateRunsInAChild(t *testing.T) {
	if ways.Binary() == "" {
		t.Skip("no tinystore binary")
	}
	start := time.Date(2026, 11, 19, 9, 0, 0, 0, time.UTC)
	store := tinystoretest.Open(t, tinystore.Private(), tinystore.Binary(ways.Binary()), tinystore.Clock(start))
	if now := ways.Must(store.Clock().Now(context.Background())); !now.Equal(start) {
		t.Fatalf("now %v", now)
	}
}
