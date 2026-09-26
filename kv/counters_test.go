package kv

import (
	"errors"
	"math"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func openTestCounters(t *testing.T, state *testState, name string, options ...CounterOption) *Counters {
	t.Helper()
	counters, err := OpenCounters(t.Context(), state.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return counters
}

// a sum past the int64 range is refused and leaves the counter as it was,
// since SQLite would quietly turn it into a REAL
func TestAnOverflowingCounterIsRefusedRatherThanRounded(t *testing.T) {
	state := openTestState(t, t.TempDir())
	counters := openTestCounters(t, state, "big")
	for _, edge := range []struct {
		key         string
		start, then int64
	}{
		{"up", math.MaxInt64 - 1, 2},
		{"down", math.MinInt64, -1},
	} {
		if _, err := counters.Add(t.Context(), edge.key, edge.start); err != nil {
			t.Fatal(err)
		}
		if _, err := counters.Add(t.Context(), edge.key, edge.then); !errors.Is(err, tinystore.ErrLimit) {
			t.Fatalf("%s: adding %d to %d gave %v, not ErrLimit", edge.key, edge.then, edge.start, err)
		}
		if held, err := counters.Get(t.Context(), edge.key); held != edge.start || err != nil {
			t.Fatalf("%s: the refused sum left %d, %v; want %d", edge.key, held, err, edge.start)
		}
	}
}

// an attempt counter's window starts at its first Add and does not slide with
// the Adds after it; once it expires the count starts again from zero
func TestACountersWindowStartsAtItsFirstAddAndDoesNotSlide(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", DefaultTTL(15*time.Minute)).Of("ip")
	ctx := t.Context()
	for want := int64(1); want <= 3; want++ {
		if n, err := attempts.Add(ctx, "10.0.0.1", 1); n != want || err != nil {
			t.Fatalf("Add gave %d, %v; want %d", n, err, want)
		}
		state.clock.advance(5 * time.Minute)
	}
	if n, err := attempts.Get(ctx, "10.0.0.1"); n != 0 || err != nil {
		t.Fatalf("fifteen minutes after the first Add the counter holds %d, %v", n, err)
	}
	if n, err := attempts.Add(ctx, "10.0.0.1", 1); n != 1 || err != nil {
		t.Fatalf("an expired counter restarted at %d, %v", n, err)
	}
	if n, err := attempts.Get(ctx, "10.0.0.2"); n != 0 || err != nil {
		t.Fatalf("an absent counter holds %d, %v", n, err)
	}
}

// Max keeps the larger, an absent counter counting as zero, and Delete
// leaves an absent counter
func TestMaxKeepsTheLargerAndDeleteForgets(t *testing.T) {
	state := openTestState(t, t.TempDir())
	marks := openTestCounters(t, state, "marks")
	ctx := t.Context()
	for _, step := range []struct{ n, want int64 }{{-3, 0}, {5, 5}, {3, 5}, {9, 9}} {
		if held, err := marks.Max(ctx, "k", step.n); held != step.want || err != nil {
			t.Fatalf("Max(%d) gave %d, %v; want %d", step.n, held, err, step.want)
		}
	}
	if err := marks.Delete(ctx, "k"); err != nil {
		t.Fatal(err)
	}
	if held, err := marks.Get(ctx, "k"); held != 0 || err != nil {
		t.Fatalf("a deleted counter holds %d, %v", held, err)
	}
}

// Adds from many goroutines each count once
func TestConcurrentAddsEachCountOnce(t *testing.T) {
	state := openTestState(t, t.TempDir())
	counters := openTestCounters(t, state, "hits")
	var wg sync.WaitGroup
	for range 32 {
		wg.Go(func() {
			for range 20 {
				if _, err := counters.Add(t.Context(), "page", 1); err != nil {
					t.Error(err)
					return
				}
			}
		})
	}
	wg.Wait()
	if held, err := counters.Get(t.Context(), "page"); held != 640 || err != nil {
		t.Fatalf("640 Adds counted %d, %v", held, err)
	}
}

// a counter inside a transaction is written with it or not at all
func TestACounterInATransactionRollsBackWithIt(t *testing.T) {
	state := openTestState(t, t.TempDir())
	counters := openTestCounters(t, state, "hits")
	refused := errors.New("refused")
	err := state.Tx(t.Context(), func(tx *Tx) error {
		if n, addErr := counters.WithTx(tx).Add(t.Context(), "k", 5); n != 5 || addErr != nil {
			t.Fatalf("Add in a transaction gave %d, %v", n, addErr)
		}
		return refused
	})
	if !errors.Is(err, refused) {
		t.Fatal(err)
	}
	if held, err := counters.Get(t.Context(), "k"); held != 0 || err != nil {
		t.Fatalf("a rolled back Add left %d, %v", held, err)
	}
}
