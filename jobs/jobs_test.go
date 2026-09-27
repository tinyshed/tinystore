package jobs

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// testStart is the clock every test store starts at
var testStart = time.Date(2026, 9, 27, 12, 0, 0, 0, time.UTC)

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

// testQueues is a Manual store whose clock the test moves, and jobs opened on it
type testQueues struct {
	*Store
	runtime *tinystore.Store
	clock   *testClock
	dir     string
}

func openTestQueues(t *testing.T, dir string) *testQueues {
	t.Helper()
	return openTestQueuesWith(t, dir, tinystore.Options{})
}

// openTestQueuesWith opens the queues on a store with options, Manual and on
// the test's clock
func openTestQueuesWith(t *testing.T, dir string, options tinystore.Options) *testQueues {
	t.Helper()
	clock := &testClock{now: testStart}
	options.Manual, options.Clock = true, clock.Now
	runtime, err := tinystore.Open(t.Context(), dir, options)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close(context.Background()) })
	queues, err := Open(t.Context(), runtime, Options{})
	if err != nil {
		t.Fatal(err)
	}
	return &testQueues{Store: queues, runtime: runtime, clock: clock, dir: dir}
}

func (s *testQueues) reopen(t *testing.T) *testQueues {
	t.Helper()
	if err := s.runtime.Close(t.Context()); err != nil {
		t.Fatal(err)
	}
	reopened := openTestQueues(t, s.dir)
	reopened.clock.now = s.clock.Now()
	return reopened
}

func openTestQueue[V any](t *testing.T, queues *testQueues, name string, options ...QueueOption) *Queue[V] {
	t.Helper()
	queue, err := OpenQueue[V](t.Context(), queues.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return queue
}

func mustClaim[V any](t *testing.T, queue *Queue[V], options ...ClaimOption) Job[V] {
	t.Helper()
	job, found, err := queue.Claim(t.Context(), options...)
	if err != nil || !found {
		t.Fatalf("no job was claimed: %v", err)
	}
	return job
}

func mustEnqueue[V any](t *testing.T, queue *Queue[V], value V, options ...EnqueueOption) {
	t.Helper()
	if err := queue.Enqueue(t.Context(), value, options...); err != nil {
		t.Fatal(err)
	}
}

func nothingDue[V any](t *testing.T, queue *Queue[V]) {
	t.Helper()
	job, found, err := queue.Claim(t.Context())
	if err != nil || found {
		t.Fatalf("a claim found %+v: %v", job, err)
	}
}

// the jobs of the abrupt exit: every one was enqueued before the exit
const crashJobs = 256

// an Enqueue that returned is in the file after an exit that closes nothing,
// since it returns only once its group's commit is durable
func TestAnEnqueuedJobSurvivesAnAbruptExit(t *testing.T) {
	dir := t.TempDir()
	runChild(t, "^TestAbruptExitHelper$", dir)

	queues := openTestQueues(t, dir)
	queue := openTestQueue[int](t, queues, "crash")
	for n := range crashJobs {
		entry, found, err := queue.Get(t.Context(), strconv.Itoa(n))
		if err != nil || !found || entry.Value != n {
			t.Fatalf("job %d after the exit: %+v, %v, %v", n, entry, found, err)
		}
	}
}

// TestAbruptExitHelper runs in the child: it enqueues from many goroutines at
// once, so that the jobs commit in groups, and exits without closing
func TestAbruptExitHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_JOBS_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	queues := openTestQueues(t, dir)
	queue := openTestQueue[int](t, queues, "crash")
	var wg sync.WaitGroup
	errs := make([]error, crashJobs)
	for n := range crashJobs {
		wg.Go(func() { errs[n] = queue.Enqueue(context.Background(), n, Key(strconv.Itoa(n))) })
	}
	wg.Wait()
	exitOn(errors.Join(errs...))
}

// a job that kills the process that runs it is counted each time, and fails
// for good after its attempts rather than stopping the program for ever
func TestAJobThatKillsItsProcessFailsAfterItsAttempts(t *testing.T) {
	dir := t.TempDir()
	for range 3 {
		runChild(t, "^TestKilledByAJobHelper$", dir)
	}
	queues := openTestQueues(t, dir)
	queue := openTestQueue[string](t, queues, "poison", MaxAttempts(3))
	entry, found, err := queue.Get(t.Context(), "poison")
	if err != nil || !found || entry.State != Waiting || entry.Attempt != 3 {
		t.Fatalf("after three deaths the job is %+v, %v, %v", entry, found, err)
	}
	nothingDue(t, queue)
	entry, _, err = queue.Get(t.Context(), "poison")
	if err != nil || entry.State != Failed || entry.Attempt != 3 || !strings.Contains(entry.Err, "3 attempts") {
		t.Fatalf("past its attempts the job is %+v: %v", entry, err)
	}
}

// TestKilledByAJobHelper runs in the child: it enqueues the poison job once,
// claims it, and dies holding its lease
func TestKilledByAJobHelper(t *testing.T) {
	dir := os.Getenv("TINYSTORE_JOBS_CRASH_DIR")
	if dir == "" {
		t.Skip("subprocess only")
	}
	queues := openTestQueues(t, dir)
	queue := openTestQueue[string](t, queues, "poison", MaxAttempts(3))
	if err := queue.Enqueue(context.Background(), "boom", Key("poison")); err != nil {
		exitOn(err)
	}
	_, found, err := queue.Claim(context.Background())
	if err == nil && !found {
		err = errors.New("the poison job was not due")
	}
	exitOn(err)
}

func runChild(t *testing.T, test, dir string) {
	t.Helper()
	command := exec.CommandContext(t.Context(), os.Args[0], "-test.run="+test)
	command.Env = append(os.Environ(), "TINYSTORE_JOBS_CRASH_DIR="+dir)
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("child failed: %v\n%s", err, output)
	}
}

func exitOn(err error) {
	if err != nil {
		fmt.Println(err)
		os.Exit(1)
	}
	os.Exit(0)
}

// a handler that Close stops gives its job back: it waits again, and the
// attempt it was on is not counted
func TestCloseGivesRunningJobsBackUncounted(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "slow")
	mustEnqueue(t, queue, "work", Key("slow"))

	started := make(chan struct{})
	worked := make(chan error, 1)
	go func() {
		worked <- queue.Work(context.Background(), func(ctx context.Context, _ Job[string]) error {
			close(started)
			<-ctx.Done()
			return ctx.Err()
		})
	}()
	<-started
	reopened := queues.reopen(t)
	if err := <-worked; !errors.Is(err, tinystore.ErrClosed) {
		t.Fatalf("Work ended with %v", err)
	}

	queue = openTestQueue[string](t, reopened, "slow")
	entry, found, err := queue.Get(t.Context(), "slow")
	if err != nil || !found || entry.State != Waiting || entry.Attempt != 0 {
		t.Fatalf("the job given back is %+v, %v, %v", entry, found, err)
	}
	if job := mustClaim(t, queue); job.Attempt != 1 {
		t.Fatalf("the next claim is attempt %d", job.Attempt)
	}
}

// every call that holds a value waits until the store's memory has room for
// it and gives the room back when it is done: an Enqueue while it waits for the
// writer, a Get or a Scan while it reads, a handler while it runs
func TestStoreMemoryBoundsEnqueuesReadsAndHandlers(t *testing.T) {
	const capacity = 8 << 20
	queues := openTestQueuesWith(t, t.TempDir(), tinystore.Options{Memory: capacity})
	queue := openTestQueue[[]byte](t, queues, "uploads")
	upload := bytes.Repeat([]byte("u"), 600_000)
	mustEnqueue(t, queue, upload, Key("first"))
	ran := 0
	work := map[string]func(context.Context) error{
		"enqueue": func(ctx context.Context) error { return queue.Enqueue(ctx, upload, Key("second")) },
		"get": func(ctx context.Context) error {
			_, _, err := queue.Get(ctx, "first")
			return err
		},
		"scan": func(ctx context.Context) error {
			_, err := queue.Scan(ctx, Query{})
			return err
		},
		"work": func(ctx context.Context) error {
			return queue.Work(ctx, func(context.Context, Job[[]byte]) error {
				ran++
				return nil
			}, UntilIdle())
		},
	}
	for _, name := range []string{"enqueue", "get", "scan", "work"} {
		reserved, err := queues.runtime.Reserve(t.Context(), capacity)
		if err != nil {
			t.Fatal(err)
		}
		short, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
		if err = work[name](short); !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("%s while the store's memory is taken: %v", name, err)
		}
		cancel()
		reserved.Release()
		if err = work[name](t.Context()); err != nil {
			t.Fatalf("%s once the memory is free: %v", name, err)
		}
		if usage := queues.runtime.Memory(); usage.Used != 0 {
			t.Fatalf("%s kept %d bytes", name, usage.Used)
		}
	}
	if ran != 2 {
		t.Fatalf("the handler ran %d times, not once an upload", ran)
	}
}

// counted is a job's value that counts how often it is written as JSON
type counted struct {
	N      int
	writes *atomic.Int64
}

func (c counted) MarshalJSON() ([]byte, error) {
	c.writes.Add(1)
	return json.Marshal(c.N)
}

func (c *counted) UnmarshalJSON(raw []byte) error {
	return json.Unmarshal(raw, &c.N)
}

// an Enqueue waiting for the store's memory has written nothing yet: a value
// JSON writes waits for the largest value's room before it is written
func TestAnEnqueueWaitingForMemoryHasWrittenNothing(t *testing.T) {
	queues := openTestQueuesWith(t, t.TempDir(), tinystore.Options{Memory: maxValue})
	queue := openTestQueue[counted](t, queues, "counted")
	taken, err := queues.runtime.Reserve(t.Context(), maxValue)
	if err != nil {
		t.Fatal(err)
	}

	var writes atomic.Int64
	var enqueues sync.WaitGroup
	for n := range 24 {
		enqueues.Go(func() {
			if enqueueErr := queue.Enqueue(t.Context(), counted{N: n, writes: &writes}); enqueueErr != nil {
				t.Error(enqueueErr)
			}
		})
	}
	time.Sleep(100 * time.Millisecond)
	if n := writes.Load(); n != 0 {
		t.Fatalf("%d values were written while no memory was free", n)
	}
	taken.Release()
	enqueues.Wait()
	if n := writes.Load(); n != 24 {
		t.Fatalf("24 enqueues wrote %d values", n)
	}
	if usage := queues.runtime.Memory(); usage.Used != 0 {
		t.Fatalf("the enqueues kept %d bytes", usage.Used)
	}
}

// a call inside Tx waits for none of the store's memory, which the writes
// waiting for its writer hold: it takes what is free, and past that it is
// ErrLimit at once
func TestATransactionTakesOnlyTheMemoryThatIsFree(t *testing.T) {
	const capacity = 4 << 20
	queues := openTestQueuesWith(t, t.TempDir(), tinystore.Options{Memory: capacity})
	queue := openTestQueue[string](t, queues, "mail")
	mustEnqueue(t, queue, "hello", Key("first"))
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	taken, err := queues.runtime.Reserve(ctx, capacity)
	if err != nil {
		t.Fatal(err)
	}

	err = queues.Tx(ctx, func(tx *Tx) error {
		if _, _, getErr := queue.WithTx(tx).Get(ctx, "first"); !errors.Is(getErr, tinystore.ErrLimit) {
			t.Errorf("a Get inside Tx while the memory is taken: %v", getErr)
		}
		if enqueueErr := queue.WithTx(tx).Enqueue(ctx, "second"); !errors.Is(enqueueErr, tinystore.ErrLimit) {
			t.Errorf("an Enqueue inside Tx while the memory is taken: %v", enqueueErr)
		}
		return nil
	})
	if err != nil || ctx.Err() != nil {
		t.Fatalf("the transaction: %v, %v", err, ctx.Err())
	}
	taken.Release()
	err = queues.Tx(ctx, func(tx *Tx) error {
		if _, _, getErr := queue.WithTx(tx).Get(ctx, "first"); getErr != nil {
			return getErr
		}
		return queue.WithTx(tx).Enqueue(ctx, "second")
	})
	if err != nil {
		t.Fatalf("the transaction once the memory is free: %v", err)
	}
	if usage := queues.runtime.Memory(); usage.Used != 0 {
		t.Fatalf("the transactions kept %d bytes", usage.Used)
	}
}
