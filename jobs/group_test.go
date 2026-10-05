package jobs

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// claimAll claims every job due now and says the values claimed, in order
func claimAll(t *testing.T, queue *Queue[string]) ([]string, []Job[string]) {
	t.Helper()
	var values []string
	var held []Job[string]
	for {
		job, found, err := queue.Claim(t.Context())
		if err != nil {
			t.Fatal(err)
		}
		if !found {
			return values, held
		}
		values = append(values, job.Value)
		held = append(held, job)
	}
}

// MaxRunningInGroup bounds the running jobs of each group alone: a claim
// passes over a full group's jobs and takes the next, and a settlement lets
// the group's next job run, in the order of their times
func TestAGroupBoundsItsRunningJobs(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(2))
	for i := range 5 {
		mustEnqueue(t, refreshes, fmt.Sprintf("a%d", i), Group("db:a"), At(testStart.Add(time.Duration(i)*time.Second)))
	}
	mustEnqueue(t, refreshes, "b0", Group("db:b"), At(testStart.Add(5*time.Second)))
	mustEnqueue(t, refreshes, "free", At(testStart.Add(6*time.Second)))
	queues.clock.advance(time.Minute)

	claimed, held := claimAll(t, refreshes)
	if strings.Join(claimed, " ") != "a0 a1 b0 free" {
		t.Fatalf("the claims took %v", claimed)
	}
	if err := held[0].Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := held[1].Retry(t.Context(), errors.New("locked"), After(time.Hour)); err != nil {
		t.Fatal(err)
	}
	if claimed, _ = claimAll(t, refreshes); strings.Join(claimed, " ") != "a2 a3" {
		t.Fatalf("two places freed let %v run", claimed)
	}
}

// a claim parks the jobs of a full group once, rather than passing over them
// at every claim, so that a group with thousands of jobs due cannot hold back
// a job of another group behind them
func TestABusyGroupDoesNotHoldBackTheGroupsBehindIt(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(1))
	err := queues.Tx(t.Context(), func(tx *Tx) error {
		inTx := refreshes.WithTx(tx)
		for i := range 2*parkAtMost + 500 {
			if err := inTx.Enqueue(t.Context(), fmt.Sprintf("a%d", i), Group("db:a")); err != nil {
				return err
			}
		}
		return inTx.Enqueue(t.Context(), "b", Group("db:b"), After(time.Second))
	})
	if err != nil {
		t.Fatal(err)
	}
	queues.clock.advance(time.Second)

	if job := mustClaim(t, refreshes); job.Value != "a0" {
		t.Fatalf("the first claim took %s", job.Value)
	}
	if job := mustClaim(t, refreshes); job.Value != "b" {
		t.Fatalf("the second claim took %s", job.Value)
	}
	nothingDue(t, refreshes)
	if parked := parkedRows(t, queues); parked != 2*parkAtMost+499 {
		t.Fatalf("%d jobs are parked", parked)
	}
}

// parkedRows counts the jobs parked through the index that holds them
func parkedRows(t *testing.T, queues *testQueues) int {
	t.Helper()
	var count int
	err := queues.file.Lookup(t.Context(), func(r sqlite.Reader) error {
		return sqlite.QueryRow(t.Context(), r,
			`select count(*) from _tinystore_jobs where next >= `+parkedFromText).Scan(&count)
	})
	if err != nil {
		t.Fatal(err)
	}
	return count
}

// a parked job keeps its time and its key: Get finds it waiting at its time,
// an Enqueue under its key brings it forward only from that time, and it runs
// in the order of its time once its group has room
func TestAParkedJobKeepsItsTimeAndItsKey(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(1))
	mustEnqueue(t, refreshes, "first", Key("r:1"), Group("db:a"))
	mustEnqueue(t, refreshes, "second", Key("r:2"), Group("db:a"), At(testStart.Add(time.Second)))
	mustEnqueue(t, refreshes, "third", Key("r:3"), Group("db:a"), At(testStart.Add(2*time.Second)))
	queues.clock.advance(time.Minute)
	first := mustClaim(t, refreshes)
	nothingDue(t, refreshes)

	checkEntry(t, refreshes, "r:2", "second", testStart.Add(time.Second))
	mustEnqueue(t, refreshes, "second", Key("r:2"), At(testStart.Add(time.Hour)))
	checkEntry(t, refreshes, "r:2", "second", testStart.Add(time.Second))
	if entry, _, err := refreshes.Get(t.Context(), "r:3"); err != nil || entry.State != Waiting {
		t.Fatalf("a parked job is %+v: %v", entry, err)
	}
	if err := first.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if job := mustClaim(t, refreshes); job.Value != "second" {
		t.Fatalf("the group's next job is %s", job.Value)
	}
	if parked := parkedRows(t, queues); parked != 1 {
		t.Fatalf("%d jobs are parked", parked)
	}
}

// a job that leaves its group's place without a settlement gives it to the
// group's first parked job: one cancelled, and one whose attempts all ended
// without one
func TestACancelledOrAbandonedJobGivesItsGroupItsPlace(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(1), MaxAttempts(1),
		Lease(time.Second))
	mustEnqueue(t, refreshes, "cancelled", Key("r:1"), Group("db:a"))
	mustEnqueue(t, refreshes, "abandoned", Key("r:2"), Group("db:a"), At(testStart.Add(time.Millisecond)))
	mustEnqueue(t, refreshes, "last", Key("r:3"), Group("db:a"), At(testStart.Add(2*time.Millisecond)))
	queues.clock.advance(time.Millisecond)
	mustClaim(t, refreshes)
	nothingDue(t, refreshes)
	if cancelled, err := refreshes.Cancel(t.Context(), "r:1"); err != nil || !cancelled {
		t.Fatalf("Cancel: %v, %v", cancelled, err)
	}
	if job := mustClaim(t, refreshes); job.Value != "abandoned" {
		t.Fatalf("the cancelled job's place went to %s", job.Value)
	}
	queues.clock.advance(time.Millisecond)
	nothingDue(t, refreshes)

	queues.clock.advance(time.Minute) // its lease ends, and its one attempt with it
	if job := mustClaim(t, refreshes); job.Value != "last" {
		t.Fatalf("the abandoned job's place went to %s", job.Value)
	}
	if entry, _, err := refreshes.Get(t.Context(), "r:2"); err != nil || entry.State != Failed {
		t.Fatalf("the abandoned job is %+v: %v", entry, err)
	}
}

// a queue opened with another MaxRunningInGroup gives back the jobs the old
// bound parked, so that a raised bound or none runs them
func TestAChangedGroupBoundGivesBackTheParkedJobs(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(1))
	for i := range 4 {
		mustEnqueue(t, refreshes, fmt.Sprintf("a%d", i), Key(fmt.Sprintf("r:%d", i)), Group("db:a"))
	}
	if claimed, _ := claimAll(t, refreshes); len(claimed) != 1 {
		t.Fatalf("a bound of one claimed %v", claimed)
	}

	queues = queues.reopen(t)
	refreshes = openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(3))
	if claimed, _ := claimAll(t, refreshes); strings.Join(claimed, " ") != "a0 a1 a2" {
		t.Fatalf("a bound of three claimed %v", claimed)
	}
	queues = queues.reopen(t)
	refreshes = openTestQueue[string](t, queues, "refreshes")
	if claimed, _ := claimAll(t, refreshes); len(claimed) != 4 {
		t.Fatalf("no bound claimed %v", claimed)
	}
	if parked := parkedRows(t, queues); parked != 0 || keysKept(t, queues) != 4 {
		t.Fatalf("%d jobs stayed parked", parked)
	}
	for i := range 4 {
		if _, found, err := refreshes.Get(t.Context(), fmt.Sprintf("r:%d", i)); err != nil || !found {
			t.Fatalf("r:%d is not found by its key: %v", i, err)
		}
	}
}

// a failed job keeps its group for the Update or Enqueue that starts it again,
// unless the call names another
func TestAJobStartedAgainKeepsItsGroup(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(1), MaxAttempts(1))
	mustEnqueue(t, refreshes, "failing", Key("r:1"), Group("db:a"))
	if err := mustClaim(t, refreshes).Fail(t.Context(), errors.New("no such table")); err != nil {
		t.Fatal(err)
	}
	mustEnqueue(t, refreshes, "running", Key("r:2"), Group("db:a"))
	running := mustClaim(t, refreshes)

	if err := refreshes.Update(t.Context(), "r:1", "fixed"); err != nil {
		t.Fatal(err)
	}
	nothingDue(t, refreshes)
	if err := refreshes.Update(t.Context(), "r:1", "elsewhere", Group("db:b")); err != nil {
		t.Fatal(err)
	}
	if job := mustClaim(t, refreshes); job.Value != "elsewhere" {
		t.Fatalf("the claim took %s", job.Value)
	}
	if err := running.Ack(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := refreshes.Enqueue(t.Context(), "x", Group("")); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("an empty group: %v", err)
	}
}

// Work runs no more of a group's jobs at once than its bound, across its
// workers, and runs every job
func TestWorkRunsAGroupsJobsWithinItsBound(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	refreshes := openTestQueue[string](t, queues, "refreshes", MaxRunningInGroup(2))
	for i := range 40 {
		mustEnqueue(t, refreshes, fmt.Sprintf("db:%d", i%3), Group(fmt.Sprintf("db:%d", i%3)))
	}
	var mu sync.Mutex
	running, most := map[string]int{}, map[string]int{}
	ran := 0
	err := refreshes.Work(t.Context(), func(_ context.Context, job Job[string]) error {
		mu.Lock()
		running[job.Value]++
		most[job.Value] = max(most[job.Value], running[job.Value])
		mu.Unlock()
		time.Sleep(2 * time.Millisecond)
		mu.Lock()
		running[job.Value]--
		ran++
		mu.Unlock()
		return nil
	}, Workers(8), UntilIdle())
	if err != nil {
		t.Fatal(err)
	}
	if ran != 40 || most["db:0"] > 2 || most["db:1"] > 2 || most["db:2"] > 2 {
		t.Fatalf("%d jobs ran, at most %v at once a group", ran, most)
	}
}

// the parked index answers the query for a group's first parked job, which
// names its bound in the same digits
func TestAGroupsParkedJobsAreFoundByTheirIndex(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	if parkedFromText != fmt.Sprint(parkedFrom) || parkedBy != 2*parkedFrom {
		t.Fatalf("the parked bound %s is not %d", parkedFromText, parkedFrom)
	}
	var plan []string
	err := queues.file.Lookup(t.Context(), func(r sqlite.Reader) error {
		rows, err := r.QueryContext(t.Context(), `explain query plan `+firstParked, 1, "db:a") //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "plan", func(rows *sql.Rows) error {
			var id, parent, unused int
			var detail string
			scanErr := rows.Scan(&id, &parent, &unused, &detail)
			plan = append(plan, detail)
			return scanErr
		})
	})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(strings.Join(plan, "\n"), "_tinystore_jobs_parked") {
		t.Fatalf("the query for a parked job is planned as %q", plan)
	}
}
