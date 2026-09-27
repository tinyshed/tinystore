package kv

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// testClock is the store's clock, moved by the test
type testClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *testClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *testClock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.now = c.now.Add(d)
}

var testStart = time.Date(2026, 9, 26, 12, 0, 0, 0, time.UTC)

type testState struct {
	*Store
	runtime *tinystore.Store
	clock   *testClock
	dir     string
}

// openTestState opens a Manual store on dir with a clock the test moves, and
// kv inside it; the test closes it
func openTestState(t testing.TB, dir string) *testState {
	t.Helper()
	return openTestStateWith(t, dir, tinystore.Options{})
}

// openTestStateWith is openTestState with the store's other options
func openTestStateWith(t testing.TB, dir string, options tinystore.Options) *testState {
	t.Helper()
	clock := &testClock{now: testStart}
	options.Manual, options.Clock = true, clock.Now
	runtime, err := tinystore.Open(t.Context(), dir, options)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	state, err := Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	return &testState{Store: state, runtime: runtime, clock: clock, dir: dir}
}

func (s *testState) reopen(t *testing.T) *testState {
	t.Helper()
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened := openTestState(t, s.dir)
	reopened.clock.now = s.clock.Now()
	return reopened
}

func openTestBucket[V any](t testing.TB, state *testState, name string, options ...BucketOption) *Bucket[V] {
	t.Helper()
	bucket, err := OpenBucket[V](t.Context(), state.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return bucket
}

// the keys of the abrupt exit: every one of them was answered before the exit
const crashKeys = 256

// a Set that returned is in the file after an exit that closes nothing, since
// it returns only once its group's commit is durable
func TestAWriteThatReturnedSurvivesAnAbruptExit(t *testing.T) {
	dir := t.TempDir()
	command := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestAbruptExitHelper$")
	command.Env = append(os.Environ(), "TINYSTORE_KV_CRASH_DIR="+dir)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("child failed: %v\n%s", err, output)
	}

	state := openTestState(t, dir)
	bucket := openTestBucket[int](t, state, "crash")
	for n := range crashKeys {
		value, found, err := bucket.Get(t.Context(), n)
		if err != nil || !found || value != n {
			t.Fatalf("key %d after the exit: %d, %v, %v", n, value, found, err)
		}
	}
}

// TestAbruptExitHelper runs in the child: it writes from many goroutines at
// once, so that the writes commit in groups, and exits without closing
func TestAbruptExitHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_KV_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	state := openTestState(t, dir)
	bucket := openTestBucket[int](t, state, "crash")
	var wg sync.WaitGroup
	errs := make([]error, crashKeys)
	for n := range crashKeys {
		wg.Go(func() { errs[n] = bucket.Set(context.Background(), n, n) })
	}
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			fmt.Println(err)
			os.Exit(1)
		}
	}
	os.Exit(0)
}

// every call that reads or makes values waits for the store's memory before
// it does, and gives back all it held
func TestStoreMemoryBoundsWritesReadsAndScans(t *testing.T) {
	budget := int64(pageHeld(scanLimit))
	state := openTestStateWith(t, t.TempDir(), tinystore.Options{Memory: budget})
	values := openTestBucket[string](t, state, "values")
	if err := values.Set(t.Context(), "kept", "value"); err != nil {
		t.Fatal(err)
	}
	calls := map[string]func(context.Context) error{
		"Set": func(ctx context.Context) error { return values.Set(ctx, "set", "value") },
		"SetIfAbsent": func(ctx context.Context) error {
			_, err := values.SetIfAbsent(ctx, "kept", "other")
			return err
		},
		"Get": func(ctx context.Context) error {
			_, _, err := values.Get(ctx, "kept")
			return err
		},
		"Take": func(ctx context.Context) error {
			_, _, err := values.Take(ctx, "set")
			return err
		},
		"Scan": func(ctx context.Context) error {
			_, err := values.Scan(ctx, Query{})
			return err
		},
	}
	for _, name := range []string{"Set", "SetIfAbsent", "Get", "Take", "Scan"} {
		taken, err := state.runtime.Reserve(t.Context(), budget)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = calls[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		taken.Release()
		if err = calls[name](t.Context()); err != nil {
			t.Fatalf("%s once the memory is free: %v", name, err)
		}
		if usage := state.runtime.Memory(); usage.Used != 0 {
			t.Fatalf("%s kept %d bytes", name, usage.Used)
		}
	}
}

// countingCodec counts its encodings, each of them the largest value
type countingCodec struct{ encoded *atomic.Int64 }

func (c countingCodec) Encode(int) ([]byte, error) {
	c.encoded.Add(1)
	return make([]byte, maxValue), nil
}

func (countingCodec) Decode([]byte) (int, error) { return 0, nil }

// a write waiting for the store's memory has made nothing yet: a value only
// its codec measures waits for the largest value's room before it is encoded
func TestAWriteWaitingForMemoryHasEncodedNothing(t *testing.T) {
	state := openTestStateWith(t, t.TempDir(), tinystore.Options{Memory: maxValue})
	var encoded atomic.Int64
	values := openTestBucket[int](t, state, "values", WithCodec[int](countingCodec{encoded: &encoded}))
	taken, err := state.runtime.Reserve(t.Context(), maxValue)
	if err != nil {
		t.Fatal(err)
	}

	var writes sync.WaitGroup
	for n := range 24 {
		writes.Go(func() {
			if setErr := values.Set(t.Context(), n, n); setErr != nil {
				t.Error(setErr)
			}
		})
	}
	time.Sleep(100 * time.Millisecond)
	if n := encoded.Load(); n != 0 {
		t.Fatalf("%d values were encoded while no memory was free", n)
	}
	taken.Release()
	writes.Wait()
	if n := encoded.Load(); n != 24 {
		t.Fatalf("24 writes encoded %d values", n)
	}
	if usage := state.runtime.Memory(); usage.Used != 0 {
		t.Fatalf("the writes kept %d bytes", usage.Used)
	}
}

func TestACallAfterCloseIsClosed(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[string](t, state, "closing")
	if err := state.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := bucket.Set(t.Context(), "a", "b"); !isClosed(err) {
		t.Fatalf("a Set after Close: %v", err)
	}
	if _, _, err := bucket.Get(t.Context(), "a"); !isClosed(err) {
		t.Fatalf("a Get after Close: %v", err)
	}
}

func isClosed(err error) bool {
	return errors.Is(err, tinystore.ErrClosed)
}
