package jobs

import (
	"bytes"
	"encoding/json"
	"errors"
	"math"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
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
	if cancelled, err := queue.Cancel(t.Context(), "m2"); err != nil || cancelled {
		t.Fatalf("a running job's Cancel: %v, %v", cancelled, err)
	}
	if err := job.Ack(t.Context()); err != nil {
		t.Fatalf("the worker's Ack after a late Cancel: %v", err)
	}
	if cancelled, err := queue.Cancel(t.Context(), "m2"); err != nil || cancelled {
		t.Fatalf("a done job's Cancel: %v, %v", cancelled, err)
	}
}

type testMessage struct {
	Chat int64
	Text string
	Tags []string
}

// a value goes in as JSON and comes back as it went in, a []byte as its bytes,
// a large one through its own row; one JSON cannot write or past 1 MiB is refused
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
