package jobs

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// Get says where a job is: how many jobs run before a waiting one, what the
// handler of a running one reported, why a failed one failed, and that a done
// one was done while KeepDone keeps its key
func TestGetSaysWhereAJobIsAndHowManyRunBeforeIt(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "videos", KeepDone(time.Hour), MaxAttempts(1))
	for _, key := range []string{"a", "b", "c", "d"} {
		mustEnqueue(t, queue, "video "+key, Key(key))
	}
	running := mustClaim(t, queue)
	running.Progress(map[string]int{"done": 4, "total": 10})
	expectEntry(t, queue, "a", Running, 0, `{"done":4,"total":10}`)
	expectEntry(t, queue, "c", Waiting, 1, "")
	expectEntry(t, queue, "d", Waiting, 2, "")

	if err := running.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	expectEntry(t, queue, "a", Done, 0, "")
	if err := mustClaim(t, queue).Fail(t.Context(), errors.New("a broken file")); err != nil {
		t.Fatal(err)
	}
	if entry := expectEntry(t, queue, "b", Failed, 0, ""); entry.Err != "a broken file" {
		t.Fatalf("a failed job's entry: %+v", entry)
	}
	expectEntry(t, queue, "d", Waiting, 1, "")
	if _, found, err := queue.Get(t.Context(), "nothing"); err != nil || found {
		t.Fatalf("a key that names nothing: %v, %v", found, err)
	}
}

// a job whose lease ended unsettled waits again before the jobs behind it, and
// once a claim fails it for good past its attempts, memory lets go of it
func TestAJobWhoseLeaseEndedWaitsAgain(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "videos", MaxAttempts(1))
	for _, key := range []string{"a", "b", "c"} {
		mustEnqueue(t, queue, "video "+key, Key(key))
	}
	mustClaim(t, queue, Lease(time.Minute))
	expectEntry(t, queue, "c", Waiting, 1, "")
	queues.clock.advance(time.Minute)
	expectEntry(t, queue, "a", Waiting, 0, "")
	expectEntry(t, queue, "c", Waiting, 2, "")

	if job := mustClaim(t, queue); job.Key != "b" {
		t.Fatalf("claimed %s, where a fails for good and b runs", job.Key)
	}
	expectEntry(t, queue, "a", Failed, 0, "")
	queue.state.watch.mu.Lock()
	held := len(queue.state.watch.held)
	queue.state.watch.mu.Unlock()
	if held != 1 {
		t.Fatalf("memory holds %d jobs, where only b runs", held)
	}
}

// a watcher sees its job move up the queue, run, report and end, and its
// watch ends with the job
func TestAWatchFollowsItsJobToItsEnd(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "videos")
	for _, key := range []string{"a", "b", "c"} {
		mustEnqueue(t, queue, "video "+key, Key(key))
	}
	seen := watchInto(t, queue, "c")
	expectSeen(t, seen, Waiting, 2, "")

	first := mustClaim(t, queue)
	expectSeen(t, seen, Waiting, 1, "")
	if err := first.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	expectNothingSeen(t, seen) // a job that ran before it left: c is where it was
	second := mustClaim(t, queue)
	expectSeen(t, seen, Waiting, 0, "")
	if err := second.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	third := mustClaim(t, queue)
	expectSeen(t, seen, Running, 0, "")
	third.Progress(0.5)
	expectSeen(t, seen, Running, 0, "0.5")
	if err := third.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if entry := expectSeen(t, seen, Done, 0, ""); entry.Value != "video c" || entry.Attempt != 1 {
		t.Fatalf("the last entry: %+v", entry)
	}
	expectWatchEnded(t, seen)

	quiet := watchInto(t, queue, "nothing")
	expectWatchEnded(t, quiet)
}

// Cancel takes a job that runs: its handler's context ends with ErrCancelled,
// what the handler returns settles nothing, the job never runs again, and its
// watcher sees it cancelled
func TestCancelStopsTheHandlerOfARunningJob(t *testing.T) {
	var log bytes.Buffer
	queues := openTestQueuesWith(t, t.TempDir(), tinystore.Options{Logger: slog.New(slog.NewTextHandler(&log, nil))})
	queue := openTestQueue[string](t, queues, "videos")
	mustEnqueue(t, queue, "video", Key("v1"))
	seen := watchInto(t, queue, "v1")
	expectSeen(t, seen, Waiting, 0, "")

	causes := make(chan error, 1)
	stopped := working(t, queue, func(ctx context.Context, _ Job[string]) error {
		<-ctx.Done()
		causes <- context.Cause(ctx)
		return ctx.Err()
	})
	expectSeen(t, seen, Running, 0, "")
	if cancelled, err := queue.Cancel(t.Context(), "v1"); err != nil || !cancelled {
		t.Fatalf("a running job's Cancel: %v, %v", cancelled, err)
	}
	select {
	case cause := <-causes:
		if !errors.Is(cause, ErrCancelled) {
			t.Fatalf("the handler's context ended with %v", cause)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the handler's context did not end in five seconds")
	}
	expectSeen(t, seen, Cancelled, 0, "")
	expectWatchEnded(t, seen)
	stopped()

	queues.clock.advance(time.Hour)
	nothingDue(t, queue)
	if strings.Contains(log.String(), "lost the leases") {
		t.Fatalf("a cancelled job's lease was logged as lost:\n%s", log.String())
	}
}

// a job a Work loop keeps for its busy worker waits, next, rather than runs,
// and a Cancel of it keeps it from starting
func TestAJobHeldForABusyWorkerWaitsAndCancelKeepsItFromStarting(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "videos")
	for _, key := range []string{"a", "b", "c"} {
		mustEnqueue(t, queue, "video "+key, Key(key))
	}
	ran := make(chan string, 3)
	release := make(chan struct{})
	stopped := working(t, queue, func(ctx context.Context, job Job[string]) error {
		ran <- job.Key
		select {
		case <-release:
		case <-ctx.Done():
		}
		return nil
	})
	if key := <-ran; key != "a" {
		t.Fatalf("ran %s first", key)
	}
	expectEntry(t, queue, "b", Waiting, 0, "")
	expectEntry(t, queue, "c", Waiting, 1, "")
	if cancelled, err := queue.Cancel(t.Context(), "b"); err != nil || !cancelled {
		t.Fatalf("a held job's Cancel: %v, %v", cancelled, err)
	}
	close(release)
	if key := <-ran; key != "c" {
		t.Fatalf("ran %s after a, not c", key)
	}
	stopped()
}

// MaxRunning holds a queue to its places across Work loops and claims, and a
// settlement that gives a place back wakes the loops waiting for one
func TestMaxRunningHoldsAQueueToItsPlaces(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "encode", MaxRunning(2))
	for n := range 6 {
		mustEnqueue(t, queue, n)
	}
	var running, most atomic.Int32
	started := make(chan int, 6)
	release := make(chan struct{})
	handle := func(ctx context.Context, job Job[int]) error {
		now := running.Add(1)
		for seen := most.Load(); now > seen && !most.CompareAndSwap(seen, now); seen = most.Load() {
		}
		started <- job.Value
		select {
		case <-release:
		case <-ctx.Done():
		}
		running.Add(-1)
		return nil
	}
	stopFirst := working(t, queue, handle, Workers(3))
	stopSecond := working(t, queue, handle, Workers(3))
	for range 2 {
		<-started
	}
	select {
	case n := <-started:
		t.Fatalf("job %d started past MaxRunning(2)", n)
	case <-time.After(300 * time.Millisecond):
	}
	nothingDue(t, queue)

	close(release)
	for range 4 {
		select {
		case <-started:
		case <-time.After(10 * time.Second):
			t.Fatal("a settlement that gave a place back woke no loop")
		}
	}
	stopFirst()
	stopSecond()
	if most.Load() != 2 {
		t.Fatalf("at most %d ran at once, not 2", most.Load())
	}
}

// a progress JSON cannot write, or past 4 KiB, is dropped and logged once,
// and the last one kept stays
func TestAProgressPastItsBoundIsDropped(t *testing.T) {
	var log bytes.Buffer
	queues := openTestQueuesWith(t, t.TempDir(), tinystore.Options{Logger: slog.New(slog.NewTextHandler(&log, nil))})
	queue := openTestQueue[string](t, queues, "videos")
	mustEnqueue(t, queue, "video", Key("v1"))
	job := mustClaim(t, queue)
	job.Progress("a third")
	job.Progress(strings.Repeat("x", maxProgress))
	job.Progress(make(chan int))
	expectEntry(t, queue, "v1", Running, 0, `"a third"`)
	if count := strings.Count(log.String(), "progress was dropped"); count != 1 {
		t.Fatalf("logged %d times, want once:\n%s", count, log.String())
	}
}

func expectEntry[V any](t *testing.T, queue *Queue[V], key string, state State, ahead int, progress string) Entry[V] {
	t.Helper()
	entry, found, err := queue.Get(t.Context(), key)
	if err != nil || !found || entry.State != state || entry.Ahead != ahead || string(entry.Progress) != progress {
		t.Fatalf("%s: %+v, found %v, %v; want %s, %d ahead, progress %q", key, entry, found, err, state, ahead,
			progress)
	}
	return entry
}

// seenEntry is what a watch yielded: an entry or the error that ended it
type seenEntry[V any] struct {
	entry Entry[V]
	err   error
}

// watchInto watches key until the test ends, sending what it yields
func watchInto[V any](t *testing.T, queue *Queue[V], key string) <-chan seenEntry[V] {
	t.Helper()
	seen := make(chan seenEntry[V], 16)
	go func() {
		defer close(seen)
		for entry, err := range queue.Watch(t.Context(), key) {
			seen <- seenEntry[V]{entry: entry, err: err}
		}
	}()
	return seen
}

func expectSeen[V any](t *testing.T, seen <-chan seenEntry[V], state State, ahead int, progress string) Entry[V] {
	t.Helper()
	select {
	case got, open := <-seen:
		switch {
		case !open:
			t.Fatalf("the watch ended before %s", state)
		case got.err != nil || got.entry.State != state || got.entry.Ahead != ahead ||
			string(got.entry.Progress) != progress:
			t.Fatalf("the watch yielded %+v, %v; want %s, %d ahead, progress %q", got.entry, got.err, state, ahead,
				progress)
		}
		return got.entry
	case <-time.After(5 * time.Second):
		t.Fatalf("the watch yielded nothing in five seconds; want %s", state)
	}
	return Entry[V]{}
}

// expectNothingSeen waits past a watcher's next read for it to yield nothing
func expectNothingSeen[V any](t *testing.T, seen <-chan seenEntry[V]) {
	t.Helper()
	select {
	case got := <-seen:
		t.Fatalf("the watch yielded %+v, %v for a change it does not show", got.entry, got.err)
	case <-time.After(3 * watchEvery):
	}
}

func expectWatchEnded[V any](t *testing.T, seen <-chan seenEntry[V]) {
	t.Helper()
	select {
	case got, open := <-seen:
		if open {
			t.Fatalf("the watch yielded %+v, %v after its end", got.entry, got.err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the watch did not end in five seconds")
	}
}

// working runs a Work loop until the function it returns is called, which
// waits for the loop to return
func working[V any](t *testing.T, queue *Queue[V], handle func(context.Context, Job[V]) error,
	options ...WorkOption,
) (stop func()) {
	t.Helper()
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- queue.Work(ctx, handle, options...) }()
	return func() {
		cancel()
		if err := <-done; err != nil && !errors.Is(err, context.Canceled) {
			t.Errorf("the Work loop returned %v", err)
		}
	}
}
