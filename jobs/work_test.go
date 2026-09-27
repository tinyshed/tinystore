package jobs

import (
	"context"
	"errors"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

// Work acknowledges a job whose handler returned nil, retries one whose handler
// failed or panicked, and leaves one its handler settled itself as it settled it
func TestWorkSettlesByWhatTheHandlerReturns(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work")
	for _, key := range []string{"done", "failed", "panicked", "snoozed"} {
		mustEnqueue(t, queue, key, Key(key))
	}
	settle := func(ctx context.Context, job Job[string]) error {
		switch job.Value {
		case "failed":
			return errors.New("the provider is down")
		case "panicked":
			panic("a nil map")
		case "snoozed":
			return job.Snooze(ctx, After(time.Hour))
		}
		return nil
	}
	if err := queue.Work(t.Context(), settle, Workers(4), UntilIdle()); err != nil {
		t.Fatal(err)
	}

	if _, found, err := queue.Get(t.Context(), "done"); err != nil || found {
		t.Fatalf("the acknowledged job is still there: %v, %v", found, err)
	}
	for key, cause := range map[string]string{"failed": "the provider is down", "panicked": "panicked: a nil map"} {
		entry, _, err := queue.Get(t.Context(), key)
		if err != nil || entry.State != Waiting || entry.Attempt != 1 || !strings.Contains(entry.Err, cause) {
			t.Fatalf("the %s job is %+v: %v", key, entry, err)
		}
	}
	snoozed, _, err := queue.Get(t.Context(), "snoozed")
	if err != nil || snoozed.State != Waiting || snoozed.Attempt != 0 || snoozed.Err != "" {
		t.Fatalf("the snoozed job is %+v: %v", snoozed, err)
	}

	queues.clock.advance(2 * time.Second)
	ran := workUntilIdle(t, queue)
	slices.Sort(ran) // each backoff is a tenth longer or shorter at random
	if strings.Join(ran, ",") != "failed,panicked" {
		t.Fatalf("after their backoff Work ran %v", ran)
	}
	queues.clock.advance(time.Hour)
	if ran := workUntilIdle(t, queue); strings.Join(ran, ",") != "snoozed" {
		t.Fatalf("after its snooze Work ran %v", ran)
	}
}

// workUntilIdle acknowledges every job due, one worker in the order of their
// times, and returns their values
func workUntilIdle(t *testing.T, queue *Queue[string]) []string {
	t.Helper()
	var ran []string
	err := queue.Work(t.Context(), func(_ context.Context, job Job[string]) error {
		ran = append(ran, job.Value)
		return nil
	}, UntilIdle())
	if err != nil {
		t.Fatal(err)
	}
	return ran
}

// jobs due together are claimed as many at a time as there are free workers,
// and their settlements share the writes that claim, so that a burst does not
// cost a commit a job
func TestJobsDueTogetherAreClaimedInBatches(t *testing.T) {
	const burst = 64
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "burst")
	err := queues.Tx(t.Context(), func(tx *Tx) error {
		for n := range burst {
			if err := queue.WithTx(tx).Enqueue(t.Context(), n); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}

	before := commits(t, queues)
	var running sync.WaitGroup
	running.Add(burst)
	err = queue.Work(t.Context(), func(context.Context, Job[int]) error {
		running.Done()
		running.Wait() // each job waits for all the others, so all of them are held at once
		return nil
	}, Workers(burst), UntilIdle())
	if err != nil {
		t.Fatal(err)
	}
	if written := commits(t, queues) - before; written > 8 {
		t.Fatalf("%d jobs took %d commits to claim and settle", burst, written)
	}
	nothingDue(t, queue)
}

// a Work loop whose job another claim took, after its lease ended while the
// handler ran, lets that lease go rather than trying to extend it again at once
func TestWorkLetsGoOfALeaseAnotherClaimTook(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work", Lease(200*time.Millisecond))
	mustEnqueue(t, queue, "job")

	started, release := make(chan struct{}), make(chan struct{})
	releaseOnce := sync.OnceFunc(func() { close(release) })
	t.Cleanup(releaseOnce) // before the store closes, which waits for the handler
	ctx, stop := context.WithCancel(t.Context())
	worked := make(chan error, 1)
	go func() {
		worked <- queue.Work(ctx, func(context.Context, Job[string]) error {
			close(started)
			<-release
			return nil
		})
	}()
	<-started
	taken := claimTheEndedLease(t, queues, queue)

	time.Sleep(300 * time.Millisecond) // the loop wakes to extend within half a lease
	before := commits(t, queues)
	time.Sleep(200 * time.Millisecond)
	if written := commits(t, queues) - before; written > 2 {
		t.Fatalf("the loop wrote %d times in 200 ms for a lease it had lost", written)
	}

	releaseOnce()
	stop()
	if err := <-worked; !errors.Is(err, context.Canceled) {
		t.Fatalf("Work ended with %v", err)
	}
	if err := taken.Ack(t.Context()); err != nil {
		t.Fatalf("the claim that took the job could not settle it: %v", err)
	}
	nothingDue(t, queue)
}

// claimTheEndedLease moves the clock past the lease a Work loop holds and
// claims its job, both inside one transaction, so that the loop cannot extend
// the lease in between
func claimTheEndedLease[V any](t *testing.T, queues *testQueues, queue *Queue[V]) Job[V] {
	t.Helper()
	var taken Job[V]
	err := queues.Tx(t.Context(), func(tx *Tx) error {
		queues.clock.advance(time.Second)
		job, found, err := queue.WithTx(tx).Claim(t.Context())
		if err == nil && !found {
			err = errors.New("the job of the ended lease was not claimable")
		}
		taken = job
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return taken
}

// commits is how many transactions the writer of jobs.db has committed
func commits(t *testing.T, queues *testQueues) uint64 {
	t.Helper()
	counters, err := queues.file.WriterCounters(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return counters.Commits
}
