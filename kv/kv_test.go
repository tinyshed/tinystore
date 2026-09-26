package kv

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sync"
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
func openTestState(t *testing.T, dir string) *testState {
	t.Helper()
	clock := &testClock{now: testStart}
	runtime, err := tinystore.Open(t.Context(), dir, tinystore.Options{Manual: true, Clock: clock.Now})
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

func openTestBucket[V any](t *testing.T, state *testState, name string, options ...BucketOption) *Bucket[V] {
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
