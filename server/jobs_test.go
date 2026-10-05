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

// a work stream asked to end when idle ends once no job is due and none runs,
// as Work does with UntilIdle, and leaves a job due later waiting
func TestAWorkStreamUntilIdleEndsOnceNoJobIsDue(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	sends := openQueue(t, conn, wire.JobsQueue{Name: "sends"})
	if err := enqueueJobs(t, conn, sends, wire.JobsJob{Value: `1`, Key: "now"},
		wire.JobsJob{Value: `2`, Key: "later", After: time.Hour.Milliseconds()}); err != nil {
		t.Fatal(err)
	}

	st, err := conn.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: sends, UntilIdle: true}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	held := nextHeld(t, st)
	if held.Key != "now" {
		t.Fatalf("the job handed over: %+v", held)
	}
	sendOutcome(t, st, wire.JobsOutcome{Job: held.Job, How: wire.JobAck}, false)
	if _, last, err := st.Next(t.Context()); err != nil || !last {
		t.Fatalf("the work stream's end once idle: %v %v", last, err)
	}
	if later := fetchJob(t, conn, sends, "later"); later.State != uint64(jobs.Waiting) || later.Attempt != 0 {
		t.Fatalf("the job due later: %+v", later)
	}
}

// A work stream's loop extends its jobs' leases itself, so an extend sent on
// it is refused: the stream ends invalid rather than taking it as an ack, and
// the job in the client's hands fails that attempt.
func TestAnExtendOnAWorkStreamIsRefused(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	sends := openQueue(t, conn, wire.JobsQueue{Name: "sends"})
	if err := enqueueJobs(t, conn, sends, wire.JobsJob{Value: `1`, Key: "long"}); err != nil {
		t.Fatal(err)
	}

	st, err := conn.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: sends}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	held := nextHeld(t, st)
	sendOutcome(t, st, wire.JobsOutcome{Job: held.Job, How: wire.JobExtend, After: 60_000}, false)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second) // taken as an ack, the stream never ends
	defer cancel()
	_, _, err = st.Next(ctx)
	var failure *wire.Error
	if !asError(err, &failure) || failure.Code != wire.CodeInvalid {
		t.Fatalf("the work stream's end after an extend: %v", err)
	}
	waitFor(t, "the attempt in the worker's hands to fail", func() bool {
		return fetchJob(t, conn, sends, "long").Err != ""
	})
	if long := fetchJob(t, conn, sends, "long"); !long.Found || long.Attempt != 1 {
		t.Fatalf("the job whose extend was refused: %+v", long)
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

// a watcher follows a job a remote worker runs: its place, its progress as
// the worker reports it, and a Cancel that the worker is told of and that
// ends the watch; MaxRunning holds the next job until the first is settled
func TestAJobWatchFollowsARemoteWorkersJob(t *testing.T) {
	ts := startTestServer(t, Options{})
	watcher, worker := ts.dial(t, wire.Hello{}), ts.dial(t, wire.Hello{})
	videos := openQueue(t, watcher, wire.JobsQueue{Name: "videos", MaxRunning: 1})
	working := openQueue(t, worker, wire.JobsQueue{Name: "videos", MaxRunning: 1})
	if err := enqueueJobs(t, watcher, videos, wire.JobsJob{Value: `1`, Key: "a"}, wire.JobsJob{Value: `2`, Key: "b"}); err != nil {
		t.Fatal(err)
	}
	watch, err := watcher.Open(t.Context(), wire.JobsWatch, wire.JobsKey{Handle: videos, Key: "b"}, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = watch.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	expectWatched(t, watch, jobs.Waiting, 1, "")

	work, err := worker.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: working, Workers: 2, Cancels: true}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = work.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	first := nextHeld(t, work)
	expectWatched(t, watch, jobs.Waiting, 0, "")
	sendOutcome(t, work, wire.JobsOutcome{Job: first.Job, How: wire.JobProgress, Progress: `{"done":1}`}, false)
	deadline := time.Now().Add(5 * time.Second)
	for got := fetchJob(t, watcher, videos, "a"); got.Progress != `{"done":1}`; got = fetchJob(t, watcher, videos, "a") {
		if time.Now().After(deadline) {
			t.Fatalf("the worker's progress did not reach the job: %+v", got)
		}
		time.Sleep(10 * time.Millisecond)
	}

	sendOutcome(t, work, wire.JobsOutcome{Job: first.Job, How: wire.JobAck}, false)
	second := nextHeld(t, work)
	expectWatched(t, watch, jobs.Running, 0, "")
	body, err := watcher.Call(t.Context(), wire.JobsCancel, wire.JobsKey{Handle: videos, Key: "b"})
	var cancelled wire.JobsEntry
	if err == nil {
		err = cancelled.Decode(body)
	}
	if err != nil || !cancelled.Found {
		t.Fatalf("a running job's cancel: %+v, %v", cancelled, err)
	}
	if told := nextHeld(t, work); told.Job != second.Job || !told.Cancelled {
		t.Fatalf("the worker was told %+v", told)
	}
	expectWatched(t, watch, jobs.Cancelled, 0, "")
	if _, last, err := watch.Next(t.Context()); err != nil || !last {
		t.Fatalf("the watch's end: last %v, %v", last, err)
	}
	sendOutcome(t, work, wire.JobsOutcome{Job: second.Job, How: wire.JobAck}, true)
	if _, last, err := work.Next(t.Context()); err != nil || !last {
		t.Fatalf("the work stream's end: last %v, %v", last, err)
	}
}

func expectWatched(t *testing.T, st *client.Stream, state jobs.State, ahead uint64, progress string) {
	t.Helper()
	body, last, err := st.Next(t.Context())
	var entry wire.JobsEntry
	if err == nil {
		err = entry.Decode(body)
	}
	if err != nil || last || entry.State != uint64(state) || entry.Ahead != ahead || entry.Progress != progress {
		t.Fatalf("the watch sent %+v, last %v, %v; want %s, %d ahead, progress %q", entry, last, err, state, ahead,
			progress)
	}
}

// a job's last run travels with its entry: when the run a worker finished
// began and how long it took, beside its error
func TestAJobsLastRunOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	later := openQueue(t, conn, wire.JobsQueue{Name: "later"})
	if err := enqueueJobs(t, conn, later, wire.JobsJob{Value: `1`, Key: "a"}); err != nil {
		t.Fatal(err)
	}
	if fresh := fetchJob(t, conn, later, "a"); fresh.Ran != 0 || fresh.Took != 0 {
		t.Fatalf("a job never run: %+v", fresh)
	}
	before := time.Now().UnixMilli()
	held := claim(t, conn, later)
	time.Sleep(20 * time.Millisecond)
	settleJobs(t, conn, wire.JobsOutcome{Job: held.Job, How: wire.JobRetry, Err: "busy", After: 3_600_000})
	ran := fetchJob(t, conn, later, "a")
	if ran.Ran < before || ran.Ran > time.Now().UnixMilli() || ran.Took < 20 || ran.Err != "busy" {
		t.Fatalf("a run that took 20 ms: %+v", ran)
	}
}

// lookStep asks the server for the answer a held job's run kept under a step
func lookStep(t *testing.T, conn *client.Conn, job uint64, name string) wire.JobsKept {
	t.Helper()
	body, err := conn.Call(t.Context(), wire.JobsStep, wire.JobsAnswer{Job: job, Name: name})
	var kept wire.JobsKept
	if err == nil {
		err = kept.Decode(body)
	}
	if err != nil {
		t.Fatal(err)
	}
	return kept
}

func keepStep(t *testing.T, conn *client.Conn, job uint64, name, answer string) {
	t.Helper()
	if _, err := conn.Call(t.Context(), wire.JobsKeep, wire.JobsAnswer{Job: job, Name: name, Answer: answer}); err != nil {
		t.Fatal(err)
	}
}

// A remote worker's steps are kept across the attempts of a run, for a job a
// claim leased and for one a work stream handed over, which only its stream
// settles; a look-up carrying an answer is refused.
func TestAJobsStepsOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	agent := openQueue(t, conn, wire.JobsQueue{Name: "agent", BackoffFirst: 1, BackoffMost: 1})
	if err := enqueueJobs(t, conn, agent, wire.JobsJob{Value: `"claimed"`, Key: "claimed"}); err != nil {
		t.Fatal(err)
	}

	first := claim(t, conn, agent)
	if kept := lookStep(t, conn, first.Job, "search"); kept.Found {
		t.Fatalf("a step no run kept: %+v", kept)
	}
	keepStep(t, conn, first.Job, "search", `["a file"]`)
	settleJobs(t, conn, wire.JobsOutcome{Job: first.Job, How: wire.JobRetry, Err: "the model is down", HasAfter: true})
	second := claim(t, conn, agent)
	if kept := lookStep(t, conn, second.Job, "search"); !kept.Found || kept.Answer != `["a file"]` {
		t.Fatalf("the second attempt's step: %+v", kept)
	}
	_, err := conn.Call(t.Context(), wire.JobsStep, wire.JobsAnswer{Job: second.Job, Name: "search", Answer: "1"})
	if failureOf(err).Code != wire.CodeInvalid {
		t.Errorf("a look-up carrying an answer: %v", err)
	}
	settleJobs(t, conn, wire.JobsOutcome{Job: second.Job, How: wire.JobAck})

	if err = enqueueJobs(t, conn, agent, wire.JobsJob{Value: `"streamed"`, Key: "streamed"}); err != nil {
		t.Fatal(err)
	}
	st, err := conn.Open(t.Context(), wire.JobsWork, wire.JobsWorkers{Handle: agent, Workers: 1}, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = st.Response(t.Context()); err != nil {
		t.Fatal(err)
	}
	held := nextHeld(t, st)
	keepStep(t, conn, held.Job, "search", `"kept on a stream"`)
	if kept := lookStep(t, conn, held.Job, "search"); kept.Answer != `"kept on a stream"` {
		t.Fatalf("a streamed job's step: %+v", kept)
	}
	if settled := settleJobs(t, conn, wire.JobsOutcome{Job: held.Job, How: wire.JobAck}); settled.Errors[0] == nil ||
		settled.Errors[0].Code != wire.CodeInvalid {
		t.Fatalf("jobs.settle of a job a stream holds: %+v", settled.Errors)
	}
	sendOutcome(t, st, wire.JobsOutcome{Job: held.Job, How: wire.JobAck}, true)
	if _, last, err := st.Next(t.Context()); err != nil || !last {
		t.Fatalf("the work stream's end: %v %v", last, err)
	}
	if left := fetchJob(t, conn, agent, "streamed"); left.Found {
		t.Fatalf("the streamed job stayed: %+v", left)
	}
}

// a queue's group bound, its rate, a job's group, a Move and a spread repeat
// reach the engine over the wire, and a spread that is not an interval's, or
// a Move without a key, is refused
func TestJobsBoundsAndMovesOverTheWire(t *testing.T) {
	ts := startTestServer(t, Options{})
	conn := ts.dial(t, wire.Hello{})
	refreshes := openQueue(t, conn, wire.JobsQueue{Name: "refreshes", MaxRunningInGroup: 1, Rate: 2, Per: 60_000})
	if err := enqueueJobs(t, conn, refreshes,
		wire.JobsJob{Value: `{"dataset":1}`, Key: "refresh:1", Group: "db:42"},
		wire.JobsJob{Value: `{"dataset":2}`, Key: "refresh:2", Group: "db:42"},
		wire.JobsJob{Value: `{"dataset":3}`, Key: "refresh:3", Group: "db:7"},
		wire.JobsJob{Value: `{"dataset":4}`, Key: "refresh:4"},
	); err != nil {
		t.Fatal(err)
	}
	for _, want := range []string{"refresh:1", "refresh:3"} {
		if held := claim(t, conn, refreshes); !held.Found || held.Key != want {
			t.Fatalf("a claim took %+v, not %s", held, want)
		}
	}
	if held := claim(t, conn, refreshes); held.Found {
		t.Fatalf("a claim past the rate took %+v", held)
	}

	checks := openQueue(t, conn, wire.JobsQueue{Name: "checks"})
	later := time.Now().Add(time.Hour).UnixMilli()
	for _, at := range []int64{later, later + 60_000} {
		if err := enqueueJobs(t, conn, checks, wire.JobsJob{Value: `{}`, Key: "check:7", At: at, Move: true}); err != nil {
			t.Fatal(err)
		}
	}
	if entry := fetchJob(t, conn, checks, "check:7"); entry.At != later+60_000 {
		t.Fatalf("a moved deadline: %+v", entry)
	}
	err := enqueueJobs(t, conn, checks,
		wire.JobsJob{Value: `{}`, Key: "probe:7", Repeat: &wire.Repeat{Every: 30_000, Spread: true}})
	if entry := fetchJob(t, conn, checks, "probe:7"); err != nil || entry.Repeat != "@every 30s +6178ms" {
		t.Fatalf("a spread repeat: %+v, %v", entry, err)
	}
	for _, refused := range []wire.JobsJob{
		{Value: `{}`, Key: "probe:8", Repeat: &wire.Repeat{Cron: "* * * * *", Zone: "UTC", Spread: true}},
		{Value: `{}`, Move: true},
	} {
		if err := enqueueJobs(t, conn, checks, refused); codeOfError(err) != wire.CodeInvalid {
			t.Fatalf("%+v: %v", refused, err)
		}
	}
}
