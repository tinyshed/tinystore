package main

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
)

// workload is a case on one implementation: the state a phase starts from, the
// requests of each worker, and what can be checked only once the phase is over
type workload interface {
	prepare(ctx context.Context) error
	requests(worker, workers int) (request, error)
	verify(ctx context.Context) error
}

// request is one worker's next request, which the worker alone calls
type request func(context.Context) (step, error)

// step is what a request was and what came of it
type step struct {
	op, outcome string
}

// violation is an answer that breaks the case's promise: a code taken twice,
// an event handled twice, a count given twice
type violation string

func (v violation) Error() string {
	return string(v)
}

// runPhase runs every worker's requests until the phase ends; a request that
// has begun is finished, so that none is cut short and counted as failed
func runPhase(ctx context.Context, load workload, workers int, length time.Duration) (*tally, error) {
	requests := make([]request, workers)
	for worker := range workers {
		var err error
		if requests[worker], err = load.requests(worker, workers); err != nil {
			return nil, err
		}
	}

	tallies := make([]*tally, workers)
	start := time.Now()
	deadline := start.Add(length)
	var running sync.WaitGroup
	for worker, next := range requests {
		running.Go(func() { tallies[worker] = drive(ctx, next, deadline) })
	}
	running.Wait()

	total := newTally()
	for _, t := range tallies {
		total.add(t)
	}
	total.elapsed = time.Since(start)
	return total, nil
}

// drive calls one worker's requests until the deadline, each timed from its
// call to its return, waiting for a connection or a commit included
func drive(ctx context.Context, next request, deadline time.Time) *tally {
	t := newTally()
	for {
		began := time.Now()
		if !began.Before(deadline) {
			return t
		}
		done, err := next(ctx)
		t.record(done, err, time.Since(began))
	}
}

// tally is the requests of a worker or a phase: their latencies by operation,
// their outcomes, and their errors by kind with the first of each
type tally struct {
	latencies  map[string][]time.Duration
	outcomes   map[string]int
	errors     map[string]int
	first      map[string]error
	violations int
	elapsed    time.Duration
}

func newTally() *tally {
	return &tally{
		latencies: map[string][]time.Duration{}, outcomes: map[string]int{},
		errors: map[string]int{}, first: map[string]error{},
	}
}

func (t *tally) record(done step, err error, took time.Duration) {
	t.latencies[done.op] = append(t.latencies[done.op], took)
	if err == nil {
		t.outcomes[done.outcome]++
		return
	}
	kind := errorKind(err)
	t.errors[kind]++
	if t.first[kind] == nil {
		t.first[kind] = err
	}
	var broken violation
	if errors.As(err, &broken) {
		t.violations++
	}
}

func (t *tally) add(other *tally) {
	for op, latencies := range other.latencies {
		t.latencies[op] = append(t.latencies[op], latencies...)
	}
	for outcome, count := range other.outcomes {
		t.outcomes[outcome] += count
	}
	for kind, count := range other.errors {
		t.errors[kind] += count
		if t.first[kind] == nil {
			t.first[kind] = other.first[kind]
		}
	}
	t.violations += other.violations
}

// summary is the phase as a whole: its requests, their rate and latencies, and
// its errors
func (t *tally) summary() string {
	var all []time.Duration
	for _, latencies := range t.latencies {
		all = append(all, latencies...)
	}
	slices.Sort(all)
	failed := 0
	for _, count := range t.errors {
		failed += count
	}
	return fmt.Sprintf("operations=%d operations_per_second=%.0f %s errors=%d", len(all),
		float64(len(all))/t.elapsed.Seconds(), percentiles(all), failed)
}

// print writes the phase's outcomes, each operation's latencies, and each
// kind of error with the first of its kind
func (t *tally) print(label string) {
	var outcomes []string
	for _, outcome := range slices.Sorted(maps.Keys(t.outcomes)) {
		outcomes = append(outcomes, fmt.Sprintf("%s=%d", outcome, t.outcomes[outcome]))
	}
	fmt.Printf("outcomes %s %s\n", label, strings.Join(outcomes, " "))
	for _, op := range slices.Sorted(maps.Keys(t.latencies)) {
		latencies := t.latencies[op]
		slices.Sort(latencies)
		fmt.Printf("op=%s %s operations=%d %s\n", op, label, len(latencies), percentiles(latencies))
	}
	for _, kind := range slices.Sorted(maps.Keys(t.errors)) {
		fmt.Printf("error %s kind=%s count=%d first=%q\n", label, kind, t.errors[kind], t.first[kind].Error())
	}
}

func percentiles(sorted []time.Duration) string {
	if len(sorted) == 0 {
		return "p50=- p99=- p999=- max=-"
	}
	at := func(q float64) time.Duration { return sorted[int(q*float64(len(sorted)-1))].Round(time.Microsecond) }
	return fmt.Sprintf("p50=%v p99=%v p999=%v max=%v", at(0.5), at(0.99), at(0.999), at(1))
}

// errorKind is the name an error is counted under
func errorKind(err error) string {
	var broken violation
	switch {
	case errors.As(err, &broken):
		return string(broken)
	case errors.Is(err, tinystore.ErrConflict):
		return "conflict"
	case errors.Is(err, kv.ErrOutcomeUnknown):
		return "outcome_unknown"
	case errors.Is(err, tinystore.ErrLimit):
		return "limit"
	case errors.Is(err, tinystore.ErrInvalid):
		return "invalid"
	case errors.Is(err, tinystore.ErrClosed):
		return "closed"
	case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
		return "cancelled"
	case strings.Contains(err.Error(), "SQLITE_BUSY"), strings.Contains(err.Error(), "database is locked"):
		return "busy"
	}
	return "other"
}
