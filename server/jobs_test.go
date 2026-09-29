package server

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/jobs"
	"github.com/tinyshed/tinystore/server/internal/client"
	"github.com/tinyshed/tinystore/server/wire"
)

func openQueue(t *testing.T, conn *client.Conn, queue wire.JobsQueue) uint64 {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.JobsOpen, queue)
	if err != nil {
		t.Fatal(err)
	}
	var handle wire.Handle
	if err := handle.Decode(body); err != nil {
		t.Fatal(err)
	}
	return handle.Handle
}

func fetchJob(t *testing.T, conn *client.Conn, handle uint64, key string) wire.JobsEntry {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.JobsGet, wire.JobsKey{Handle: handle, Key: key})
	if err != nil {
		t.Fatal(err)
	}
	var entry wire.JobsEntry
	if err := entry.Decode(body); err != nil {
		t.Fatal(err)
	}
	return entry
}

func enqueueJobs(t *testing.T, conn *client.Conn, handle uint64, batch ...wire.JobsJob) error {
	t.Helper()
	_, err := conn.Call(t.Context(), wire.JobsEnqueue, wire.JobsBatch{Handle: handle, Jobs: batch})
	return err
}

func TestJobsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	reminders := openQueue(t, conn, wire.JobsQueue{Name: "reminders", MaxAttempts: 3})

	later := time.Now().Add(time.Hour).UnixMilli()
	if err := enqueueJobs(t, conn, reminders,
		wire.JobsJob{Value: `{"user":42,"text":"call mom"}`, Key: "mom", At: later},
		wire.JobsJob{Value: `{"user":7}`, Key: "now"},
	); err != nil {
		t.Fatal(err)
	}
	mom := fetchJob(t, conn, reminders, "mom")
	if !mom.Found || mom.Value != `{"user":42,"text":"call mom"}` || mom.At != later || mom.State != 1 {
		t.Fatalf("a job waiting: %+v", mom)
	}

	if _, err := conn.Call(t.Context(), wire.JobsUpdate, wire.JobsChange{
		Handle:  reminders,
		JobsJob: wire.JobsJob{Key: "mom", Value: `{"user":42,"text":"call dad"}`},
	}); err != nil {
		t.Fatal(err)
	}
	if mom = fetchJob(t, conn, reminders, "mom"); !strings.Contains(mom.Value, "dad") || mom.At != later {
		t.Fatalf("an updated job: %+v", mom)
	}

	claimed := claim(t, conn, reminders)
	if !claimed.Found || claimed.Key != "now" || claimed.Attempt != 1 || claimed.Job == 0 {
		t.Fatalf("a claim: %+v", claimed)
	}
	settled := settleJobs(t, conn, wire.JobsOutcome{Job: claimed.Job, How: wire.JobAck})
	if settled.Errors[0] != nil {
		t.Fatalf("an ack: %+v", settled.Errors[0])
	}
	if again := settleJobs(t, conn, wire.JobsOutcome{Job: claimed.Job, How: wire.JobAck}); again.Errors[0] == nil ||
		again.Errors[0].Code != wire.CodeInvalid {
		t.Fatalf("a job settled twice: %+v", again.Errors[0])
	}
	if nothing := claim(t, conn, reminders); nothing.Found {
		t.Fatalf("a claim with nothing due: %+v", nothing)
	}

	body, err := conn.Call(t.Context(), wire.JobsCancel, wire.JobsKey{Handle: reminders, Key: "mom"})
	var cancelled wire.JobsEntry
	if err != nil || cancelled.Decode(body) != nil || !cancelled.Found {
		t.Fatalf("a cancel: %+v %v", cancelled, err)
	}
	if gone := fetchJob(t, conn, reminders, "mom"); gone.Found {
		t.Fatalf("a cancelled job: %+v", gone)
	}
}

func claim(t *testing.T, conn *client.Conn, handle uint64) wire.JobsHeld {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.JobsClaim, wire.JobsLease{Handle: handle})
	if err != nil {
		t.Fatal(err)
	}
	var held wire.JobsHeld
	if err := held.Decode(body); err != nil {
		t.Fatal(err)
	}
	return held
}

func settleJobs(t *testing.T, conn *client.Conn, outcomes ...wire.JobsOutcome) wire.JobsSettled {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.JobsSettle, wire.JobsOutcomes{Outcomes: outcomes})
	if err != nil {
		t.Fatal(err)
	}
	var settled wire.JobsSettled
	if err := settled.Decode(body); err != nil {
		t.Fatal(err)
	}
	return settled
}

// an enqueue of many is one transaction: a job the queue refuses leaves none
func TestAJobsEnqueueIsOneTransaction(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	pushes := openQueue(t, conn, wire.JobsQueue{Name: "pushes"})
	err := enqueueJobs(t, conn, pushes,
		wire.JobsJob{Value: `{"to":1}`, Key: "one"},
		wire.JobsJob{Value: `{"to":`, Key: "two"},
	)
	var failure *wire.Error
	if !asError(err, &failure) || failure.Code != wire.CodeInvalid || failure.What["call"] != "1" {
		t.Fatalf("a batch with a value that is not JSON: %v", err)
	}
	if one := fetchJob(t, conn, pushes, "one"); one.Found {
		t.Fatal("the first job of a refused batch stayed")
	}
}

// a remote worker's outcomes settle its jobs: an ack removes one, a retry
// runs it again with its attempt counted
func TestARemoteWorkerSettlesByItsOutcomes(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	sends := openQueue(t, conn, wire.JobsQueue{Name: "sends", BackoffFirst: 1, BackoffMost: 1})
	var batch []wire.JobsJob
	for _, key := range []string{"a", "b", "c", "d", "e"} {
		batch = append(batch, wire.JobsJob{Value: `"` + key + `"`, Key: key})
	}
	if err := enqueueJobs(t, conn, sends, batch...); err != nil {
		t.Fatal(err)
	}

	st, err := conn.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: sends, Workers: 2}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	attempts := map[string][]uint64{}
	for acked := 0; acked < 5; {
		held := nextHeld(t, st)
		attempts[held.Key] = append(attempts[held.Key], held.Attempt)
		how := uint64(wire.JobAck)
		if held.Key == "c" && held.Attempt == 1 {
			how = wire.JobRetry
		} else {
			acked++
		}
		sendOutcome(t, st, wire.JobsOutcome{Job: held.Job, How: how, Err: "try again"}, false)
	}
	if err := st.Send(t.Context(), nil, true); err != nil {
		t.Fatal(err)
	}
	if _, last, err := st.Next(t.Context()); err != nil || !last {
		t.Fatalf("the work stream's end: %v %v", last, err)
	}
	if got := attempts["c"]; len(got) != 2 || got[0] != 1 || got[1] != 2 {
		t.Fatalf("the retried job's attempts: %v", got)
	}
	for _, key := range []string{"a", "c", "e"} {
		if left := fetchJob(t, conn, sends, key); left.Found {
			t.Fatalf("an acknowledged job stayed: %+v", left)
		}
	}
}

func nextHeld(t *testing.T, st *client.Stream) wire.JobsHeld {
	t.Helper()
	body, last, err := st.Next(t.Context())
	if err != nil || last {
		t.Fatalf("a job: %v, last %v", err, last)
	}
	var held wire.JobsHeld
	if err := held.Decode(body); err != nil {
		t.Fatal(err)
	}
	return held
}

func sendOutcome(t *testing.T, st *client.Stream, outcome wire.JobsOutcome, last bool) {
	t.Helper()
	if err := st.Send(t.Context(), outcome.Append(nil), last); err != nil {
		t.Fatal(err)
	}
}

// A worker that goes away with a job in its hands fails that attempt, as a
// process that died would. The job claimed ahead for it goes back uncounted.
func TestALostWorkerFailsTheAttemptsInItsHands(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	crashes := openQueue(t, conn, wire.JobsQueue{Name: "crashes"})
	if err := enqueueJobs(t, conn, crashes,
		wire.JobsJob{Value: `1`, Key: "first"}, wire.JobsJob{Value: `2`, Key: "second"},
	); err != nil {
		t.Fatal(err)
	}

	worker := ts.dial(t, wire.Hello{})
	st, err := worker.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: openQueue(t, worker,
		wire.JobsQueue{Name: "crashes"}), Workers: 1}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	held := nextHeld(t, st)
	if held.Key != "first" {
		t.Fatalf("the first job handed over: %+v", held)
	}
	_ = worker.Close()

	waitFor(t, "the lost attempt", func() bool {
		first := fetchJob(t, conn, crashes, "first")
		return first.Found && first.State == uint64(jobs.Waiting) && first.Err != ""
	})
	first := fetchJob(t, conn, crashes, "first")
	if first.Attempt != 1 || !strings.Contains(first.Err, "went away") {
		t.Fatalf("the job in the worker's hands: %+v", first)
	}
	waitFor(t, "the job claimed ahead to go back", func() bool {
		return fetchJob(t, conn, crashes, "second").State == uint64(jobs.Waiting)
	})
	if second := fetchJob(t, conn, crashes, "second"); second.Attempt != 0 || second.Err != "" {
		t.Fatalf("the job claimed ahead: %+v", second)
	}
}

// a schedule opens with its repeat, and its one job is claimed as any queue's
func TestAScheduleIsAQueueOfOneRepeatingJob(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	purge := openQueue(t, conn, wire.JobsQueue{Name: "purge", Schedule: &wire.Repeat{Every: 1000}})
	job := fetchJob(t, conn, purge, "purge")
	if !job.Found || job.Repeat != "@every 1s" || job.Value != "{}" {
		t.Fatalf("a schedule's job: %+v", job)
	}
	if err := enqueueJobs(t, conn, purge, wire.JobsJob{Value: `{}`}); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("an enqueue into a schedule: %v", err)
	}
	if _, err := conn.Call(t.Context(), wire.JobsOpen, wire.JobsQueue{
		Name:     "nightly",
		Schedule: &wire.Repeat{Cron: "10 3 * * *", Zone: "Nowhere/Atlantis"},
	}); codeOfError(err) != wire.CodeInvalid {
		t.Fatalf("a zone nobody named: %v", err)
	}
}

func TestAJobsScanPagesTheQueue(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	later := openQueue(t, conn, wire.JobsQueue{Name: "later"})
	for _, key := range []string{"chat:42:a", "chat:42:b", "chat:43:c"} {
		if err := enqueueJobs(t, conn, later, wire.JobsJob{Value: `{}`, Key: key, After: 60_000}); err != nil {
			t.Fatal(err)
		}
	}
	st, err := conn.Open(t.Context(), wire.JobsScan, wire.JobsQuery{Handle: later, Prefix: "chat:42:"}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	var keys []string
	for {
		body, last, err := st.Next(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if last {
			break
		}
		var entry wire.JobsEntry
		if err := entry.Decode(body); err != nil {
			t.Fatal(err)
		}
		keys = append(keys, entry.Key)
	}
	if strings.Join(keys, ",") != "chat:42:a,chat:42:b" {
		t.Fatalf("scanned %v", keys)
	}
}

// a Go program's queue and a wire client's are one queue: what one enqueues
// the other claims
func TestAWireClientAndAGoProgramShareAQueue(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	store, err := ts.server.jobsStore(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	type reminder struct {
		User int    `json:"user"`
		Text string `json:"text"`
	}
	queue, err := jobs.OpenQueue[reminder](context.Background(), store, "shared")
	if err != nil {
		t.Fatal(err)
	}
	if err := queue.Enqueue(context.Background(), reminder{User: 42, Text: "hi"}, jobs.Key("go")); err != nil {
		t.Fatal(err)
	}
	shared := openQueue(t, conn, wire.JobsQueue{Name: "shared"})
	held := claim(t, conn, shared)
	var got reminder
	if err := json.Unmarshal([]byte(held.Value), &got); err != nil || got.User != 42 || got.Text != "hi" {
		t.Fatalf("Go's job over the wire: %+v %v", held, err)
	}
	if err := errors.Join(); err != nil {
		t.Fatal(err)
	}
}
