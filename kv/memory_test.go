package kv

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"math"
	"os"
	"os/exec"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// rowsOf counts the rows a bucket holds in the file, expired or not
func (s *testState) rowsOf(t *testing.T, bucket string) int {
	t.Helper()
	var count int
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select count(*) from _tinystore_kv_cells as c join _tinystore_kv_buckets as b on b.id = c.bucket
			where b.name = ?1`, bucket).Scan(&count)
	})
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// a change is read from memory before a flush writes it, and from the file
// after the flush has let it go; a Delete waits for the flush as a change does
func TestAChangeInMemoryIsReadBeforeTheFlushWritesIt(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	ctx := t.Context()
	for range 3 {
		if _, err := attempts.Add(ctx, "ip", 1); err != nil {
			t.Fatal(err)
		}
	}
	if held, err := attempts.Get(ctx, "ip"); held != 3 || err != nil || state.rowsOf(t, "attempts") != 0 {
		t.Fatalf("before a flush: %d, %v, %d rows; want 3 in memory alone", held, err, state.rowsOf(t, "attempts"))
	}

	done, err := state.Maintain(ctx)
	if err != nil || done.Flushed != 1 || state.rowsOf(t, "attempts") != 1 {
		t.Fatalf("Maintain flushed %d, %v", done.Flushed, err)
	}
	if held, getErr := attempts.Get(ctx, "ip"); held != 3 || getErr != nil {
		t.Fatalf("after the flush the counter reads %d, %v", held, getErr)
	}

	if err = attempts.Delete(ctx, "ip"); err != nil {
		t.Fatal(err)
	}
	if held, _ := attempts.Get(ctx, "ip"); held != 0 || state.rowsOf(t, "attempts") != 1 {
		t.Fatalf("a Delete before its flush reads %d with %d rows", held, state.rowsOf(t, "attempts"))
	}
	if _, err = state.Maintain(ctx); err != nil || state.rowsOf(t, "attempts") != 0 {
		t.Fatalf("the flushed Delete left %d rows, %v", state.rowsOf(t, "attempts"), err)
	}
}

// what LoseAtMost counters hold in memory keeps the rules the file keeps: a
// window that starts at the first Add and a sum refused past int64
func TestACounterInMemoryKeepsTheFilesRules(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", DefaultTTL(15*time.Minute), LoseAtMost(time.Hour))
	ctx := t.Context()
	if _, err := attempts.Add(ctx, "ip", 1); err != nil {
		t.Fatal(err)
	}
	state.clock.advance(10 * time.Minute)
	if n, _ := attempts.Add(ctx, "ip", 1); n != 2 {
		t.Fatalf("a second Add inside the window gave %d", n)
	}
	state.clock.advance(6 * time.Minute)
	if n, err := attempts.Add(ctx, "ip", 1); n != 1 || err != nil {
		t.Fatalf("an Add after the window gave %d, %v; want 1", n, err)
	}

	if _, err := attempts.Add(ctx, "big", math.MaxInt64); err != nil {
		t.Fatal(err)
	}
	if _, err := attempts.Add(ctx, "big", 1); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a sum past int64 in memory: %v", err)
	}
	if held, _ := attempts.Max(ctx, "big", 5); held != math.MaxInt64 {
		t.Fatalf("Max after a refused sum holds %d", held)
	}
}

// counters of one name keep their numbers one way: handles opened alike share
// one memory, and another LoseAtMost, or none, is refused
func TestCountersOpenAgainOnlyAsTheyWereOpened(t *testing.T) {
	state := openTestState(t, t.TempDir())
	first := openTestCounters(t, state, "attempts", LoseAtMost(time.Second))
	second := openTestCounters(t, state, "attempts", LoseAtMost(time.Second), DefaultTTL(time.Minute))
	if _, err := first.Add(t.Context(), "ip", 2); err != nil {
		t.Fatal(err)
	}
	if held, err := second.Get(t.Context(), "ip"); held != 2 || err != nil {
		t.Fatalf("a second handle read %d, %v before the flush; want 2", held, err)
	}
	for _, other := range [][]CounterOption{nil, {LoseAtMost(5 * time.Second)}} {
		if _, err := OpenCounters(t.Context(), state.Store, "attempts", other...); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("counters opened again another way: %v", err)
		}
	}
	openTestCounters(t, state, "durable")
	if _, err := OpenCounters(t.Context(), state.Store, "durable", LoseAtMost(time.Second)); !errors.Is(err,
		tinystore.ErrInvalid) {
		t.Fatalf("durable counters opened again with LoseAtMost: %v", err)
	}
}

// LoseAtMost counters live in memory, so a transaction, which promises its
// writes are in the file when it commits, refuses them
func TestALoseAtMostCounterRefusesATransaction(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Second))
	err := state.Tx(t.Context(), func(tx *Tx) error {
		_, addErr := attempts.WithTx(tx).Add(t.Context(), "ip", 1)
		return addErr
	})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a LoseAtMost Add inside a transaction: %v", err)
	}
}

func countersHeld(held *memory) int {
	count := 0
	for i := range held.shards {
		held.shards[i].mu.Lock()
		count += len(held.shards[i].counters)
		held.shards[i].mu.Unlock()
	}
	return count
}

// however many keys arrive, the counters memory holds stay within the bound:
// the change that finds no room flushes first
func TestCountersInMemoryStayWithinTheirBound(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	attempts.memory.bound = 10
	for n := range 95 {
		if _, err := attempts.Add(t.Context(), n, 1); err != nil {
			t.Fatal(err)
		}
		if held := countersHeld(attempts.memory); held > 10 {
			t.Fatalf("memory holds %d counters after %d Adds", held, n+1)
		}
	}
	if rows := state.rowsOf(t, "attempts"); rows != 90 {
		t.Fatalf("the flushes wrote %d counters; want 90", rows)
	}
}

// a change that fails leaves the counter it read held and unchanged, and
// such counters count against the bound as changed ones do
func TestFailedChangesStayWithinTheBound(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	for n := range 30 {
		if _, err := attempts.Add(t.Context(), n, math.MaxInt64); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	attempts.memory.bound = 10
	for n := range 30 {
		if _, err := attempts.Add(t.Context(), n, 1); !errors.Is(err, tinystore.ErrLimit) {
			t.Fatalf("an Add past the int64 range: %v", err)
		}
		if held := countersHeld(attempts.memory); held > 10 {
			t.Fatalf("memory holds %d counters after %d refused Adds", held, n+1)
		}
	}
}

// callers arriving together with keys memory does not hold take their places
// one at a time, so that none passes the bound between a look and an add
func TestColdCountersArrivingTogetherStayWithinTheBound(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	attempts.memory.bound = 10
	var adders sync.WaitGroup
	for worker := range 64 {
		adders.Go(func() {
			for n := range 20 {
				if _, err := attempts.Add(t.Context(), worker*100+n, 1); err != nil {
					t.Error(err)
					return
				}
				if held := attempts.memory.held.Load(); held > 10 {
					t.Errorf("memory took places for %d counters", held)
					return
				}
			}
		})
	}
	adders.Wait()
	if held := countersHeld(attempts.memory); held > 10 {
		t.Fatalf("memory holds %d counters", held)
	}
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	if rows := state.rowsOf(t, "attempts"); rows != 64*20 {
		t.Fatalf("the flushes wrote %d counters; want %d", rows, 64*20)
	}
}

// While the file refuses what a flush writes, a counter memory does not hold is
// refused with the flush's error rather than held past the bound. The ones it
// holds still change, and nothing is lost once the file accepts again.
func TestAFailedFlushRefusesNewCountersRatherThanHoldThem(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	attempts.memory.bound = 10
	for n := range 10 {
		if _, err := attempts.Add(t.Context(), n, 1); err != nil {
			t.Fatal(err)
		}
	}
	state.exec(t, `create trigger refuse_flush before insert on _tinystore_kv_cells begin select raise(abort, 'a refused flush'); end`)
	if _, err := attempts.Add(t.Context(), "new", 1); err == nil {
		t.Fatal("a new counter was held while the flush that makes room failed")
	}
	if n, err := attempts.Add(t.Context(), 3, 1); n != 2 || err != nil {
		t.Fatalf("a counter memory holds, while the file refuses: %d, %v", n, err)
	}
	if held := countersHeld(attempts.memory); held > 10 {
		t.Fatalf("memory holds %d counters", held)
	}
	state.exec(t, `drop trigger refuse_flush`)

	if n, err := attempts.Add(t.Context(), "new", 1); n != 1 || err != nil {
		t.Fatalf("a new counter once the file accepts: %d, %v", n, err)
	}
	if _, err := state.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened := state.reopen(t)
	attempts = openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour))
	for key, want := range map[any]int64{0: 1, 3: 2, 9: 1, "new": 1} {
		if n, err := attempts.Get(t.Context(), key); n != want || err != nil {
			t.Fatalf("counter %v holds %d, %v; want %d", key, n, err, want)
		}
	}
}

// changes from many goroutines each count once while flushes write and let go
// of what they hold beside them
func TestChangesCountOnceWhileFlushesRun(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	ctx := t.Context()
	stop := make(chan struct{})
	var flusher sync.WaitGroup
	flusher.Go(func() {
		for {
			select {
			case <-stop:
				return
			default:
			}
			if _, err := state.Maintain(ctx); err != nil {
				t.Error(err)
				return
			}
		}
	})
	var adders sync.WaitGroup
	for worker := range 16 {
		adders.Go(func() {
			for n := range 200 {
				if _, err := attempts.Add(ctx, (worker+n)%7, 1); err != nil {
					t.Error(err)
					return
				}
			}
		})
	}
	adders.Wait()
	close(stop)
	flusher.Wait()
	total := int64(0)
	for key := range 7 {
		held, err := attempts.Get(ctx, key)
		if err != nil {
			t.Fatal(err)
		}
		total += held
	}
	if total != 16*200 {
		t.Fatalf("%d Adds counted %d", 16*200, total)
	}
}

// BenchmarkChangesInMemory adds to LoseAtMost counters from every processor at
// once, on a Manual store that writes nothing while it runs.
//
// It adds to 1024 keys memory holds already, and to keys it never held, each of
// which reads the file once. It opens the store itself, so that it runs against
// an earlier kv as it is.
func BenchmarkChangesInMemory(b *testing.B) {
	b.Run("held", func(b *testing.B) {
		attempts := benchmarkCounters(b)
		var keys [1024]string
		for i := range keys {
			keys[i] = strconv.Itoa(i)
			if _, err := attempts.Add(b.Context(), keys[i], 1); err != nil {
				b.Fatal(err)
			}
		}
		var workers atomic.Int64
		b.ResetTimer()
		b.RunParallel(func(pb *testing.PB) {
			i := int(workers.Add(1)) * 31
			for pb.Next() {
				i++
				if _, err := attempts.Add(b.Context(), keys[i%len(keys)], 1); err != nil {
					b.Error(err)
					return
				}
			}
		})
	})
	b.Run("new", func(b *testing.B) {
		attempts := benchmarkCounters(b)
		var next atomic.Int64
		b.ResetTimer()
		b.RunParallel(func(pb *testing.PB) {
			for pb.Next() {
				if _, err := attempts.Add(b.Context(), next.Add(1), 1); err != nil {
					b.Error(err)
					return
				}
			}
		})
	})
}

// benchmarkCounters opens LoseAtMost counters of an hour on a Manual store
func benchmarkCounters(b *testing.B) *Counters {
	b.Helper()
	attempts, err := OpenCounters(b.Context(), openBenchmarkStore(b), "attempts", LoseAtMost(time.Hour))
	if err != nil {
		b.Fatal(err)
	}
	b.ReportAllocs()
	return attempts.Of("ip")
}

// openBenchmarkStore opens kv on a Manual store of its own; it uses nothing a
// test helper gives, so that a benchmark runs against an earlier kv as it is
func openBenchmarkStore(b *testing.B) *Store {
	b.Helper()
	runtime, err := tinystore.Open(b.Context(), b.TempDir(), tinystore.Options{Manual: true})
	if err != nil {
		b.Fatal(err)
	}
	b.Cleanup(func() { _ = runtime.Close(context.Background()) })
	state, err := Open(b.Context(), runtime, Options{})
	if err != nil {
		b.Fatal(err)
	}
	return state
}

// Close writes what memory holds, so a store closed as it should be loses
// nothing
func TestClosingWritesWhatMemoryHolds(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	if _, err := attempts.Add(t.Context(), "ip", 4); err != nil {
		t.Fatal(err)
	}
	reopened := state.reopen(t)
	attempts = openTestCounters(t, reopened, "attempts", LoseAtMost(time.Hour))
	if held, err := attempts.Get(t.Context(), "ip"); held != 4 || err != nil {
		t.Fatalf("after Close and reopen the counter holds %d, %v", held, err)
	}
}

// the interval the child flushes on, and the Adds before and after it waits
// through several of them
const (
	relaxedInterval = 50 * time.Millisecond
	relaxedFirst    = 100
	relaxedLater    = 50
)

// an exit that closes nothing loses at most the changes of the last interval:
// every Add older than a flush is in the file
func TestLoseAtMostLosesNoMoreThanItsInterval(t *testing.T) {
	dir := t.TempDir()
	command := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestLoseAtMostExitHelper$")
	command.Env = append(os.Environ(), "TINYSTORE_KV_RELAXED_DIR="+dir)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("child failed: %v\n%s", err, output)
	}

	state := openTestState(t, dir)
	attempts := openTestCounters(t, state, "relaxed", LoseAtMost(relaxedInterval))
	held, err := attempts.Get(t.Context(), "ip")
	if err != nil || held < relaxedFirst || held > relaxedFirst+relaxedLater {
		t.Fatalf("after the exit the counter holds %d, %v; want %d to %d", held, err,
			relaxedFirst, relaxedFirst+relaxedLater)
	}
}

// TestLoseAtMostExitHelper runs in the child. Its store flushes in the
// background: it adds, waits through several flushes, adds again and exits
// without closing.
func TestLoseAtMostExitHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_KV_RELAXED_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	ctx := context.Background()
	runtime, err := tinystore.Open(ctx, dir, tinystore.Options{})
	if err != nil {
		t.Fatal(err)
	}
	state, err := Open(ctx, runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	attempts, err := OpenCounters(ctx, state, "relaxed", LoseAtMost(relaxedInterval))
	if err != nil {
		t.Fatal(err)
	}
	add := func(times int) {
		for range times {
			if _, addErr := attempts.Add(ctx, "ip", 1); addErr != nil {
				fmt.Println(addErr)
				os.Exit(1)
			}
		}
	}
	add(relaxedFirst)
	time.Sleep(20 * relaxedInterval)
	add(relaxedLater)
	os.Exit(0)
}
