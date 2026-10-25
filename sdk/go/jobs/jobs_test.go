package jobs_test

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/jobs"
	"tinyshed.org/tinystore/kv"
)

type Reminder struct {
	UserID int    `json:"userId"`
	Text   string `json:"text"`
}

type Text struct {
	Text string `json:"text"`
}

func TestAnIDAddsAJobOnceSetMakesItWhateverItWasUpdateAndCancel(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		later := ways.Must(jobs.OpenQueue[Text](store, "later"))
		id := "chat:42:draft-1"
		if !ways.Must(later.Add(ctx, Text{"hi"}, jobs.ID(id), jobs.Delay(time.Hour))) {
			t.Fatal("the first add adds")
		}
		if ways.Must(later.Add(ctx, Text{"hi again"}, jobs.ID(id), jobs.Delay(time.Hour))) {
			t.Fatal("an id taken adds nothing")
		}
		added, found := ways.Must2(later.Get(ctx, id))
		if !found || added.State != jobs.StateScheduled || added.Value.Text != "hi" || added.Attempt != 0 {
			t.Fatalf("added %+v", added)
		}
		ways.Must(0, later.Set(ctx, id, Text{"edited"}, jobs.Delay(2*time.Hour)))
		if edited, _ := ways.Must2(later.Get(ctx, id)); edited.Value.Text != "edited" {
			t.Fatalf("edited %+v", edited)
		}
		if !ways.Must(later.Update(ctx, id, Text{"edited twice"})) {
			t.Fatal("an update of a job not started changes it")
		}
		if !ways.Must(later.Cancel(ctx, id)) {
			t.Fatal("a cancel takes the job")
		}
		if _, found := ways.Must2(later.Get(ctx, id)); found {
			t.Fatal("a cancelled job is gone")
		}
		if ways.Must(later.Update(ctx, id, Text{"too late"})) || ways.Must(later.Cancel(ctx, id)) {
			t.Fatal("an id with no job changes nothing")
		}
	})
}

// working runs a worker in the background, stopped by the returned function
// once its handlers under way answered.
func working[T any](t *testing.T, q *jobs.Queue[T], handler func(ctx context.Context, value T, run *jobs.Run) error, opts ...jobs.WorkOption) func() {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() { done <- q.Work(ctx, handler, opts...) }()
	return func() {
		cancel()
		if err := <-done; err != nil {
			t.Errorf("work: %v", err)
		}
	}
}

func TestAWorkerRunsEachJobAsItFallsDueTheValueFirstAndTheRunSecond(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		reminders := ways.Must(jobs.OpenQueue[Reminder](store, "reminders"))
		var mu sync.Mutex
		var ran []string
		stop := working(t, reminders, func(ctx context.Context, r Reminder, run *jobs.Run) error {
			mu.Lock()
			defer mu.Unlock()
			ran = append(ran, r.Text+"|"+run.ID+"|"+string(rune('0'+run.Attempt)))
			return nil
		})
		ways.Must(reminders.Add(ctx, Reminder{1, "Call mom"}, jobs.ID("r1")))
		ways.Must(reminders.Add(ctx, Reminder{2, "Drink water"}))
		ways.Until(t, func() bool { mu.Lock(); defer mu.Unlock(); return len(ran) == 2 })
		stop()
		if ran[0] != "Call mom|r1|1" || ran[1] != "Drink water||1" {
			t.Fatalf("ran %v", ran)
		}
		if _, found := ways.Must2(reminders.Get(ctx, "r1")); found {
			t.Fatal("a done job is gone")
		}
	})
}

func TestWhatAHandlerReturnsSettlesItsJob(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pushes := ways.Must(jobs.OpenQueue[string](store, "pushes", jobs.Backoff{Initial: time.Hour, Max: 2 * time.Hour}))
		for _, how := range []string{"done", "raised", "retry", "snooze", "fail"} {
			ways.Must(pushes.Add(ctx, how, jobs.ID(how)))
		}
		var ran atomic.Int32
		ways.Must(0, pushes.RunDue(ctx, func(ctx context.Context, how string, run *jobs.Run) error {
			ran.Add(1)
			switch how {
			case "raised":
				return errors.New("the provider is down")
			case "retry":
				return run.RetryAfter(10 * time.Minute)
			case "snooze":
				return run.Snooze(10 * time.Minute)
			case "fail":
				return run.Fail("the provider no longer knows the token")
			}
			return nil
		}))
		if ran.Load() != 5 {
			t.Fatalf("ran %d", ran.Load())
		}
		if _, found := ways.Must2(pushes.Get(ctx, "done")); found {
			t.Fatal("done is gone")
		}
		raised, _ := ways.Must2(pushes.Get(ctx, "raised"))
		if raised.State != jobs.StateScheduled || raised.Attempt != 1 || raised.Error != "the provider is down" {
			t.Fatalf("raised %+v", raised)
		}
		if retry, _ := ways.Must2(pushes.Get(ctx, "retry")); retry.State != jobs.StateScheduled || retry.Attempt != 1 {
			t.Fatalf("retry %+v", retry)
		}
		if snooze, _ := ways.Must2(pushes.Get(ctx, "snooze")); snooze.State != jobs.StateScheduled || snooze.Attempt != 0 {
			t.Fatalf("snooze %+v", snooze)
		}
		if fail, _ := ways.Must2(pushes.Get(ctx, "fail")); fail.State != jobs.StateFailed || fail.Error != "the provider no longer knows the token" {
			t.Fatalf("fail %+v", fail)
		}
	})
}

func TestAStepOneAttemptKeptIsFoundByTheNext(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		orders := ways.Must(jobs.OpenQueue[map[string]int](store, "orders"))
		ways.Must(orders.Add(ctx, map[string]int{"order": 7}, jobs.ID("order:7")))
		var charged atomic.Int32
		var attempts []string
		handle := func(ctx context.Context, _ map[string]int, run *jobs.Run) error {
			kept, err := jobs.Step(ctx, run, "charge", func(context.Context) (map[string]string, error) {
				charged.Add(1)
				return map[string]string{"charge": "ch_1"}, nil
			})
			if err != nil {
				return err
			}
			attempts = append(attempts, string(rune('0'+run.Attempt))+":"+kept["charge"])
			if run.Attempt == 1 {
				return run.RetryAt(time.Unix(0, 0))
			}
			return nil
		}
		ways.Must(0, orders.RunDue(ctx, handle))
		if strings.Join(attempts, ",") != "1:ch_1,2:ch_1" || charged.Load() != 1 {
			t.Fatalf("attempts %v, charged %d", attempts, charged.Load())
		}
	})
}

func TestWhatAHandlerReportsShowsWhileItsJobRunsAndACancelEndsItsCtx(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		videos := ways.Must(jobs.OpenQueue[map[string]int](store, "videos"))
		ways.Must(videos.Add(ctx, map[string]int{"video": 3}, jobs.ID("video:3")))
		released := make(chan struct{})
		stop := working(t, videos, func(ctx context.Context, _ map[string]int, run *jobs.Run) error {
			run.SetProgress(map[string]int{"frames": 120})
			<-released
			return nil
		})
		ways.Until(t, func() bool {
			job, _, _ := videos.Get(ctx, "video:3")
			return job.Progress != nil
		})
		running, _ := ways.Must2(videos.Get(ctx, "video:3"))
		if running.State != jobs.StateRunning || string(running.Progress) != `{"frames":120}` {
			t.Fatalf("running %+v %s", running, running.Progress)
		}
		close(released)
		stop()

		exports := ways.Must(jobs.OpenQueue[map[string]int](store, "exports"))
		ways.Must(exports.Add(ctx, map[string]int{"report": 9}, jobs.ID("export:9")))
		var ended atomic.Bool
		stop = working(t, exports, func(ctx context.Context, _ map[string]int, run *jobs.Run) error {
			<-ctx.Done()
			ended.Store(true)
			return run.Fail("it should settle nothing")
		})
		ways.Until(t, func() bool {
			job, _, _ := exports.Get(ctx, "export:9")
			return job.State == jobs.StateRunning
		})
		if !ways.Must(exports.Cancel(ctx, "export:9")) {
			t.Fatal("a cancel takes a running job")
		}
		ways.Until(t, ended.Load)
		stop()
		if _, found := ways.Must2(exports.Get(ctx, "export:9")); found {
			t.Fatal("what a cancelled handler returns settles nothing")
		}
	})
}

func TestAWatchFollowsItsJobToItsEnd(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		clips := ways.Must(jobs.OpenQueue[map[string]int](store, "clips"))
		ways.Must(clips.Add(ctx, map[string]int{"clip": 1}, jobs.ID("clip:1")))
		var mu sync.Mutex
		var seen []string
		watched := make(chan struct{})
		go func() {
			defer close(watched)
			for job, err := range clips.Watch(ctx, "clip:1") {
				if err != nil {
					t.Error(err)
					return
				}
				mu.Lock()
				seen = append(seen, string(job.State)+" "+string(job.Progress))
				mu.Unlock()
			}
		}()
		ways.Until(t, func() bool { mu.Lock(); defer mu.Unlock(); return len(seen) > 0 })
		released := make(chan struct{})
		stop := working(t, clips, func(ctx context.Context, _ map[string]int, run *jobs.Run) error {
			run.SetProgress(0.5)
			<-released
			return nil
		})
		ways.Until(t, func() bool { mu.Lock(); defer mu.Unlock(); return seen[len(seen)-1] == "running 0.5" })
		close(released)
		<-watched
		stop()
		if seen[0] != "waiting " || seen[len(seen)-1] != "done 0.5" {
			t.Fatalf("seen %q", seen)
		}
		for range clips.Watch(ctx, "clip:never") {
			t.Fatal("an id with no job yields nothing")
		}
		ways.Must(clips.Add(ctx, map[string]int{"clip": 2}, jobs.ID("clip:2"), jobs.Delay(time.Hour)))
		var states []jobs.State
		for job, err := range clips.Watch(ctx, "clip:2") {
			if err != nil {
				t.Fatal(err)
			}
			states = append(states, job.State)
			if job.State == jobs.StateScheduled {
				ways.Must(clips.Cancel(ctx, "clip:2"))
			}
		}
		if len(states) != 2 || states[1] != jobs.StateCancelled {
			t.Fatalf("states %v", states)
		}
	})
}

func TestAScheduleKeepsItsOneJobAndACronNeedsATimeZone(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		nightly := ways.Must(jobs.OpenSchedule(store, "nightly", jobs.Cron("10 3 * * *", "Europe/Berlin")))
		working, cancel := context.WithCancel(ctx)
		done := make(chan error, 1)
		go func() {
			done <- nightly.Work(working, func(context.Context, *jobs.Run) error { return nil })
		}()
		ways.Until(t, func() bool { _, err := nightly.Get(ctx); return err == nil })
		job := ways.Must(nightly.Get(ctx))
		if job.State != jobs.StateScheduled || job.Repeat != "10 3 * * * Europe/Berlin" {
			t.Fatalf("job %+v", job)
		}
		cancel()
		ways.Must(0, <-done)
		if !ways.Must(nightly.Cancel(ctx)) {
			t.Fatal("cancel removes the schedule's job")
		}
		_, err := jobs.OpenSchedule(store, "digest", jobs.Cron("0 20 * * *", ""))
		if !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("got %v", err)
		}
		_, err = jobs.OpenSchedule(store, "neither")
		if !errors.Is(err, tinystore.ErrInvalid) {
			t.Fatalf("got %v", err)
		}
	})
}

func TestAPageReadsTheIDsUnderAPrefix(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		chats := ways.Must(jobs.OpenQueue[int](store, "chats"))
		for _, n := range []string{"1", "2", "3"} {
			ways.Must(chats.Add(ctx, 1, jobs.ID("chat:4:"+n), jobs.Delay(time.Hour)))
		}
		ways.Must(chats.Add(ctx, 42, jobs.ID("chat:42:1"), jobs.Delay(time.Hour)))
		first := ways.Must(chats.List(ctx, jobs.Prefix("chat:4:"), jobs.Limit(2)))
		if len(first.Jobs) != 2 || first.Jobs[0].ID != "chat:4:1" {
			t.Fatalf("first %+v", first)
		}
		second := ways.Must(chats.List(ctx, jobs.Prefix("chat:4:"), jobs.Limit(2), jobs.After(first.Next)))
		if len(second.Jobs) != 1 || second.Jobs[0].ID != "chat:4:3" || second.Next != "" {
			t.Fatalf("second %+v", second)
		}
		var ids []string
		for job, err := range chats.All(ctx, jobs.Prefix("chat:")) {
			if err != nil {
				t.Fatal(err)
			}
			ids = append(ids, job.ID)
		}
		// byte order: "2" is 0x32, before ":" at 0x3a
		if strings.Join(ids, ",") != "chat:42:1,chat:4:1,chat:4:2,chat:4:3" {
			t.Fatalf("ids %v", ids)
		}
	})
}

func TestAGroupRateStartsEachGroupsCountASpanAndHoldsBackNoOther(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		type message struct {
			Bot string
			N   int
		}
		messages := ways.Must(jobs.OpenQueue[message](store, "messages", jobs.GroupRate(2, time.Minute)))
		for _, m := range []message{{"a", 0}, {"a", 1}, {"a", 2}, {"b", 0}, {"b", 1}} {
			ways.Must(messages.Add(ctx, m, jobs.Group(m.Bot)))
		}
		var sent []string
		send := func(ctx context.Context, m message, run *jobs.Run) error {
			sent = append(sent, fmt.Sprintf("%s%d", m.Bot, m.N))
			return nil
		}
		ways.Must(0, messages.RunDue(ctx, send))
		if want := []string{"a0", "a1", "b0", "b1"}; !slices.Equal(sent, want) {
			t.Fatalf("sent %v in the first span, not %v", sent, want)
		}
		ways.Must(0, messages.RunDue(ctx, send))
		if len(sent) != 4 {
			t.Fatalf("sent %v, more within the span", sent)
		}
	})
}

func TestAValueThatNoLongerReadsFailsItsJobNotItsQueue(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		loose := ways.Must(jobs.OpenQueue[map[string]any](store, "checked"))
		ways.Must(loose.Add(ctx, map[string]any{"userId": "not a number"}, jobs.ID("bad")))
		checked := ways.Must(jobs.OpenQueue[Reminder](store, "checked"))
		ways.Must(checked.Add(ctx, Reminder{5, "Call mom"}, jobs.ID("good")))
		var got []string
		ways.Must(0, checked.RunDue(ctx, func(ctx context.Context, r Reminder, run *jobs.Run) error {
			got = append(got, r.Text)
			return nil
		}))
		if len(got) != 1 || got[0] != "Call mom" {
			t.Fatalf("got %v", got)
		}
		bad, _ := ways.Must2(loose.Get(ctx, "bad"))
		if bad.State != jobs.StateFailed || !strings.Contains(bad.Error, "no longer reads") {
			t.Fatalf("bad %+v", bad)
		}
	})
}

func TestAWorkerRunsAsManyHandlersAtOnceAsItsConcurrencySays(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		refreshes := ways.Must(jobs.OpenQueue[int](store, "refreshes"))
		for n := range 4 {
			ways.Must(refreshes.Add(ctx, n))
		}
		var running, most, finished atomic.Int32
		stop := working(t, refreshes, func(ctx context.Context, _ int, run *jobs.Run) error {
			now := running.Add(1)
			for {
				was := most.Load()
				if now <= was || most.CompareAndSwap(was, now) {
					break
				}
			}
			time.Sleep(50 * time.Millisecond)
			running.Add(-1)
			finished.Add(1)
			return nil
		}, jobs.Concurrency{Total: 2})
		ways.Until(t, func() bool { return finished.Load() == 4 })
		stop()
		if most.Load() != 2 {
			t.Fatalf("most at once %d", most.Load())
		}
	})
}

func TestDedupeKeepsADoneIDTakenAndTheJobItsLastRun(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		welcome := ways.Must(jobs.OpenQueue[string](store, "welcome", jobs.Dedupe(time.Hour)))
		if !ways.Must(welcome.Add(ctx, "ada", jobs.ID("welcome:ada"))) {
			t.Fatal("the first add adds")
		}
		ways.Must(0, welcome.RunDue(ctx, func(context.Context, string, *jobs.Run) error { return nil }))
		if ways.Must(welcome.Add(ctx, "ada", jobs.ID("welcome:ada"))) {
			t.Fatal("a done id stays taken")
		}
		done, _ := ways.Must2(welcome.Get(ctx, "welcome:ada"))
		if done.State != jobs.StateDone || done.Value != "ada" || done.LastRun == nil || done.LastRun.EndedAt.Before(done.LastRun.StartedAt) {
			t.Fatalf("done %+v", done)
		}
	})
}

func TestATransactionOfTheStoreMakesItsJobsTogetherOrNone(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		fanout := ways.Must(jobs.OpenQueue[map[string]string](store, "fanout"))
		ways.Must(0, store.Tx(ctx, func(tx *tinystore.Tx) error {
			for _, member := range []string{"ann", "bob"} {
				if _, err := fanout.In(tx).Add(ctx, map[string]string{"member": member}, jobs.ID(member)); err != nil {
					return err
				}
			}
			_, err := fanout.In(tx).Cancel(ctx, "bob")
			return err
		}))
		if ann, _ := ways.Must2(fanout.Get(ctx, "ann")); ann.Value["member"] != "ann" {
			t.Fatalf("ann %+v", ann)
		}
		if _, found := ways.Must2(fanout.Get(ctx, "bob")); found {
			t.Fatal("bob was cancelled in the same commit")
		}
		err := store.Tx(ctx, func(tx *tinystore.Tx) error {
			if _, err := fanout.In(tx).Add(ctx, map[string]string{"member": "eve"}, jobs.ID("eve")); err != nil {
				return err
			}
			return fanout.In(tx).Set(ctx, "", map[string]string{"member": "nobody"})
		})
		var e *tinystore.Error
		if !errors.As(err, &e) || e.Kind != tinystore.Invalid || e.What["write"] != "1" {
			t.Fatalf("got %v", err)
		}
		if _, found := ways.Must2(fanout.Get(ctx, "eve")); found {
			t.Fatal("a refused write takes the others with it")
		}
		seen := ways.Must(kv.OpenBucket[bool](store, "seen"))
		err = store.Tx(ctx, func(tx *tinystore.Tx) error {
			if _, err := fanout.In(tx).Add(ctx, map[string]string{"member": "zed"}, jobs.ID("zed")); err != nil {
				return err
			}
			return seen.In(tx).Set(ctx, "zed", true)
		})
		if !errors.Is(err, tinystore.ErrInvalid) || !strings.Contains(err.Error(), "kept in kv.db, outside this transaction of jobs.db") {
			t.Fatalf("got %v", err)
		}
	})
}

func TestRunDueRunsWhatTheTestsClockMadeDue(t *testing.T) {
	if ways.Binary() == "" {
		t.Skip("tinystore is not built here")
	}
	defer ways.Recover(t)
	ctx := context.Background()
	start := time.Date(2026, 10, 9, 9, 0, 0, 0, time.UTC)
	store := ways.Must(tinystore.Open(ctx, t.TempDir(), tinystore.Private(), tinystore.Binary(ways.Binary()), tinystore.Clock(start)))
	defer store.Close()
	reminders := ways.Must(jobs.OpenQueue[Reminder](store, "reminders"))
	ways.Must(reminders.Add(ctx, Reminder{42, "Call mom"}, jobs.Delay(time.Hour)))
	var ran []time.Time
	remind := func(ctx context.Context, _ Reminder, run *jobs.Run) error {
		ran = append(ran, run.At)
		return nil
	}
	ways.Must(0, reminders.RunDue(ctx, remind))
	if len(ran) != 0 {
		t.Fatal("nothing is due yet")
	}
	ways.Must(0, store.Clock().Advance(ctx, time.Hour))
	ways.Must(0, reminders.RunDue(ctx, remind))
	if len(ran) != 1 || !ran[0].Equal(start.Add(time.Hour)) {
		t.Fatalf("ran %v", ran)
	}
}
