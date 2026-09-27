package jobs

import (
	"errors"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

// a job whose lease ended without a settlement is claimed again, its
// vanished attempt counted
func TestAJobWhoseLeaseEndedRunsAgain(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work")
	mustEnqueue(t, queue, "job")
	first := mustClaim(t, queue, Lease(time.Minute))
	nothingDue(t, queue)
	queues.clock.advance(time.Minute)
	second := mustClaim(t, queue)
	if second.Value != "job" || first.Attempt != 1 || second.Attempt != 2 {
		t.Fatalf("the claims were attempts %d and %d", first.Attempt, second.Attempt)
	}
}

// a lease that ended and whose job another claim took settles nothing; one
// that ended while nobody took the job still settles it
func TestAStaleLeaseSettlesNothing(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work")
	mustEnqueue(t, queue, "job")
	stale := mustClaim(t, queue, Lease(time.Minute))
	queues.clock.advance(2 * time.Minute)
	current := mustClaim(t, queue)
	for _, settle := range []func() error{
		func() error { return stale.Ack(t.Context()) },
		func() error { return stale.Retry(t.Context(), errors.New("late")) },
		func() error { return stale.Snooze(t.Context(), After(time.Hour)) },
		func() error { return stale.Extend(t.Context(), time.Hour) },
	} {
		if err := settle(); !errors.Is(err, tinystore.ErrConflict) {
			t.Fatalf("a stale lease settled with %v", err)
		}
	}
	if err := current.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := current.Ack(t.Context()); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a second Ack: %v", err)
	}

	mustEnqueue(t, queue, "late but alone")
	late := mustClaim(t, queue, Lease(time.Minute))
	queues.clock.advance(2 * time.Minute)
	if err := late.Ack(t.Context()); err != nil {
		t.Fatalf("an ended lease nobody took: %v", err)
	}
	nothingDue(t, queue)
}

// a retry waits longer each time, a tenth either way, and past MaxAttempts
// the job fails for good with its last error
func TestARetryWaitsLongerEachTimeThenFailsForGood(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work", MaxAttempts(3), Backoff(10*time.Second, time.Minute))
	mustEnqueue(t, queue, "job", Key("k"))
	for attempt, wait := range []time.Duration{10 * time.Second, 20 * time.Second} {
		job := mustClaim(t, queue)
		if job.Attempt != attempt+1 {
			t.Fatalf("claim %d is attempt %d", attempt+1, job.Attempt)
		}
		if err := job.Retry(t.Context(), errors.New("the provider is down")); err != nil {
			t.Fatal(err)
		}
		queues.clock.advance(wait*9/10 - time.Millisecond)
		nothingDue(t, queue)
		queues.clock.advance(wait*2/10 + time.Millisecond)
	}
	job := mustClaim(t, queue)
	if err := job.Retry(t.Context(), errors.New("still down")); err != nil {
		t.Fatal(err)
	}
	entry, found, err := queue.Get(t.Context(), "k")
	if err != nil || !found || entry.State != Failed || entry.Err != "still down" || entry.Attempt != 3 {
		t.Fatalf("past its attempts the job is %+v, %v, %v", entry, found, err)
	}
	queues.clock.advance(time.Hour)
	nothingDue(t, queue)
}

// a snooze puts the job back without counting the attempt
func TestASnoozeCountsNoAttempt(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work", MaxAttempts(1))
	mustEnqueue(t, queue, "job")
	for range 3 {
		job := mustClaim(t, queue)
		if job.Attempt != 1 {
			t.Fatalf("a snoozed job came back as attempt %d", job.Attempt)
		}
		if err := job.Snooze(t.Context(), After(time.Minute)); err != nil {
			t.Fatal(err)
		}
		nothingDue(t, queue)
		queues.clock.advance(time.Minute)
	}
	if err := mustClaim(t, queue).Snooze(t.Context()); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Snooze without a time: %v", err)
	}
}

// a job failed for good is kept for KeepFailed, found by Get and Scan, and
// removed by maintenance after it
func TestAFailedJobIsKeptThenRemoved(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work", KeepFailed(time.Hour))
	mustEnqueue(t, queue, "keyed", Key("k"))
	mustEnqueue(t, queue, "no key")
	for range 2 {
		if err := mustClaim(t, queue).Fail(t.Context(), errors.New("gone")); err != nil {
			t.Fatal(err)
		}
	}
	page, err := queue.Scan(t.Context(), Query{State: Failed})
	if err != nil || len(page.Entries) != 2 || page.Entries[0].State != Failed || page.Entries[0].Err != "gone" {
		t.Fatalf("the failed jobs are %+v: %v", page, err)
	}
	queues.clock.advance(time.Hour)
	done, err := queues.Maintain(t.Context())
	if err != nil || done.Failed != 2 {
		t.Fatalf("maintenance removed %+v: %v", done, err)
	}
	if _, found, _ := queue.Get(t.Context(), "k"); found {
		t.Fatal("a failed job outlived KeepFailed")
	}
}

// Extend keeps a job its worker's past the lease it was claimed with
func TestExtendKeepsTheJob(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "work")
	mustEnqueue(t, queue, "long")
	job := mustClaim(t, queue, Lease(time.Minute))
	queues.clock.advance(50 * time.Second)
	if err := job.Extend(t.Context(), time.Minute); err != nil {
		t.Fatal(err)
	}
	queues.clock.advance(50 * time.Second)
	nothingDue(t, queue)
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
}
