package jobs

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// a job is claimed at its time and not a moment before
func TestAJobRunsAtItsTimeAndNotBefore(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later")
	mustEnqueue(t, queue, "at noon tomorrow", At(testStart.Add(24*time.Hour)))
	mustEnqueue(t, queue, "in an hour", After(time.Hour))
	mustEnqueue(t, queue, "in the past", At(testStart.Add(-time.Hour)))

	job := mustClaim(t, queue)
	if job.Value != "in the past" || !job.At.Equal(testStart.Add(-time.Hour)) {
		t.Fatalf("the first claim got %+v", job)
	}
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	nothingDue(t, queue)
	queues.clock.advance(time.Hour - time.Millisecond)
	nothingDue(t, queue)
	queues.clock.advance(time.Millisecond)
	if job = mustClaim(t, queue); job.Value != "in an hour" {
		t.Fatalf("an hour on the claim got %+v", job)
	}
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	queues.clock.advance(23 * time.Hour)
	if job := mustClaim(t, queue); job.Value != "at noon tomorrow" {
		t.Fatalf("a day on the claim got %+v", job)
	}
}

// a key names one job: an Enqueue under it adds nothing, keeps the value, and
// can bring the job forward but never push it back
func TestAKeyNamesOneJobAndARepeatOnlyBringsItForward(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later")
	ten, nine, eleven := testStart.Add(10*time.Hour), testStart.Add(9*time.Hour), testStart.Add(11*time.Hour)
	mustEnqueue(t, queue, "first", Key("k"), At(ten))
	mustEnqueue(t, queue, "second", Key("k"), At(eleven))
	checkEntry(t, queue, "k", "first", ten)
	mustEnqueue(t, queue, "third", Key("k"), At(nine))
	checkEntry(t, queue, "k", "first", nine)

	queues.clock.advance(12 * time.Hour)
	if job := mustClaim(t, queue); job.Value != "first" {
		t.Fatalf("the claim got %+v", job)
	}
	nothingDue(t, queue)
}

func checkEntry[V comparable](t *testing.T, queue *Queue[V], key string, value V, at time.Time) {
	t.Helper()
	entry, found, err := queue.Get(t.Context(), key)
	if err != nil || !found || entry.Value != value || !entry.At.Equal(at) {
		t.Fatalf("under %q: %+v, %v, %v; want %v at %v", key, entry, found, err, value, at)
	}
}

// an Enqueue while its key's job runs asks for one run more after it, so that
// a change made while the handler worked is not lost; several are one run
func TestAnEnqueueWhileItsJobRunsAsksForOneRunMore(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "alarms")
	mustEnqueue(t, queue, "user 42", Key("user:42"))
	job := mustClaim(t, queue)

	mustEnqueue(t, queue, "user 42", Key("user:42"), After(time.Hour))
	mustEnqueue(t, queue, "user 42", Key("user:42"), After(2*time.Hour))
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	checkEntry(t, queue, "user:42", "user 42", testStart.Add(time.Hour))
	queues.clock.advance(time.Hour)
	mustClaim(t, queue)
	nothingDue(t, queue)
}

// with KeepDone a key runs once: an Enqueue under a key that waits, runs or
// ran within KeepDone adds nothing
func TestKeepDoneMakesAKeyRunOnce(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "pushes", KeepDone(time.Hour))
	mustEnqueue(t, queue, "push", Key("message:1:user:2"))
	job := mustClaim(t, queue)
	mustEnqueue(t, queue, "push", Key("message:1:user:2"))
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, queue, "push", Key("message:1:user:2"))
	nothingDue(t, queue)

	queues.clock.advance(time.Hour)
	if _, err := queues.Maintain(t.Context()); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, queue, "push", Key("message:1:user:2"))
	mustClaim(t, queue)
}

// Update changes a job that still waits, and a failed one; a job that runs,
// ran or was cancelled is ErrConflict
func TestUpdateChangesOnlyAWaitingJob(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later", MaxAttempts(1))
	nine := testStart.Add(9 * time.Hour)
	mustEnqueue(t, queue, "draft", Key("m1"), At(nine))
	if err := queue.Update(t.Context(), "m1", "edited", At(nine.Add(time.Hour))); err != nil {
		t.Fatal(err)
	}
	checkEntry(t, queue, "m1", "edited", nine.Add(time.Hour))

	queues.clock.advance(10 * time.Hour)
	job := mustClaim(t, queue)
	if err := queue.Update(t.Context(), "m1", "too late"); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("an Update of a running job: %v", err)
	}
	if err := job.Fail(t.Context(), errors.New("the chat is gone")); err != nil {
		t.Fatal(err)
	}
	if err := queue.Update(t.Context(), "m1", "again"); err != nil {
		t.Fatalf("an Update of a failed job: %v", err)
	}
	if job = mustClaim(t, queue); job.Value != "again" || job.Attempt != 1 {
		t.Fatalf("the failed job started again as %+v", job)
	}
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := queue.Update(t.Context(), "m1", "gone"); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("an Update of a done job: %v", err)
	}
}

// Cancel removes a job that waits or failed and says so; a running job is
// left to its worker, and false says it is too late
func TestCancelSaysWhetherItCameInTime(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later")
	mustEnqueue(t, queue, "draft", Key("m1"), After(time.Hour))
	if cancelled, err := queue.Cancel(t.Context(), "m1"); err != nil || !cancelled {
		t.Fatalf("a waiting job's Cancel: %v, %v", cancelled, err)
	}
	queues.clock.advance(time.Hour)
	nothingDue(t, queue)

	mustEnqueue(t, queue, "draft", Key("m2"))
	job := mustClaim(t, queue)
	if cancelled, err := queue.Cancel(t.Context(), "m2"); err != nil || !cancelled {
		t.Fatalf("a running job's Cancel: %v, %v", cancelled, err)
	}
	if err := job.Ack(t.Context()); !errors.Is(err, tinystore.ErrConflict) || !errors.Is(err, ErrCancelled) {
		t.Fatalf("the worker's Ack after a Cancel: %v", err)
	}
	queues.clock.advance(time.Hour) // past its lease: it does not come back
	nothingDue(t, queue)

	mustEnqueue(t, queue, "draft", Key("m3"))
	if err := mustClaim(t, queue).Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if cancelled, err := queue.Cancel(t.Context(), "m3"); err != nil || cancelled {
		t.Fatalf("a done job's Cancel: %v, %v", cancelled, err)
	}
}

type testMessage struct {
	Chat int64
	Text string
	Tags []string
}

// a value goes in as JSON and comes back as it went in, a []byte as its bytes,
// a large one through its own row; one JSON cannot write or past 1 MiB is
// refused
func TestAValueComesBackAsTheJSONItWentIn(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	messages := openTestQueue[testMessage](t, queues, "messages")
	long := strings.Repeat("happy new year ", 100)
	for _, message := range []testMessage{{Chat: 42, Text: "hi", Tags: []string{"a"}}, {Chat: 7, Text: long}} {
		mustEnqueue(t, messages, message, Key(message.Text[:2]))
		entry, _, err := messages.Get(t.Context(), message.Text[:2])
		if err != nil || entry.Value.Text != message.Text || entry.Value.Chat != message.Chat {
			t.Fatalf("%d bytes came back as %+v: %v", len(message.Text), entry.Value, err)
		}
	}
	for range 2 {
		job := mustClaim(t, messages)
		if job.Value.Text != "hi" && job.Value.Text != long {
			t.Fatalf("a claim got %+v", job.Value)
		}
	}

	raw := openTestQueue[[]byte](t, queues, "raw")
	mustEnqueue(t, raw, []byte{0, 1, 0xff, '{'})
	if job := mustClaim(t, raw); !bytes.Equal(job.Value, []byte{0, 1, 0xff, '{'}) {
		t.Fatalf("raw bytes came back as %v", job.Value)
	}

	numbers := openTestQueue[float64](t, queues, "numbers")
	if err := numbers.Enqueue(t.Context(), math.NaN()); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a NaN: %v", err)
	}
	if err := raw.Enqueue(t.Context(), make([]byte, maxValue+1)); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("a value over 1 MiB: %v", err)
	}
	decoded := openTestQueue[json.RawMessage](t, queues, "json")
	mustEnqueue(t, decoded, json.RawMessage(`{"a":1}`))
	if job := mustClaim(t, decoded); string(job.Value) != `{"a":1}` {
		t.Fatalf("a raw message came back as %s", job.Value)
	}
}

// a queue past MaxWaiting refuses the next job with ErrLimit, and takes jobs
// again once some left
func TestAQueuePastMaxWaitingRefusesTheNextJob(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "bounded", MaxWaiting(2))
	mustEnqueue(t, queue, 1)
	mustEnqueue(t, queue, 2)
	var refused *JobError
	if err := queue.Enqueue(t.Context(), 3); !errors.Is(err, tinystore.ErrLimit) || !errors.As(err, &refused) {
		t.Fatalf("the third job: %v", err)
	}
	if err := mustClaim(t, queue).Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, queue, 3)
}

func TestAnExistingKeyDoesNotConsumeMaxWaiting(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "bounded_key", MaxWaiting(1))
	mustEnqueue(t, queue, 1, Key("same"))
	if err := queue.Enqueue(t.Context(), 2, Key("same")); err != nil {
		t.Fatalf("existing key at capacity: %v", err)
	}
	if err := queues.Tx(t.Context(), func(tx *Tx) error {
		inside := queue.WithTx(tx)
		if err := inside.Enqueue(t.Context(), 3, Key("same")); err != nil {
			return err
		}
		return inside.Enqueue(t.Context(), 4, Key("another"))
	}); !errors.Is(err, tinystore.ErrLimit) {
		t.Fatalf("new key past capacity inside Tx: %v", err)
	}
}

func TestAQueueCannotUseAnotherStoresTransaction(t *testing.T) {
	first := openTestQueues(t, t.TempDir())
	second := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, first, "own")
	if err := second.Tx(t.Context(), func(tx *Tx) error {
		return queue.WithTx(tx).Enqueue(t.Context(), 1)
	}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("foreign transaction: %v", err)
	}
	if waiting := queue.state.waiting.Load(); waiting != 0 {
		t.Fatalf("foreign transaction enqueued %d jobs", waiting)
	}
}

func TestConcurrentEnqueuesCannotPassMaxWaiting(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "concurrent_bound", MaxWaiting(3))
	const callers = 32
	var wg sync.WaitGroup
	errs := make([]error, callers)
	for i := range callers {
		wg.Go(func() { errs[i] = queue.Enqueue(t.Context(), i) })
	}
	wg.Wait()
	accepted := 0
	for _, err := range errs {
		if err == nil {
			accepted++
		} else if !errors.Is(err, tinystore.ErrLimit) {
			t.Fatalf("unexpected enqueue failure: %v", err)
		}
	}
	if accepted != 3 {
		t.Fatalf("%d jobs accepted past MaxWaiting(3)", accepted)
	}
}

func TestTheDurableWaitingCountFollowsMovesAndRemovals(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "counted", MaxWaiting(2))
	mustEnqueue(t, queue, 1, Key("first"))
	mustEnqueue(t, queue, 2, Key("second"))
	count := func() int64 {
		t.Helper()
		var waiting int64
		err := queues.file.Lookup(t.Context(), func(r sqlite.Reader) error {
			return sqlite.QueryRow(t.Context(), r, countWaiting, queue.state.id).Scan(&waiting)
		})
		if err != nil {
			t.Fatal(err)
		}
		return waiting
	}
	if got := count(); got != 2 {
		t.Fatalf("two inserts counted as %d", got)
	}
	if err := mustClaim(t, queue).Snooze(t.Context(), After(time.Hour)); err != nil || count() != 2 {
		t.Fatalf("moving a row changed the count to %d: %v", count(), err)
	}
	if cancelled, err := queue.Cancel(t.Context(), "second"); err != nil || !cancelled || count() != 1 {
		t.Fatalf("cancel left %d waiting: %v", count(), err)
	}
}

// a repeat without a key could never be stopped, and a zone without a name
// would change meaning on another host
func TestARepeatNeedsAKeyAndANamedZone(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "digests")
	if err := queue.Enqueue(t.Context(), "digest", Every(time.Hour)); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a repeat without a key: %v", err)
	}
	if err := queue.Enqueue(t.Context(), "digest", Key("u"), Daily("20:00", time.Local)); !errors.Is(err,
		tinystore.ErrInvalid) {
		t.Fatalf("a repeat in time.Local: %v", err)
	}
}

// a job that leaves the queue leaves its key behind, where it names nothing:
// an Enqueue under it adds a job at once, and maintenance drops the rest
func TestAKeyLeftBehindNamesNothingAndMaintenanceDropsIt(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later")
	for n := range 3 {
		mustEnqueue(t, queue, "first", Key(fmt.Sprint("k", n)))
	}
	if err := queue.Work(t.Context(), func(context.Context, Job[string]) error { return nil }, UntilIdle()); err != nil {
		t.Fatal(err)
	}
	if left := keysKept(t, queues); left != 3 {
		t.Fatalf("three jobs done left %d keys", left)
	}

	mustEnqueue(t, queue, "second", Key("k1"))
	if entry, found, err := queue.Get(t.Context(), "k1"); err != nil || !found || entry.Value != "second" {
		t.Fatalf("a key left behind named %+v, %v, %v", entry, found, err)
	}
	if page, err := queue.Scan(t.Context(), Query{Prefix: "k"}); err != nil || keysOf(page.Entries) != "k1" {
		t.Fatalf("a scan over keys left behind found %v: %v", keysOf(page.Entries), err)
	}
	done, err := queues.Maintain(t.Context())
	if err != nil || done.Keys != 2 || keysKept(t, queues) != 1 {
		t.Fatalf("maintenance dropped %+v, %d keys kept: %v", done, keysKept(t, queues), err)
	}
}

// a job that moves takes its key along, brought forward, snoozed, retried or
// repeated: an Enqueue under the key finds it and adds nothing
func TestAMovedJobTakesItsKeyAlong(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later")
	mustEnqueue(t, queue, "job", Key("k"), After(time.Hour))
	mustEnqueue(t, queue, "job", Key("k"), After(time.Minute))
	queues.clock.advance(time.Minute)
	if err := mustClaim(t, queue).Snooze(t.Context(), After(time.Hour)); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, queue, "job", Key("k"), After(2*time.Hour))
	queues.clock.advance(time.Hour)
	if err := mustClaim(t, queue).Retry(t.Context(), errors.New("down"), After(time.Minute)); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, queue, "job", Key("k"), After(time.Hour))

	mustEnqueue(t, queue, "digest", Key("d"), Every(time.Hour))
	queues.clock.advance(time.Hour)
	for range 2 {
		if err := mustClaim(t, queue).Ack(t.Context()); err != nil {
			t.Fatal(err)
		}
	}
	mustEnqueue(t, queue, "digest", Key("d"), Every(time.Hour))
	if waiting := queue.state.waiting.Load(); waiting != 1 {
		t.Fatalf("%d jobs wait under two keys, one of them done", waiting)
	}
	if entry, found, err := queue.Get(t.Context(), "d"); err != nil || !found || entry.State != Waiting {
		t.Fatalf("the repeating job moved on to %+v, %v, %v", entry, found, err)
	}
}

// keysKept counts the rows of keys, those of jobs there and those left behind
func keysKept(t *testing.T, queues *testQueues) int {
	t.Helper()
	var count int
	err := queues.file.Lookup(t.Context(), func(r sqlite.Reader) error {
		return sqlite.QueryRow(t.Context(), r, `select count(*) from _tinystore_jobs_keys`).Scan(&count)
	})
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// Move sets a waiting job's time either way, as a deadline each ping pushes
// back does, and a key's job is brought forward without it but never back
func TestMoveSetsATimeLaterThanTheOneWaiting(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	checks := openTestQueue[string](t, queues, "checks")
	ten, eleven, nine := testStart.Add(10*time.Minute), testStart.Add(11*time.Minute), testStart.Add(9*time.Minute)
	mustEnqueue(t, checks, "backup", Key("check:7"), At(ten))
	mustEnqueue(t, checks, "backup", Key("check:7"), At(eleven), Move())
	checkEntry(t, checks, "check:7", "backup", eleven)
	mustEnqueue(t, checks, "backup", Key("check:7"), At(nine), Move())
	checkEntry(t, checks, "check:7", "backup", nine)

	queues.clock.advance(10 * time.Minute)
	mustEnqueue(t, checks, "backup", Key("check:7"), After(5*time.Minute), Move())
	nothingDue(t, checks)
	queues.clock.advance(5 * time.Minute)
	mustClaim(t, checks)

	if err := checks.Enqueue(t.Context(), "backup", Move()); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Move without a key: %v", err)
	}
	if err := checks.Update(t.Context(), "check:7", "backup", Move()); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Move in an Update: %v", err)
	}
}

// Move under a key without a job adds one, and under a running one sets the
// time of its next run to the last Move's, later or earlier, unless the
// queue keeps its keys once
func TestMoveCreatesAMissingJob(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	checks := openTestQueue[string](t, queues, "checks")
	mustEnqueue(t, checks, "backup", Key("check:7"), After(time.Minute), Move())
	checkEntry(t, checks, "check:7", "backup", testStart.Add(time.Minute))

	queues.clock.advance(time.Minute)
	job := mustClaim(t, checks)
	mustEnqueue(t, checks, "backup", Key("check:7"), After(time.Hour), Move())
	mustEnqueue(t, checks, "backup", Key("check:7"), After(2*time.Hour), Move())
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	checkEntry(t, checks, "check:7", "backup", testStart.Add(time.Minute+2*time.Hour))

	once := openTestQueue[string](t, queues, "once", KeepDone(time.Hour))
	mustEnqueue(t, once, "push", Key("push:1"))
	job = mustClaim(t, once)
	mustEnqueue(t, once, "push", Key("push:1"), After(time.Hour), Move())
	if err := job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if entry, _, err := once.Get(t.Context(), "push:1"); err != nil || entry.State != Done {
		t.Fatalf("a key KeepDone keeps ran again for a Move: %+v, %v", entry, err)
	}
}

// a time is from the year 1 to 9999, so that no time a job keeps reaches the
// times parked jobs are moved to
func TestATimePastTheYearsAJobKeepsIsRefused(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	later := openTestQueue[string](t, queues, "later")
	for _, at := range []time.Time{time.Date(10000, 1, 1, 0, 0, 0, 0, time.UTC), time.Date(-1, 1, 1, 0, 0, 0, 0, time.UTC)} {
		if err := later.Enqueue(t.Context(), "x", At(at)); !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("At(%v): %v", at, err)
		}
	}
	mustEnqueue(t, later, "x", At(time.Date(9999, 12, 31, 0, 0, 0, 0, time.UTC)))
	mustEnqueue(t, later, "now", Key("k"))
	if err := mustClaim(t, later).Snooze(t.Context(), At(time.Date(20000, 1, 1, 0, 0, 0, 0, time.UTC))); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a Snooze past the year 9999: %v", err)
	}
}
