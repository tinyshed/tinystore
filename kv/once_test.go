package kv

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

func openTestOnce[V any](t *testing.T, state *testState, name string, options ...BucketOption) *Once[V] {
	t.Helper()
	once, err := OpenOnce[V](t.Context(), state.Store, name, options...)
	if err != nil {
		t.Fatal(err)
	}
	return once
}

// a key's function runs once and its answer is kept a day: a second Run
// returns it without running anything, a day on or after a Delete it runs
// again, and the same key in another branch is another
func TestARunKeepsItsAnswerAndRunsAKeyOnce(t *testing.T) {
	state := openTestState(t, t.TempDir())
	charges := openTestOnce[string](t, state, "charges")
	ran := 0
	charge := func(context.Context) (string, error) {
		ran++
		return fmt.Sprintf("receipt %d", ran), nil
	}
	expectRun := func(once *Once[string], want string) {
		t.Helper()
		if receipt, err := once.Run(t.Context(), "req-7", charge); receipt != want || err != nil {
			t.Fatalf("Run: %q, %v; want %q", receipt, err, want)
		}
	}

	expectRun(charges, "receipt 1")
	expectRun(charges, "receipt 1")
	if receipt, found, err := charges.Get(t.Context(), "req-7"); receipt != "receipt 1" || !found || err != nil {
		t.Fatalf("the kept answer: %q, %v, %v", receipt, found, err)
	}
	state.clock.advance(24*time.Hour - time.Millisecond)
	expectRun(charges, "receipt 1")
	state.clock.advance(time.Millisecond)
	expectRun(charges, "receipt 2")

	if err := charges.Delete(t.Context(), "req-7"); err != nil {
		t.Fatal(err)
	}
	expectRun(charges, "receipt 3")
	expectRun(charges.Of("tenant-7"), "receipt 4")
	expectRun(charges.Of("tenant-7"), "receipt 4")

	if _, err := OpenBucket[string](t.Context(), state.Store, "charges"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("answers opened as a bucket: %v, not ErrInvalid", err)
	}
	if _, err := OpenOnce[string](t.Context(), state.Store, "x", Sliding(time.Hour)); !errors.Is(err,
		tinystore.ErrInvalid) {
		t.Fatalf("answers that slide: %v, not ErrInvalid", err)
	}
	hourly := openTestOnce[string](t, state, "hourly", DefaultTTL(time.Hour))
	expectRun(hourly, "receipt 5")
	state.clock.advance(time.Hour)
	expectRun(hourly, "receipt 6")
}

// an error keeps nothing and is returned, so the next Run runs again, and a
// panic lets go of its key as an error does
func TestAnErrorKeepsNothingAndTheNextRunRunsAgain(t *testing.T) {
	state := openTestState(t, t.TempDir())
	imports := openTestOnce[int](t, state, "imports")
	declined := errors.New("the provider said no")
	if _, err := imports.Run(t.Context(), "file-1", func(context.Context) (int, error) {
		return 0, declined
	}); !errors.Is(err, declined) {
		t.Fatalf("a failed Run returned %v", err)
	}
	if _, found, err := imports.Get(t.Context(), "file-1"); found || err != nil {
		t.Fatalf("a failed Run kept an answer: %v, %v", found, err)
	}

	func() {
		defer func() {
			if recover() == nil {
				t.Fatal("the function's panic did not reach its Run")
			}
		}()
		_, _ = imports.Run(t.Context(), "file-1", func(context.Context) (int, error) { panic("a bug") })
	}()
	if n, err := imports.Run(t.Context(), "file-1", func(context.Context) (int, error) {
		return 7, nil
	}); n != 7 || err != nil {
		t.Fatalf("the Run after an error and a panic: %d, %v", n, err)
	}
}

// a Run of a key another Run is running waits for it and returns its answer,
// one after a Run that failed runs its own function, and a key apart runs at
// once
func TestARunWaitsForTheRunOfItsKey(t *testing.T) {
	state := openTestState(t, t.TempDir())
	imports := openTestOnce[int](t, state, "imports")
	started, release := make(chan struct{}), make(chan struct{})
	slow := func(answer int, err error) func(context.Context) (int, error) {
		return func(context.Context) (int, error) {
			started <- struct{}{}
			<-release
			return answer, err
		}
	}
	never := func(context.Context) (int, error) {
		t.Error("a key's function ran while another Run of it was running")
		return 0, nil
	}

	first := runInBackground(t, imports, "file-1", slow(7, nil))
	<-started
	second := runInBackground(t, imports, "file-1", never)
	if n, err := imports.Run(t.Context(), "file-2", func(context.Context) (int, error) {
		return 9, nil
	}); n != 9 || err != nil {
		t.Fatalf("a key apart: %d, %v", n, err)
	}
	expectStillRunning(t, second)
	release <- struct{}{}
	expectAnswer(t, first, 7, nil)
	expectAnswer(t, second, 7, nil)

	failed := errors.New("the file is gone")
	first = runInBackground(t, imports, "file-3", slow(0, failed))
	<-started
	second = runInBackground(t, imports, "file-3", slow(8, nil))
	expectStillRunning(t, second)
	release <- struct{}{}
	expectAnswer(t, first, 0, failed)
	<-started
	release <- struct{}{}
	expectAnswer(t, second, 8, nil)
}

// a Run waiting for another ends with its context, and the answer of a Run
// whose context ended after its function returned is kept
func TestAWaitingRunEndsWithItsContextAndAnAnswerOutlivesIt(t *testing.T) {
	state := openTestState(t, t.TempDir())
	imports := openTestOnce[int](t, state, "imports")
	started, release := make(chan struct{}), make(chan struct{})
	first := runInBackground(t, imports, "file-1", func(context.Context) (int, error) {
		close(started)
		<-release
		return 7, nil
	})
	<-started
	waiting, cancel := context.WithTimeout(t.Context(), 50*time.Millisecond)
	defer cancel()
	if _, err := imports.Run(waiting, "file-1", func(context.Context) (int, error) {
		return 0, nil
	}); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("a Run waiting past its deadline: %v", err)
	}
	close(release)
	expectAnswer(t, first, 7, nil)

	ending, end := context.WithCancel(t.Context())
	if n, err := imports.Run(ending, "file-2", func(context.Context) (int, error) {
		end()
		return 8, nil
	}); n != 8 || err != nil {
		t.Fatalf("a Run whose context ended as its function returned: %d, %v", n, err)
	}
	if n, found, err := imports.Get(t.Context(), "file-2"); n != 8 || !found || err != nil {
		t.Fatalf("its answer: %d, %v, %v", n, found, err)
	}
}

type runResult struct {
	answer int
	err    error
}

// runInBackground runs a Run of key in a goroutine of its own and sends what it
// returns
func runInBackground(t *testing.T, once *Once[int], key string, fn func(context.Context) (int, error)) <-chan runResult {
	t.Helper()
	done := make(chan runResult, 1)
	go func() {
		answer, err := once.Run(t.Context(), key, fn)
		done <- runResult{answer: answer, err: err}
	}()
	return done
}

func expectStillRunning(t *testing.T, run <-chan runResult) {
	t.Helper()
	select {
	case got := <-run:
		t.Fatalf("a Run returned %+v while the Run of its key was running", got)
	case <-time.After(50 * time.Millisecond):
	}
}

func expectAnswer(t *testing.T, run <-chan runResult, answer int, err error) {
	t.Helper()
	select {
	case got := <-run:
		if got.answer != answer || !errors.Is(got.err, err) {
			t.Fatalf("a Run returned %d, %v; want %d, %v", got.answer, got.err, answer, err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a Run did not return in five seconds")
	}
}
