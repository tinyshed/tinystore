package jobs

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// stepsKept counts the steps the file keeps, of every job
func stepsKept(t *testing.T, queues *testQueues) int {
	t.Helper()
	var n int
	err := queues.file.ViewPrepared(t.Context(), func(r sqlite.Reader) error {
		return sqlite.QueryRow(t.Context(), r, `select count(*) from _tinystore_jobs_steps`).Scan(&n)
	})
	if err != nil {
		t.Fatal(err)
	}
	return n
}

// A step runs once in a run of its job: the attempt after a failure, and the
// one after a worker that died, get its kept answer and run only what is left.
func TestAStepRunsOnceAcrossTheAttemptsOfARun(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "agent")
	mustEnqueue(t, queue, "what is a store?")
	searches, answers := 0, 0
	search := func(context.Context) ([]string, error) {
		searches++
		return []string{"a file", "a lock"}, nil
	}

	first := mustClaim(t, queue, Lease(time.Minute))
	if hits, err := Step(t.Context(), first, "search", search); err != nil || len(hits) != 2 {
		t.Fatalf("the first search: %v, %v", hits, err)
	}
	queues.clock.advance(2 * time.Minute) // the worker died before its next step

	second := mustClaim(t, queue, Lease(time.Minute))
	hits, err := Step(t.Context(), second, "search", search)
	if err != nil || strings.Join(hits, "+") != "a file+a lock" || searches != 1 {
		t.Fatalf("the search again: %v, %v, run %d times", hits, err, searches)
	}
	_, err = Step(t.Context(), second, "answer", func(context.Context) (string, error) {
		answers++
		return "", errors.New("the model is down")
	})
	if err == nil {
		t.Fatal("a step that failed answered")
	}
	if err = second.Retry(t.Context(), err, After(0)); err != nil {
		t.Fatal(err)
	}

	third := mustClaim(t, queue)
	if _, err = Step(t.Context(), third, "search", search); err != nil || searches != 1 {
		t.Fatalf("the third search: %v, run %d times", err, searches)
	}
	answer, err := Step(t.Context(), third, "answer", func(context.Context) (string, error) {
		answers++
		return "a directory", nil
	})
	if err != nil || answer != "a directory" || answers != 2 {
		t.Fatalf("the answer: %q, %v, asked %d times", answer, err, answers)
	}
	if err = third.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if n := stepsKept(t, queues); n != 0 {
		t.Fatalf("a run that is done left %d steps", n)
	}
}

// an attempt whose lease another claim took keeps nothing, and the attempt
// holding the job does not see what it tried to keep
func TestAStepOfALostLeaseKeepsNothing(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "agent")
	mustEnqueue(t, queue, "run")
	stale := mustClaim(t, queue, Lease(time.Minute))
	queues.clock.advance(2 * time.Minute)
	current := mustClaim(t, queue)

	if err := stale.Keep(t.Context(), "late", json.RawMessage(`1`)); !errors.Is(err, tinystore.ErrConflict) {
		t.Fatalf("a stale lease kept a step: %v", err)
	}
	if _, found, err := current.Kept(t.Context(), "late"); err != nil || found {
		t.Fatalf("the current attempt sees a stale step: %v, %v", found, err)
	}
}

// A run that ends takes its steps along: done, failed for good, or cancelled
// while it waits; a repeating job's next run starts without them.
func TestStepsGoWithTheirRun(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "agent", MaxAttempts(1))
	keep := func(job Job[string]) {
		t.Helper()
		if err := job.Keep(t.Context(), "search", json.RawMessage(`["a"]`)); err != nil {
			t.Fatal(err)
		}
	}

	mustEnqueue(t, queue, "fails")
	failing := mustClaim(t, queue)
	keep(failing)
	if err := failing.Retry(t.Context(), errors.New("past its attempts")); err != nil {
		t.Fatal(err)
	}
	if n := stepsKept(t, queues); n != 0 {
		t.Fatalf("a job that failed for good left %d steps", n)
	}

	snoozing := openTestQueue[string](t, queues, "snoozing")
	mustEnqueue(t, snoozing, "waits", Key("k"))
	waiting := mustClaim(t, snoozing)
	keep(waiting)
	if err := waiting.Snooze(t.Context(), After(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if n := stepsKept(t, queues); n != 1 {
		t.Fatalf("a snoozed job keeps %d steps, not its one", n)
	}
	if cancelled, err := snoozing.Cancel(t.Context(), "k"); err != nil || !cancelled {
		t.Fatalf("the cancel: %v, %v", cancelled, err)
	}
	if n := stepsKept(t, queues); n != 0 {
		t.Fatalf("a cancelled job left %d steps", n)
	}

	repeating := openTestQueue[string](t, queues, "hourly")
	mustEnqueue(t, repeating, "every hour", Key("hourly"), Every(time.Hour))
	queues.clock.advance(time.Hour)
	run := mustClaim(t, repeating)
	keep(run)
	if err := run.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	queues.clock.advance(time.Hour)
	next := mustClaim(t, repeating)
	if _, found, err := next.Kept(t.Context(), "search"); err != nil || found {
		t.Fatalf("a repeat's next run found its last run's step: %v, %v", found, err)
	}
}

// a step's name is 1 to 256 bytes of UTF-8 and its answer at most 1 MiB of
// JSON; an answer that no longer reads into the step's type fails the step
func TestAStepsNameAndAnswerAreBounded(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "agent")
	mustEnqueue(t, queue, "run")
	job := mustClaim(t, queue)

	for _, name := range []string{"", strings.Repeat("n", maxStepName+1), "\xff"} {
		if err := job.Keep(t.Context(), name, json.RawMessage(`1`)); !errors.Is(err, tinystore.ErrInvalid) {
			t.Errorf("a step named %q: %v", name, err)
		}
	}
	if err := job.Keep(t.Context(), "text", json.RawMessage(`not JSON`)); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("an answer that is not JSON: %v", err)
	}
	large := json.RawMessage(`"` + strings.Repeat("x", maxValue) + `"`)
	var limit *tinystore.LimitError
	if err := job.Keep(t.Context(), "large", large); !errors.As(err, &limit) {
		t.Errorf("an answer past 1 MiB: %v", err)
	}

	if err := job.Keep(t.Context(), "count", json.RawMessage(`"seven"`)); err != nil {
		t.Fatal(err)
	}
	_, err := Step(t.Context(), job, "count", func(context.Context) (int, error) { return 7, nil })
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an answer that no longer reads: %v", err)
	}
}
