package jobs

import (
	"context"
	"errors"
	"strconv"
	"strings"
	"testing"
	"time"
	_ "time/tzdata" // the zones of the tests, whatever the host has

	"github.com/tinyshed/tinystore"
)

func TestMalformedKeptIntervalsAndLargeCronStepsFail(t *testing.T) {
	for _, text := range []string{"@every 1ms", "@every 9223372036854775807h"} {
		if _, err := parseRepeat(text); !errors.Is(err, tinystore.ErrCorrupt) {
			t.Errorf("kept repeat %q: %v", text, err)
		}
	}
	step := strconv.FormatInt(int64(^uint(0)>>1), 10)
	repeat := Cron("1/"+step+" * * * *", time.UTC)
	if repeat.err != nil {
		t.Fatalf("a large step: %v", repeat.err)
	}
	if invalid := Cron("* * * * *", time.FixedZone("unloadable-zone", 3600)); !errors.Is(invalid.err, tinystore.ErrInvalid) {
		t.Fatalf("an unreopenable zone: %v", invalid.err)
	}
	if oversized := Cron(strings.Repeat("1,", maxRepeat)+"* * * * *", time.UTC); !errors.Is(oversized.err, tinystore.ErrLimit) {
		t.Fatalf("an unbounded cron: %v", oversized.err)
	}
}

func mustZone(t *testing.T, name string) *time.Location {
	t.Helper()
	zone, err := time.LoadLocation(name)
	if err != nil {
		t.Fatal(err)
	}
	return zone
}

// a cron expression runs at the times its fields accept, in its zone, and is
// kept as its five fields and the zone's name
//
//	*/15 9-17 * * mon-fri   Friday 17:50 → Monday 09:00
//	0 0 31 * *              31 January → 31 March
//	30 8 1 * 1              a first of the month or a Monday, whichever is first
func TestACronExpressionRunsWhenItsFieldsSay(t *testing.T) {
	moscow := mustZone(t, "Europe/Moscow")
	for _, check := range []struct {
		expr        string
		after, want time.Time
	}{
		{"*/15 9-17 * * mon-fri", time.Date(2026, 10, 2, 17, 50, 0, 0, moscow), time.Date(2026, 10, 5, 9, 0, 0, 0, moscow)},
		{"*/15 9-17 * * mon-fri", time.Date(2026, 10, 5, 9, 0, 0, 0, moscow), time.Date(2026, 10, 5, 9, 15, 0, 0, moscow)},
		{"0 0 31 * *", time.Date(2026, 1, 31, 0, 0, 0, 0, moscow), time.Date(2026, 3, 31, 0, 0, 0, 0, moscow)},
		{"30 8 1 * 1", time.Date(2026, 9, 27, 12, 0, 0, 0, moscow), time.Date(2026, 9, 28, 8, 30, 0, 0, moscow)},
		{"@daily", time.Date(2026, 9, 27, 12, 0, 0, 0, moscow), time.Date(2026, 9, 28, 0, 0, 0, 0, moscow)},
		{"0 12 29 feb *", time.Date(2026, 3, 1, 0, 0, 0, 0, moscow), time.Date(2028, 2, 29, 12, 0, 0, 0, moscow)},
	} {
		repeat := Cron(check.expr, moscow)
		if repeat.err != nil {
			t.Fatalf("%q: %v", check.expr, repeat.err)
		}
		if got := repeat.next(check.after); !got.Equal(check.want) {
			t.Fatalf("%q after %v: %v, want %v", check.expr, check.after, got, check.want)
		}
	}
	if text := Daily("03:10", moscow).String(); text != "10 3 * * * Europe/Moscow" {
		t.Fatalf("Daily is kept as %q", text)
	}
	for _, expr := range []string{"0 0 30 2 *", "61 * * * *", "* * *", "*/0 * * * *"} {
		if repeat := Cron(expr, moscow); !errors.Is(repeat.err, tinystore.ErrInvalid) {
			t.Fatalf("%q was taken: %v", expr, repeat.err)
		}
	}
}

// Every runs at the multiples of its interval since the Unix epoch, so that a
// restart does not shift it, and is kept in its largest whole unit
func TestEveryRunsAtTheMultiplesOfItsInterval(t *testing.T) {
	every := Every(15 * time.Minute)
	if every.String() != "@every 15m" {
		t.Fatalf("Every is kept as %q", every.String())
	}
	after := time.Date(2026, 9, 27, 12, 7, 31, 0, time.UTC)
	if got := every.next(after); !got.Equal(time.Date(2026, 9, 27, 12, 15, 0, 0, time.UTC)) {
		t.Fatalf("after 12:07:31 it runs at %v", got)
	}
	kept, err := parseRepeat(Every(90 * time.Second).String())
	if err != nil || kept.every != 90*time.Second {
		t.Fatalf("a kept @every reads back as %+v: %v", kept, err)
	}
}

// a wall time daylight saving skips runs when the skip ends, and one it
// repeats runs once
func TestAScheduleKeepsItsZoneAcrossDaylightSaving(t *testing.T) {
	york := mustZone(t, "America/New_York")
	skipped := Cron("30 2 * * *", york)
	if got := skipped.next(time.Date(2026, 3, 7, 12, 0, 0, 0, york)); !got.Equal(time.Date(2026, 3, 8, 3, 0, 0, 0, york)) {
		t.Fatalf("02:30 on the day it does not exist runs at %v", got)
	}
	if got := skipped.next(time.Date(2026, 3, 8, 3, 0, 0, 0, york)); !got.Equal(time.Date(2026, 3, 9, 2, 30, 0, 0, york)) {
		t.Fatalf("the day after the skip it runs at %v", got)
	}
	repeated := Cron("30 1 * * *", york)
	first := repeated.next(time.Date(2026, 10, 31, 12, 0, 0, 0, york))
	second := repeated.next(first)
	if first.In(york).Day() != 1 || second.In(york).Day() != 2 {
		t.Fatalf("01:30 on the day it happens twice runs at %v, then %v", first, second)
	}
	kept, err := parseRepeat(repeated.String())
	if err != nil || !kept.next(first).Equal(second) {
		t.Fatalf("the kept repeat %q reads back as %+v: %v", repeated.String(), kept, err)
	}
}

// A repeating job moves to its next time when it is settled. A run that took
// long, or a program down for hours, runs once and not for every time it
// missed, and a failure does not end it.
func TestARepeatingJobNeitherOverlapsNorPilesUp(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	schedule, err := OpenSchedule(t.Context(), queues.Store, "tick", Every(time.Minute))
	if err != nil {
		t.Fatal(err)
	}
	nothingDue(t, schedule)
	queues.clock.advance(time.Minute)
	job := mustClaim(t, schedule, Lease(time.Hour))
	if !job.At.Equal(testStart.Add(time.Minute)) {
		t.Fatalf("the first run is for %v", job.At)
	}
	queues.clock.advance(10 * time.Minute)
	nothingDue(t, schedule)
	if err = job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	nothingDue(t, schedule)
	queues.clock.advance(time.Minute)
	job = mustClaim(t, schedule)
	if !job.At.Equal(testStart.Add(12 * time.Minute)) {
		t.Fatalf("after ten missed minutes the next run is for %v", job.At)
	}
	if err = job.Fail(t.Context(), errors.New("the database is locked")); err != nil {
		t.Fatal(err)
	}
	entry, found, err := schedule.Get(t.Context(), "tick")
	if err != nil || !found || entry.State != Waiting || entry.Err != "the database is locked" ||
		!entry.At.Equal(testStart.Add(13*time.Minute)) {
		t.Fatalf("a failed run left %+v, %v, %v", entry, found, err)
	}
}

// the repeat in the program is the one a schedule keeps: opened again with
// another, it moves to that repeat's next time
func TestAScheduleTakesTheProgramsRepeat(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	if _, err := OpenSchedule(t.Context(), queues.Store, "purge", Daily("03:10", time.UTC)); err != nil {
		t.Fatal(err)
	}
	queues = queues.reopen(t)
	schedule, err := OpenSchedule(t.Context(), queues.Store, "purge", Daily("04:20", time.UTC))
	if err != nil {
		t.Fatal(err)
	}
	entry, _, err := schedule.Get(t.Context(), "purge")
	if err != nil || entry.Repeat != "20 4 * * * UTC" || !entry.At.Equal(time.Date(2026, 9, 28, 4, 20, 0, 0, time.UTC)) {
		t.Fatalf("the schedule kept %+v: %v", entry, err)
	}
	if _, err = OpenQueue[string](t.Context(), queues.Store, "purge"); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a schedule opened as a queue: %v", err)
	}
}

// a job keeps its last run: when the run a handler finished began and how
// long it took, beside its error and its next time; a run given back records
// nothing, and a job that failed for good keeps the run that failed it
func TestAJobKeepsItsLastRun(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	purge, err := OpenSchedule(t.Context(), queues.Store, "purge", Every(time.Hour))
	if err != nil {
		t.Fatal(err)
	}
	expectRun(t, purge, "purge", time.Time{}, 0, "")
	queues.clock.advance(time.Hour)
	began := queues.clock.Now()
	job := mustClaim(t, purge)
	queues.clock.advance(3 * time.Second)
	if err = job.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if entry := expectRun(t, purge, "purge", began, 3*time.Second, ""); !entry.At.Equal(began.Add(time.Hour)) {
		t.Fatalf("the next run is for %v", entry.At)
	}

	queues.clock.advance(time.Hour)
	failing := queues.clock.Now()
	job = mustClaim(t, purge)
	queues.clock.advance(2 * time.Second)
	if err = job.Fail(t.Context(), errors.New("the disk is full")); err != nil {
		t.Fatal(err)
	}
	expectRun(t, purge, "purge", failing, 2*time.Second, "the disk is full")

	queues.clock.advance(time.Hour)
	stop := working(t, purge, func(ctx context.Context, _ Job[struct{}]) error {
		<-ctx.Done()
		return ctx.Err()
	})
	waitForState(t, purge, "purge", Running)
	stop()
	expectRun(t, purge, "purge", failing, 2*time.Second, "the disk is full")

	once := openTestQueue[string](t, queues, "once", MaxAttempts(1))
	mustEnqueue(t, once, "x", Key("k"))
	gone := queues.clock.Now()
	job2 := mustClaim(t, once)
	queues.clock.advance(time.Second)
	if err = job2.Fail(t.Context(), errors.New("no")); err != nil {
		t.Fatal(err)
	}
	if entry := expectRun(t, once, "k", gone, time.Second, "no"); entry.State != Failed {
		t.Fatalf("the failed job is %s", entry.State)
	}
}

func expectRun[V any](t *testing.T, queue *Queue[V], key string, ran time.Time, took time.Duration, err string) Entry[V] {
	t.Helper()
	entry, found, getErr := queue.Get(t.Context(), key)
	if getErr != nil || !found || !entry.Ran.Equal(ran) || entry.Took != took || entry.Err != err {
		t.Fatalf("%s: %+v, %v, %v; want ran %v, took %v, error %q", key, entry, found, getErr, ran, took, err)
	}
	return entry
}

func waitForState[V any](t *testing.T, queue *Queue[V], key string, state State) {
	t.Helper()
	for deadline := time.Now().Add(5 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		if entry, _, err := queue.Get(t.Context(), key); err == nil && entry.State == state {
			return
		}
	}
	t.Fatalf("%s did not become %s in five seconds", key, state)
}
