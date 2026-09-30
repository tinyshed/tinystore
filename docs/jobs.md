# Jobs: work that runs at its time

The design of the jobs engine, built in its first slice: `jobs/` holds the API
and the contracts below. What lies under [Storage](#storage) was settled by
[the mechanics round](https://github.com/tinyshed/research/blob/main/tinystore/reports/jobs-mechanics-2026-09-27.md) in a container on
one development machine; what the round left is under [Open](#open).

## What it is for

Work a process must do later, or now but outside the request that asked for
it: a message sent at nine, a reminder at six, a push to each member of a
group, an account deleted thirty days after its owner asked, a purge every
night. Today each is a table with a status column that a loop polls every few
minutes, or a Redis beside the program that the program must also keep.

- **One process's work.** One store owns the directory, so the process that
  holds it claims the jobs of `jobs.db`. A worker in another process or
  language reaches them through a server of its own that speaks
  [the protocol](#the-same-in-another-language); the embedded API is the same
  protocol as calls.
- **Not a log, state or a workflow.** What happened is the records engine's,
  current state kv's and sqldb's. A job is one unit of work that runs until it
  is acknowledged; steps that wait on each other are the application's.
- **Its own engine.** `jobs.db` has its own writer, schema and code; the
  package imports the root and `internal/`, never kv or sqldb, and no write
  spans two files. A store opened `In` an sqldb database keeps its tables in
  that file instead, so that a job commits in the database's `Batch` with the
  rows it is about: the file's writer is then the database's and the queue's.

## The public surface

```go
queues, err := jobs.Open(ctx, store, jobs.Options{}) // data/jobs.db

reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
later, err := jobs.OpenQueue[Draft](ctx, queues, "send-later", jobs.MaxAttempts(20))
purge, err := jobs.OpenSchedule(ctx, queues, "purge-deleted", jobs.Daily("03:10", moscow))

err = reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening))

go reminders.Work(ctx, a.remind, jobs.Workers(4)) // until ctx ends or the store closes
```

- **The simplest job has no name.** `Enqueue` takes a value and when to run
  it, and returns only an error: a reminder at six needs nothing else.
- **A type is declared once.** `OpenQueue[V]` fixes the value's type; a
  queue's name and kind are kept in `jobs.db` and checked when it opens. Its
  options are the program's and may change between runs.
- **Enqueuing and working are two acts.** A queue opens without a handler: one
  part of a program enqueues, `Work` runs a handler on each job as it falls
  due, and `Claim` hands one job to whoever works it by hand, or to a worker in
  another language through a server.
- **A job in the queue is an `Entry`; a job in a worker's hands is a `Job`.**
  Only a `Job` can be acknowledged, so the compiler refuses to settle what the
  caller never claimed.

```go
// Queue[V]: enqueuing and finding
Enqueue(ctx, value, opts...) error                     // jobs.At, jobs.After, jobs.Key, a repeat
Update(ctx, key, value, opts...) error                  // a job that still waits takes a new value or time
Cancel(ctx, key) (bool, error)                          // true: removed before it ran
Get(ctx, key) (jobs.Entry[V], bool, error)
Scan(ctx, jobs.Query) (jobs.Page[V], error)             // keys under a prefix, or the failed jobs
All(ctx, jobs.Query) iter.Seq2[jobs.Entry[V], error]   // the same, a page at a time
WithTx(tx *jobs.Tx) *jobs.Queue[V]                      // inside queues.Tx: several enqueues, one commit

// Queue[V]: working
Work(ctx, handle, opts...) error                        // jobs.Workers, jobs.Timeout, jobs.UntilIdle
Claim(ctx, opts...) (jobs.Job[V], bool, error)          // one job leased to the caller, or none due

// Job[V]: settling a claimed job
Ack(ctx) error                                          // done: gone from the queue
Retry(ctx, err, opts...) error                          // an attempt failed: again later, or failed for good
Fail(ctx, err) error                                    // failed for good, no more attempts
Snooze(ctx, opts...) error                              // not yet: again at jobs.At or jobs.After, no attempt counted
Extend(ctx, d) error                                    // the lease runs d from now
```

## The same in another language

A server that serves `jobs.db` to other programs is a module of its own and is
not built; the embedded API is shaped so that it can be. Every call above is
one operation of the protocol, and what is not an operation, `Work` and `All`,
is a loop over operations that any SDK repeats.

```text
enqueue   queue, [value, at?, key?, repeat?]…       → ok
update    queue, key, value, at?, repeat?           → ok | conflict
cancel    queue, key                                → cancelled
get       queue, key                                → entry?
scan      queue, prefix? | state, after?, limit     → page
claim     queue, max, lease?, wait?                 → jobs, each with its lease
extend    job, lease, for                           → until
ack       [job, lease]…                             → ok | conflict
retry     job, lease, error, at?                    → ok | conflict
fail      job, lease, error                         → ok | conflict
snooze    job, lease, at                            → ok | conflict
```

- **Nothing that cannot travel.** A value is JSON, a key is text, a time is
  unix milliseconds, a repeat is the text of a cron expression and a zone's
  name. No callback is stored and no Go type is on the wire.
- **Enqueues, claims and acks carry many.** A remote worker claims up to `max`
  jobs and acknowledges several at once, and a program enqueues a batch in one
  call, as `Tx` does here, or the protocol pays a commit a job.

```ts
const reminders = store.jobs.queue<Reminder>("reminders")
await reminders.enqueue({ user: 42, text: "call mom" }, { at: evening })

await reminders.work(async job => { await remind(job.value) }, { workers: 4 })

const job = await reminders.claim()
if (job) {
  try { await remind(job.value); await job.ack() }
  catch (err) { await job.retry(err) }
}
```

## Five cases

They are the API's examples, the gates' workloads and the round's.

**A reminder at six.** No key, nothing to find again.

```go
reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
err = reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening))

func (a *app) remind(ctx context.Context, job jobs.Job[Reminder]) error {
	return a.push(ctx, job.Value.User, job.Value.Text) // nil acknowledges, an error retries
}
```

**Messages sent later: the job is the data.** A scheduled message is not yet a
message; it lives in the queue until it is sent and becomes a row of the
application's `messages` then. The client makes the message's id, so a second
tap on send adds nothing.

```go
later, err := jobs.OpenQueue[Draft](ctx, queues, "send-later")

key := "chat:42:" + draft.ID
err = later.Enqueue(ctx, draft, jobs.At(nine), jobs.Key(key))
page, err := later.Scan(ctx, jobs.Query{Prefix: "chat:42:"})  // "3 scheduled messages"
err = later.Update(ctx, key, edited, jobs.At(ten))            // ErrConflict: it is being sent, or was
cancelled, err := later.Cancel(ctx, key)                       // false: too late

func (a *app) sendLater(ctx context.Context, job jobs.Job[Draft]) error {
	d := job.Value
	_, err := a.db.Exec(ctx, `insert into messages (id, chat, author, text, sent_at)
		values (?, ?, ?, ?, ?) on conflict (id) do nothing`, d.ID, d.Chat, d.Author, d.Text, job.At.UnixMilli())
	return err // run twice after a crash, it inserts once
}
```

`jobs.db` is as durable as the application's database, the same SQLite with
the same fsync and the same backup, so a scheduled message is as safe in the
queue as in a table, and no write spans two files.

**A push to each member.** One job a member, keyed by the message and the
member, in a queue that remembers a done key for an hour, so that a
`sendLater` run twice pushes nobody twice; one transaction for all of them. A
provider that asks to wait is answered with `Snooze`, which counts no attempt;
a token the provider no longer knows fails for good.

```go
pushes, err := jobs.OpenQueue[Push](ctx, queues, "pushes", jobs.KeepDone(time.Hour))

err = a.queues.Tx(ctx, func(tx *jobs.Tx) error {
	for _, member := range members {
		push := Push{User: member, Message: d.ID}
		if err := a.pushes.WithTx(tx).Enqueue(ctx, push, jobs.Key(d.ID+":"+member)); err != nil {
			return err
		}
	}
	return nil
})

func (a *app) push(ctx context.Context, job jobs.Job[Push]) error {
	err := a.provider.Send(ctx, job.Value)
	switch {
	case errors.Is(err, errUnknownToken):
		return job.Fail(ctx, err)
	case errors.As(err, &slowDown):
		return job.Snooze(ctx, jobs.After(slowDown.Wait))
	}
	return err
}
```

**An account deleted in thirty days: the row is the truth, the job the
alarm.** The deletion shows in the application's `users` row, which the
settings page reads and which "cancel" clears; the job only wakes the program
at the row's time. The alarm is enqueued before the row is written, and the
handler does what the row says.

```go
deletions, err := jobs.OpenQueue[int64](ctx, queues, "delete-accounts")

err = deletions.Enqueue(ctx, userID, jobs.At(at), jobs.Key(fmt.Sprint("user:", userID))) // the alarm first
_, err = a.db.Exec(ctx, `update users set delete_at = ? where id = ?`, at.UnixMilli(), userID)
_, err = a.db.Exec(ctx, `update users set delete_at = null where id = ?`, userID) // changed their mind

func (a *app) deleteAccount(ctx context.Context, job jobs.Job[int64]) error {
	at, pending, err := a.deletionTime(ctx, job.Value)
	switch {
	case err != nil:
		return err
	case !pending:
		return nil // cancelled, or deleted already
	case at.After(a.now()):
		return job.Snooze(ctx, jobs.At(at)) // early: the row says later
	}
	return a.purgeUser(ctx, job.Value)
}
```

A crash between the two writes leaves an alarm that finds no row and does
nothing, since the request that wrote it failed. A row moved later wakes the
alarm early, and it snoozes to the row's time. A row moved earlier enqueues
the key again, which brings a waiting alarm forward and asks one that is
running to run once more, so that a handler that read the row a moment before
it changed does not lose the change. Clearing the row is the cancellation;
`Cancel` only tidies.

**A purge every night and a digest a user.** A schedule is a queue whose one
job repeats; a job enqueued with a repeat does the same under its own key, so
a million users' digests are a million rows, each moving to its next evening
when it is acknowledged.

```go
purge, err := jobs.OpenSchedule(ctx, queues, "purge-deleted", jobs.Daily("03:10", moscow))
go purge.Work(ctx, func(ctx context.Context, job jobs.Job[struct{}]) error {
	_, err := a.db.Exec(ctx, `delete from messages where deleted_at < ?`, job.At.AddDate(0, 0, -30).UnixMilli())
	return err
})

err = digests.Enqueue(ctx, Digest{User: id}, jobs.Key(fmt.Sprint("user:", id)), jobs.Daily("20:00", zone))
```

## Keys

- **A key is text** of 1 to 1024 bytes, `jobs.Key(k)`; a job without one is
  found by nothing but its turn.
- **A key names a job's next run**, one job of a queue, waiting, leased or
  failed. `Enqueue` under a key whose job waits adds nothing but can bring its
  time forward, never back, and keeps its value. Under a key whose job runs it
  asks for one more run after this one, no later than the time it gives, so
  that a change made while the handler worked is not lost; several such
  enqueues are one more run. Under a failed job it starts the job again, its
  attempts from zero.

```text
key's job   Enqueue(at 9:00)                   the job then
waiting     10:00 → 9:00; 8:00 → stays 8:00     one run, at the earlier time
running     asks for one more run at 9:00       settled by Ack: runs again at 9:00
failed      starts again at 9:00                attempts from zero
none        a new job at 9:00                   —
```

- **`jobs.KeepDone(d)` makes a key run once.** The queue remembers the keys of
  acknowledged jobs for `d`, and an `Enqueue` under a key that waits, runs or
  ran within `d` adds nothing: a push, an invoice's email, a trigger that must
  not run twice when its caller retries it after it ran.
- **`Update` changes a job that still waits** or has failed: its value, its
  time if `jobs.At` or `jobs.After` says one, its repeat if one is given. A
  leased job, one acknowledged or cancelled, or an absent key is
  `ErrConflict`: too late to edit what is being sent, or was. Two updates race
  as two writes do, the last one kept.
- **`Cancel` removes a job that waits or failed** and says whether it did; a
  leased job is left to its worker.
- **`Scan` walks keys under a prefix** in the byte order of their text, a page
  at a time, and with `State: jobs.Failed` and no prefix the failed jobs, the
  last failed first, keyed or not. A prefix is text like any other: end it
  with a separator, `"chat:42:"`, or `"chat:4"` meets chat 42 too.
- **Without `KeepDone` a key is forgotten when its job is done**, so the next
  `Enqueue` under it adds a job: the alarm of a row that changed again.

## Values

- **JSON, whatever the type.** A value is written by `encoding/json` and read
  back into `V`, so that a worker in another language reads what Go wrote; a
  `[]byte` or `json.RawMessage` is kept as it is. This is not kv's rule, where
  a value keeps the bytes of its Go type.
- **Refused, not changed.** A value JSON cannot write, a NaN or a channel, is
  `ErrInvalid` at `Enqueue`. A value that no longer reads into `V`, because
  the type changed, fails its job for good with the reason rather than
  stopping the queue.
- **At most 1 MiB.** Larger bytes are the blobs engine's, with their key in the
  value.

## Time

- **The store's clock, read once a call.** A job's time is a schedule, not an
  observation, so jobs has no `ErrTooOld` window; a time in the past runs now.
  The store's `tinystore.Options.Clock` moves a test a month ahead without
  sleeping.
- **A job runs when it is due and a worker is free**, in the order of its
  time, equal times in the order they were enqueued. With several workers
  jobs run side by side, so `Workers(1)` is the one order a queue promises.
- **Nothing polls.** Each queue knows the time of its next job and waits for
  it; an `Enqueue` earlier than that wakes it. A million jobs due next week
  cost nothing until next week.

## A job's life

```text
       Enqueue
          │
          ▼
┌──►  waiting (until At) ──── Cancel ──► gone
│         │ Claim: leased until now + Lease, attempt + 1
│         ▼
│     leased ── Extend ──► leased until now + d
│      │  │  └─ Fail, or Retry past MaxAttempts ──► failed (kept KeepFailed, found by Scan)
│      │  └──── Ack ──► gone
└─ Retry, Snooze, or the lease ends
```

- **At least once.** A job enqueued and not cancelled runs, whatever the
  process does. It may run twice: when a process dies after the work and before
  the acknowledgement, the next claim runs it again. A handler's effect is
  therefore keyed by what the job names, `on conflict do nothing`, and exactly
  once is a promise no store can keep for a side effect outside it.
- **A lease that ends gives the job back.** A worker that crashes, hangs,
  forgets its `Ack` or loses its connection never acknowledges, and when its
  lease ends the job is claimable again. A process that died held every lease
  it had, and its store's next open ends them.
- **An attempt is counted before it runs.** A claim writes the attempt, so a
  job that kills its process, out of memory or a fatal error, fails for good
  after `MaxAttempts` restarts rather than stopping the program for ever.
- **A lease is its attempt.** Settling a job names the attempt it claimed; one
  whose lease ended and whose job another worker has claimed since is
  `ErrConflict`, and changes nothing. Kv's claims keep the same promise.
- **`Retry` waits longer each time**, one second, then two, then four, up to
  an hour, each a tenth longer or shorter at random, so that the jobs one
  outage failed do not return in the same second; `jobs.Backoff(first,
  longest)` changes both, and `jobs.At` or `jobs.After` names the time
  instead. Past `MaxAttempts`, 10 by default, the job fails for good.
- **`Snooze` is not a failure.** It moves the job to its new time and counts
  no attempt: a provider that asks for a minute, a row that says later.
- **A failed job is kept** `KeepFailed`, seven days by default, with the last
  error, and `Scan`, `Get`, `Update` and `Cancel` find it.

## Repeats

- **A repeat is a cron expression and a zone**, kept as their text so that
  another language reads the same schedule: five fields, minute, hour, day of
  the month, month and day of the week, with `*`, ranges, steps and lists;
  `@hourly`, `@daily`, `@weekly` and `@monthly`; and `@every 15m`, aligned to
  multiples of its interval since the Unix epoch. `jobs.Cron(expr, zone)`,
  `jobs.Daily("03:10", zone)` and `jobs.Every(d)` write it.
- **A zone has a name.** `time.Local` names none, and a schedule kept by it
  would change meaning on a host in another zone, so it is `ErrInvalid`; a time
  that daylight saving skips runs at the first minute after it, and one it
  repeats runs once.
- **A repeating job never overlaps itself and never piles up.** It moves to
  its next time when it is acknowledged or fails, the first after its own time
  and after now: a purge that ran long, or a program down all night, runs once
  and not for every time it missed. A `Retry` never moves it past its next
  time, and a failure does not end it; its last error stays on its entry.
- **A job enqueued with a repeat needs a key**, since only a key stops it: a
  repeat without one is `ErrInvalid`. `Cancel` ends it, and `Update` changes
  its value or its repeat.
- **`OpenSchedule` is a queue of `struct{}`** holding one job under the
  queue's name; the repeat in the program is the one kept, so a changed
  `Daily` takes effect on the next open. `job.At` is the time it runs for.

## Working

```go
err = later.Work(ctx, a.sendLater, jobs.Workers(8))    // blocks until ctx ends or the store closes
err = later.Work(ctx, a.sendLater, jobs.UntilIdle())   // until nothing is due: tests and Manual stores

job, found, err := later.Claim(ctx)                    // by hand: what Work does, and a remote worker will
if found {
	if err = deliver(ctx, job.Value); err != nil {
		err = job.Retry(ctx, err)
	} else {
		err = job.Ack(ctx)
	}
}
```

- **`Work` settles by what the handler returns**: nil acknowledges, an error
  retries by the queue's policy, and a handler that settles its job itself,
  with `Fail`, `Retry` or `Snooze`, is left as it settled it. A panic is an
  error, with its value in the job's.
- **`Work` holds the lease while the handler runs**, extending it every half a
  lease, so `Lease` bounds how long a vanished worker keeps a job, not how
  long a handler may take. `jobs.Timeout(d)`, a minute by default, is the
  deadline of the handler's context; past it the attempt failed.
- **`Work` holds two jobs a worker**, the one it runs and the next, and claims
  what it lacks in one transaction with the settlements of the jobs its
  workers finished since, a finished worker counted free in that same write,
  so that a queue under load commits once for many jobs and a worker never
  waits for a commit to start its next. A job claimed ahead is leased:
  `Update` and `Cancel` find it too late, and a crash counts its attempt.
- **A handler stopped by its caller's context or by `Close`** gives its job
  back, and that attempt is not counted, when it returns that cancellation.
  What a handler returned decides, not whether `Work` has ended since: the
  loop may end between the handler's return and its settlement, and a handler
  that finished, or failed on its own, is settled as it returned. Deciding by
  the loop lost an ack now and then, the job running again, and failed the
  server's attempts a lost worker held as given back.
- **`Work` starts its workers inside the call** and waits for them before it
  returns, so nothing it starts outlives it, and the store starts no goroutine
  for jobs.
- **`Claim` does not wait**: it leases the next due job, or says there is
  none. `jobs.Lease(d)` gives the lease, the queue's by default, 30 seconds.

## Under a flood

**Memory does not grow with the queue.** What waits is rows in `jobs.db`; a
queue holds in memory the time of its next job, the jobs its workers have in
hand and the settlements not yet written. A million jobs cost their rows,
about three hundred bytes each in the probe that preceded this design, and
nothing else.

**A burst is drained in batches in the order of time.** A million jobs due
within one minute are claimed from the front of the queue as many at a time as
the workers take, each batch one transaction with the settlements before it;
the round decides the layout that keeps the jobs of one minute on neighbouring
pages, which the probe found worth several times the throughput.

**One queue does not hold up another.** Each has its workers and its own
claims; what they share is the file's one writer, which a batch holds for tens
of milliseconds at most. Priority within a queue is a second queue.

**What the calls hold is the store's memory.** With `Options.Memory`, an
`Enqueue` takes the memory its value may hold before it encodes it, and holds
the value while it waits for the writer; a `Get` holds 1 MiB and a `Scan` 4.5
MiB while they read. `Work` reserves room for the inline rows and keys of a
claim batch before the writer reads them, releasing each row's room when a
worker takes it; the worker holds its value from before it reads it until its
handler returns. A claim reads no spilled value inside the writer,
where waiting for memory would hold the writer too. A call inside `Tx` holds
the writer, so it takes only the memory that is free and is `ErrLimit` past
it.

**What a program adds is bounded.** `MaxWaiting(n)`, ten million by default,
refuses the job past it with `ErrLimit` and logs it once, so a loop that
enqueues without end stops at a limit and not at a full disk. A limit a user
or a chat may schedule is the application's, and kv's counters count it.

## Two files, no transaction

A job and a row of `sql/app.db` are two commits, and no transaction spans
them. The cases show the two shapes that need none:

- **The job is the data** when the thing exists only until it happens: a
  scheduled message, a reminder, a push. The handler writes its effect into
  the application's database under the job's key.
- **The row is the truth and the job its alarm** when the application queries
  the thing: an account's deletion, a trial's end, an invoice's reminder. The
  alarm is enqueued first under the row's id; the handler reads the row and
  does what it says, snoozing when the row says later and doing nothing when
  the row is gone.

Neither polls, and neither needs a sweep that repairs what a crash left.

## Errors

| Sentinel | When |
|---|---|
| `ErrInvalid` | a key empty or over 1 KiB, a value JSON cannot write, a repeat without a key, a cron expression that does not parse or never runs, a zone without a name, a queue opened as another kind, an option of another kind |
| `ErrLimit` | a value over 1 MiB, a job past `MaxWaiting` |
| `ErrConflict` | settling a job whose lease another claim has taken since, `Update` of a job that no longer waits |
| `ErrClosed` | the store closed, or a `WithTx` handle used after its callback |
| `ErrCorrupt` | a row that no longer decodes |

An error about one job is a `*jobs.JobError` naming its queue and key;
`errors.Is` finds the store's sentinel in it.

## Storage

Settled by the round: rows in the order of their time, a lease in a table of
its own, 4 KiB pages, values past 512 bytes spilled; and by
[the engine's round](https://github.com/tinyshed/research/blob/main/tinystore/reports/jobs-engine-2026-09-27.md), keys in a table of
their own.

```text
queues    id | name | kind | waiting                a queue or a schedule; its exact job count
jobs      queue | next | id | key | at | attempt | again | repeat | error | value | spill
          without rowid, primary key (queue, next, id)
keys      queue | key | next | id                   where a keyed job lies; left behind when it leaves
leases    id | queue | next | attempt | until       a claimed job's lease; attempt is its token
failed    queue | id | key | at | attempt | failed | error | value | spill   kept KeepFailed
spilled   id | value                                values past 512 bytes
done      queue | key | until                       keys KeepDone remembers
meta      name | value                              the high-water mark of job ids
```

- **Rows lie in the order of their time**, so the jobs of one minute sit on
  neighbouring pages however long before they were enqueued: a million of
  them drained 5.4 times faster than from a table in the order they arrived,
  which scatters them. A job due at a random time pays for it when it is
  enqueued, into the middle of the tree.
- **A claim leaves the job's row alone**: it writes a row of `leases`, and the
  claims after it pass over jobs a live lease holds. Settling drops the lease
  row, the token, then deletes, moves or fails the job's row; an attempt the
  lease counted is written to the job's row only when it failed, so a
  `Snooze` or a job given back counts none. A next open counts the attempts of
  the leases the file still holds and empties the table, and their jobs are
  due at once.
- **A key lives beside its job, not on its row.** A keyed job's key is a row
  of `keys` naming where the job lies; a job that moves takes it along, and one
  that leaves the queue leaves it behind, where it names nothing until
  maintenance drops it in the order of the keys. Acknowledging a burst of
  keyed jobs then writes no page outside the order of their time: 2.7 times
  the jobs a second in the prototype, a key read 3.7 µs slower, an enqueue no
  slower.
- **A job's id is never given twice**, not after a crash: ids come from a block
  of a thousand reserved in `meta` by a transaction of its own, and a `Tx`
  reserves its own.
- **Maintenance runs through `Store.Every`**: failed jobs past `KeepFailed` and
  the keys their jobs left behind, of the queues this process opened, and
  keys past `KeepDone`.
- **Backup copies `jobs.db`** like any engine's file; settlements `Work` has
  not written yet are not in the copy, and those jobs run again after a
  restore.

## Bounds

| Object | Limit |
|---|---:|
| A key | 1 KiB |
| A value | 1 MiB |
| A lease | 30 s by default |
| Attempts | 10 by default |
| A retry's wait | 1 s doubling to 1 h by default, ± 10 % |
| Waiting jobs a queue | 10,000,000 by default |
| A failed job kept | 7 days by default |
| A handler's deadline | 1 min by default |
| A `Scan` page | 1000 jobs, 4 MiB of values |
| Jobs a claim takes | one a `Claim`, the free workers of a `Work`, 1,000 at most |

## Gates

The five cases are the gates' workloads.

| Promise | What enforces it |
|---|---|
| an `Enqueue` that returned survives an abrupt exit | `TestAnEnqueuedJobSurvivesAnAbruptExit` |
| a job runs at its time and not before | `TestAJobRunsAtItsTimeAndNotBefore` |
| a key names one job, and enqueuing it again only brings it forward | `TestAKeyNamesOneJobAndARepeatOnlyBringsItForward` |
| an enqueue while its key's job runs asks for one run more | `TestAnEnqueueWhileItsJobRunsAsksForOneRunMore` |
| `KeepDone` makes a key run once | `TestKeepDoneMakesAKeyRunOnce` |
| `Update` changes only a job that still waits | `TestUpdateChangesOnlyAWaitingJob` |
| `Cancel` says whether it came in time | `TestCancelSaysWhetherItCameInTime` |
| a job whose lease ended runs again | `TestAJobWhoseLeaseEndedRunsAgain` |
| a stale lease settles nothing | `TestAStaleLeaseSettlesNothing` |
| a job that kills its process fails after its attempts | `TestAJobThatKillsItsProcessFailsAfterItsAttempts` |
| a retry waits longer each time, then fails for good | `TestARetryWaitsLongerEachTimeThenFailsForGood` |
| a snooze counts no attempt | `TestASnoozeCountsNoAttempt` |
| a repeating job neither overlaps nor piles up | `TestARepeatingJobNeitherOverlapsNorPilesUp` |
| a schedule keeps its zone across daylight saving | `TestAScheduleKeepsItsZoneAcrossDaylightSaving` |
| `Work` settles by what the handler returns | `TestWorkSettlesByWhatTheHandlerReturns` |
| a handler stopped by `Close` gives its job back uncounted | `TestCloseGivesRunningJobsBackUncounted` |
| a value comes back as the JSON it went in as | `TestAValueComesBackAsTheJSONItWentIn` |
| a value that no longer reads fails its job, not its queue | `TestAValueThatNoLongerReadsFailsItsJob` |
| a queue past `MaxWaiting` refuses the next job | `TestAQueuePastMaxWaitingRefusesTheNextJob` |
| a failed job is kept, then removed | `TestAFailedJobIsKeptThenRemoved` |
| jobs due together are claimed in batches | `TestJobsDueTogetherAreClaimedInBatches` |
| a Work loop lets go of a lease another claim took | `TestWorkLetsGoOfALeaseAnotherClaimTook` |
| a Scan walks keys under a prefix, and the failed jobs by time | `TestScanWalksTheKeysUnderAPrefixAPageAtATime`, `TestScanListsTheFailedJobsTheLastFailedFirst` |
| a Scan page holds at most its jobs and bytes | `TestAScanPageHoldsAtMostItsBytes` |
| a key left behind names nothing, and maintenance drops it | `TestAKeyLeftBehindNamesNothingAndMaintenanceDropsIt` |
| a job that moves takes its key along | `TestAMovedJobTakesItsKeyAlong` |
| values are held in the store's memory | `TestStoreMemoryBoundsEnqueuesReadsAndHandlers` |
| an `Enqueue` waiting for memory has written nothing | `TestAnEnqueueWaitingForMemoryHasWrittenNothing` |
| a call inside `Tx` takes only the memory that is free | `TestATransactionTakesOnlyTheMemoryThatIsFree` |

## What the runtime gains

- **A rule's wording.** "Only the store starts goroutines" becomes "only the
  store starts goroutines that outlive a call": `Work` starts its workers
  inside the call and waits for them, and nothing of it runs after it returns.
  `AGENTS.md` changes with the code.
- **Grouped writes in sqldb.** A handler that writes the application's
  database commits once an `Exec`, and a transaction each held about 340
  writes a second in the container at any concurrency in
  [the kv round](https://github.com/tinyshed/research/blob/main/tinystore/reports/kv-mechanics-2026-09-26.md); eight workers inserting
  messages wait for each other's fsync however fast the queue is. `Exec` joins
  `internal/sqlite.File.UpdateGrouped`, as kv's writes did, as a change of its
  own with its own gate.

## Not in the first version

Each waits for a workload that needs it and a measurement that pays for it.

- Priority within a queue: the jobs are kept in the order of their time, and a
  second order beside it is a second queue.
- Cancelling a job that runs, through its handler's context.
- A rate a queue may not pass; `Workers` bounds how many run at once.
- Remote workers, through the server of their own.
- Writing a job's history to records: the application logs what it does, and
  the engine logs failures once a quiet period.

## Open

What [the round](https://github.com/tinyshed/research/blob/main/tinystore/reports/jobs-mechanics-2026-09-27.md) left, for the next:

| Question | Why |
|---|---|
| the engine against a table an application polls by hand, on the five cases | what the engine buys over the way it replaces |
| the page cache of the writer in a burst over a large file | every burst ran with 1 MiB a connection |
| a queue's count at open | `OpenQueue` reads the durable `queues.waiting` counter; jobs table triggers keep it exact inside a write |

Settled since, in [the engine's round](https://github.com/tinyshed/research/blob/main/tinystore/reports/jobs-engine-2026-09-27.md): a
Work loop holds two jobs a worker, sqldb's `Exec` commits grouped, and keys
live in a table of their own, dropped lazily in key order by maintenance.
