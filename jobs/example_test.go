package jobs_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"
	_ "time/tzdata" // the zone of the schedules, whatever the host has

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/jobs"
)

// clock is the store's clock in the examples, moved rather than waited for
type clock struct{ now time.Time }

func (c *clock) Now() time.Time { return c.now }

func (c *clock) advance(d time.Duration) { c.now = c.now.Add(d) }

// exampleQueues opens jobs in a Manual store in a new directory, at noon UTC,
// so that an example moves its clock instead of sleeping and runs its jobs with
// UntilIdle; done closes and removes it
func exampleQueues() (queues *jobs.Store, at *clock, done func()) {
	ctx := context.Background()
	directory, err := os.MkdirTemp("", "tinystore-jobs-")
	check(err)
	at = &clock{now: time.Date(2026, 9, 27, 12, 0, 0, 0, time.UTC)}
	store, err := tinystore.Open(ctx, directory, tinystore.Options{Manual: true, Clock: at.Now})
	check(err)
	queues, err = jobs.Open(ctx, store, jobs.Options{})
	check(err)
	return queues, at, func() {
		_ = store.Close(ctx)
		_ = os.RemoveAll(directory)
	}
}

// check ends an example that meets an error
func check(err error) {
	if err != nil {
		panic(err)
	}
}

type Reminder struct {
	User int64
	Text string
}

// A reminder at six has no key and nothing to find again: Work runs it at its
// time, and a handler's nil acknowledges it.
func Example_reminder() {
	ctx := context.Background()
	queues, clock, done := exampleQueues()
	defer done()
	reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
	check(err)

	evening := time.Date(2026, 9, 27, 18, 0, 0, 0, time.UTC)
	check(reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening)))

	remind := func(_ context.Context, job jobs.Job[Reminder]) error {
		fmt.Println(job.At.UTC().Format("15:04"), "remind", job.Value.User, "to", job.Value.Text)
		return nil
	}
	check(reminders.Work(ctx, remind, jobs.UntilIdle())) // noon: nothing is due
	clock.advance(6 * time.Hour)
	check(reminders.Work(ctx, remind, jobs.UntilIdle()))
	// Output:
	// 18:00 remind 42 to call mom
}

type Draft struct {
	ID, Text string
}

// A message sent later is the job itself until it is sent. The client makes
// its id, so a second tap on send adds nothing; the chat lists what it has
// scheduled, and a message is edited or cancelled until it goes.
func Example_sendLater() {
	ctx := context.Background()
	queues, clock, done := exampleQueues()
	defer done()
	later, err := jobs.OpenQueue[Draft](ctx, queues, "send-later")
	check(err)

	nine, ten := clock.now.Add(21*time.Hour), clock.now.Add(22*time.Hour)
	for _, draft := range []Draft{{"m1", "happy birthday"}, {"m2", "see you"}, {"m1", "happy birthday"}} {
		check(later.Enqueue(ctx, draft, jobs.At(nine), jobs.Key("chat:42:"+draft.ID)))
	}
	page, err := later.Scan(ctx, jobs.Query{Prefix: "chat:42:"})
	check(err)
	fmt.Println(len(page.Entries), "scheduled messages")

	check(later.Update(ctx, "chat:42:m1", Draft{"m1", "happy birthday!"}, jobs.At(ten)))
	cancelled, err := later.Cancel(ctx, "chat:42:m2")
	check(err)
	fmt.Println("m2 cancelled:", cancelled)

	clock.advance(22 * time.Hour)
	check(later.Work(ctx, func(_ context.Context, job jobs.Job[Draft]) error {
		fmt.Println(job.At.UTC().Format("15:04"), "sent:", job.Value.Text) // insert … on conflict (id) do nothing
		return nil
	}, jobs.UntilIdle()))

	err = later.Update(ctx, "chat:42:m1", Draft{"m1", "too late"})
	fmt.Println("editing a sent message:", errors.Is(err, tinystore.ErrConflict))
	// Output:
	// 2 scheduled messages
	// m2 cancelled: true
	// 10:00 sent: happy birthday!
	// editing a sent message: true
}

type Push struct {
	User, Message string
}

// A push to each member is a job a member, enqueued in one transaction and
// keyed by the message and the member, in a queue that remembers a key done
// for an hour, so that a message sent twice pushes nobody twice. A provider
// that asks to wait is snoozed, which counts no attempt, and a token it no
// longer knows fails for good.
func Example_pushes() {
	ctx := context.Background()
	queues, clock, done := exampleQueues()
	defer done()
	pushes, err := jobs.OpenQueue[Push](ctx, queues, "pushes", jobs.KeepDone(time.Hour))
	check(err)

	notify := func(message string, members ...string) error {
		return queues.Tx(ctx, func(tx *jobs.Tx) error {
			for _, member := range members {
				push := Push{User: member, Message: message}
				if err := pushes.WithTx(tx).Enqueue(ctx, push, jobs.Key(message+":"+member)); err != nil {
					return err
				}
			}
			return nil
		})
	}
	busy := true
	push := func(ctx context.Context, job jobs.Job[Push]) error {
		switch {
		case job.Value.User == "eve":
			return job.Fail(ctx, errors.New("the provider no longer knows the token"))
		case job.Value.User == "bob" && busy:
			busy = false
			return job.Snooze(ctx, jobs.After(time.Minute))
		}
		fmt.Println("pushed", job.Value.Message, "to", job.Value.User)
		return nil
	}

	check(notify("m1", "ann", "bob", "eve"))
	check(pushes.Work(ctx, push, jobs.UntilIdle()))
	check(notify("m1", "ann", "bob")) // the message sent twice: ann's push ran within the hour
	clock.advance(time.Minute)
	check(pushes.Work(ctx, push, jobs.UntilIdle()))

	failed, err := pushes.Scan(ctx, jobs.Query{State: jobs.Failed})
	check(err)
	for _, entry := range failed.Entries {
		fmt.Println("failed:", entry.Key, "-", entry.Err)
	}
	// Output:
	// pushed m1 to ann
	// pushed m1 to bob
	// failed: m1:eve - the provider no longer knows the token
}

// An account deleted in thirty days is a row of the application, which the
// settings page reads and "cancel" clears; the job is only its alarm. The alarm
// is enqueued before the row is written, and the handler does what the row
// says: nothing when the row is gone, a snooze when it says later.
func Example_accountDeletion() {
	ctx := context.Background()
	queues, clock, done := exampleQueues()
	defer done()
	deletions, err := jobs.OpenQueue[int64](ctx, queues, "delete-accounts")
	check(err)
	deleteAt := map[int64]time.Time{} // the application's users.delete_at

	for _, user := range []int64{42, 7} {
		at := clock.now.Add(30 * 24 * time.Hour)
		check(deletions.Enqueue(ctx, user, jobs.At(at), jobs.Key(fmt.Sprint("user:", user)))) // the alarm first
		deleteAt[user] = at
	}
	deleteAt[42] = deleteAt[42].Add(24 * time.Hour) // a day more to download their photos
	delete(deleteAt, 7)                             // changed their mind

	purge := func(ctx context.Context, job jobs.Job[int64]) error {
		at, pending := deleteAt[job.Value]
		switch {
		case !pending:
			fmt.Println("user", job.Value, "stays")
			return nil
		case at.After(clock.Now()):
			fmt.Println("user", job.Value, "waits for", at.Format("Jan 2"))
			return job.Snooze(ctx, jobs.At(at))
		}
		delete(deleteAt, job.Value)
		fmt.Println("user", job.Value, "deleted")
		return nil
	}
	clock.advance(30 * 24 * time.Hour)
	check(deletions.Work(ctx, purge, jobs.UntilIdle()))
	clock.advance(24 * time.Hour)
	check(deletions.Work(ctx, purge, jobs.UntilIdle()))
	// Output:
	// user 42 waits for Oct 28
	// user 7 stays
	// user 42 deleted
}

// A purge every night is a schedule, a queue whose one job repeats; a digest a
// user is a job enqueued with a repeat under its own key. Each moves to its
// next time when it is acknowledged, and a program away for a day runs it once.
func Example_schedules() {
	ctx := context.Background()
	queues, clock, done := exampleQueues()
	defer done()
	moscow, err := time.LoadLocation("Europe/Moscow")
	check(err)
	purge, err := jobs.OpenSchedule(ctx, queues, "purge-deleted", jobs.Daily("03:10", moscow))
	check(err)
	digests, err := jobs.OpenQueue[string](ctx, queues, "digests")
	check(err)
	check(digests.Enqueue(ctx, "ann", jobs.Key("user:ann"), jobs.Daily("20:00", moscow)))

	for range 2 {
		clock.advance(24 * time.Hour)
		check(purge.Work(ctx, func(_ context.Context, job jobs.Job[struct{}]) error {
			fmt.Println("purge of", job.At.In(moscow).Format("Jan 2 15:04"))
			return nil
		}, jobs.UntilIdle()))
		check(digests.Work(ctx, func(_ context.Context, job jobs.Job[string]) error {
			fmt.Println("digest for", job.Value, "of", job.At.In(moscow).Format("Jan 2 15:04"))
			return nil
		}, jobs.UntilIdle()))
	}
	// Output:
	// purge of Sep 28 03:10
	// digest for ann of Sep 27 20:00
	// purge of Sep 29 03:10
	// digest for ann of Sep 28 20:00
}
