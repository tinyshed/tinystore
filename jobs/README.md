# jobs

Work an application must do later, or now but outside the request that asked
for it, in `jobs.db` inside a `tinystore.Store`: a reminder at six, a message
sent at nine, a push to each member of a group, an account deleted thirty days
after its owner asked, a purge every night. Typed queues keep their jobs in the
order of their time, lease each to a worker, retry, repeat by cron text, run
every job at least once, and say where each one is. The design and the
measurements behind it are [docs/jobs.md](../docs/jobs.md).

```go
queues, err := jobs.Open(ctx, store, jobs.Options{}) // data/jobs.db

reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")
later, err := jobs.OpenQueue[Draft](ctx, queues, "send-later", jobs.MaxAttempts(20))
pushes, err := jobs.OpenQueue[Push](ctx, queues, "pushes", jobs.KeepDone(time.Hour))
videos, err := jobs.OpenQueue[Video](ctx, queues, "videos", jobs.MaxRunning(2))
purge, err := jobs.OpenSchedule(ctx, queues, "purge-deleted", jobs.Daily("03:10", moscow))

err = reminders.Enqueue(ctx, Reminder{User: 42, Text: "call mom"}, jobs.At(evening))
err = later.Enqueue(ctx, draft, jobs.At(nine), jobs.Key("chat:42:"+draft.ID))
page, err := later.Scan(ctx, jobs.Query{Prefix: "chat:42:"}) // "3 scheduled messages"
cancelled, err := later.Cancel(ctx, "chat:42:"+draft.ID)     // false: sent already

go reminders.Work(ctx, remind, jobs.Workers(4)) // until ctx ends or the store closes
entry, found, err := purge.Get(ctx, "purge-deleted") // At: the next run; Ran, Took and Err: the last

func remind(ctx context.Context, job jobs.Job[Reminder]) error {
	return push(ctx, job.Value.User, job.Value.Text) // nil acknowledges, an error retries
}

for entry, err := range videos.Watch(ctx, video.ID) { // waiting, 3 ahead … running, 0.4 … done
	if err != nil {
		return err
	}
	send(entry.State, entry.Ahead, entry.Progress)
}

func transcode(ctx context.Context, job jobs.Job[Video]) error {
	return ffmpeg(ctx, job.Value, func(done float64) { job.Progress(done) })
}
```

[example_test.go](example_test.go) runs the five cases of
[docs/jobs.md](../docs/jobs.md) as examples, and `go doc` shows them.

A store opened `In` an sqldb database keeps its queues in that database's own
file, so that a job commits with the rows it is about, in one batch and one
fsync, or not at all:

```go
queues, err := jobs.Open(ctx, store, jobs.Options{In: db}) // data/sql/app.db
index, err := jobs.OpenQueue[IndexNote](ctx, queues, "index")

err = db.Batch(ctx, func(b *sqldb.Batch) error {
	b.Exec(`update notes set body = ? where id = ?`, body, id)
	b.Add(index.Enqueued(ctx, IndexNote{ID: id}))
	return nil
})
```

## Contracts

- **Queues live in `jobs.db`, or `In` a database.** A store opened with
  `Options.In` migrates its tables, named `_tinystore_jobs…`, into the
  database's file with a history of their own, keeps no file of its own and
  copies nothing into a snapshot: the database's copy holds the queues. A
  database holds one store's queues. Its `Enqueued` writes a job in a batch of
  that database, which commits it with the batch's rows or not at all; a queue
  whose store lives elsewhere is refused there with `ErrInvalid`. The store
  opens after the database it names, and so closes before it.

- **A job runs at least once.** An `Enqueue` returns once the job is in the
  file, and a job not cancelled runs whatever the process does. It may run
  twice: a process that dies after the work and before the acknowledgement
  runs it again, so a handler keys its effect by what the job names, `on
  conflict do nothing`.
- **A job runs when it is due and a worker is free**, in the order of its
  time, equal times in the order they were enqueued. A time in the past runs
  now. `Workers(1)`, the default, is the one order a queue promises.
- **A key names one job**, 1 to 1024 bytes of text, waiting, running or failed.
  An `Enqueue` under a key whose job waits adds nothing and can bring it
  forward, never back; under one whose job runs it asks for one run more after
  this one; under a failed one it starts the job again, its attempts from zero.
  `KeepDone(d)` remembers the keys of acknowledged jobs for `d`, and an
  `Enqueue` under one of them adds nothing: a key runs once. Without it a key
  is forgotten when its job is done.
- **`Update` changes a job that waits or failed**, its value and, when the
  options say, its time or repeat. A job a worker holds, one done or
  cancelled, or an absent key is `tinystore.ErrConflict`.
- **`Cancel` takes the job under a key**, whether it waits, runs or failed,
  and says whether there was one. A running job's handler sees its context end
  with `jobs.ErrCancelled`, and what it returns settles nothing, as a `Claim`'s
  caller's settlement is then `ErrConflict` wrapping `ErrCancelled`; what the
  handler did before it stopped stays done. A job a `Work` loop holds for a
  busy worker never starts, and a repeating job stops repeating.
- **A value is JSON.** `encoding/json` writes it and reads it back into `V`,
  so that another language reads what Go wrote; a `[]byte` or
  `json.RawMessage` is kept as it is. A value JSON cannot write is
  `ErrInvalid`; one over 1 MiB is `ErrLimit`; one over 512 bytes lives in a row
  of its own. A value that no longer reads into `V`, because the type changed,
  fails its job for good with the reason, and the queue goes on.
- **A lease is its attempt.** A claim leases a job for the queue's `Lease`, 30
  seconds unless it says, and counts the attempt before the job runs, so a
  job that kills its process fails for good after `MaxAttempts` restarts. A
  settlement names the attempt it claimed: one whose lease ended and whose job
  another claim has taken since is `ErrConflict` and changes nothing. A lease
  that ends gives the job back; the leases a dead process held are given back,
  their attempts counted, when the store next opens.
- **`Retry` waits longer each time**, one second doubling to an hour, a tenth
  longer or shorter at random, or at `jobs.At` or `jobs.After`; past
  `MaxAttempts`, 10 unless it says, the job fails for good. `Snooze` moves a
  job without counting an attempt. `Fail` fails it for good. A failed job is
  kept `KeepFailed`, seven days unless it says, with its last error, and
  `Get`, `Scan`, `Update` and `Cancel` find it.
- **A repeat is a cron expression and a zone's name**, kept as their text:
  `jobs.Cron`, `jobs.Daily`, `jobs.Every`. A zone without a name,
  `time.Local`, is `ErrInvalid`; a time daylight saving skips runs when the
  skip ends, one it repeats runs once. A repeating job moves to its next time
  after its own and after now when it is acknowledged or fails, so it never
  overlaps itself and a program down all night runs it once. It needs a key,
  since only a key stops it. `OpenSchedule` is a queue of `struct{}` holding
  one repeating job under the queue's name, with the program's repeat.
- **`Work` settles by what the handler returns**: nil acknowledges, an error
  retries, a panic is an error with its stack, and a handler that settled its
  job itself is left as it settled it. It holds two jobs a worker, the one
  the worker runs and its next, and claims what it lacks, 1,000 at most, in
  one write with the settlements of the jobs its workers finished since. A job
  claimed ahead is leased, though it waits: `Update` finds it too late, `Cancel`
  keeps it from starting, and a crash counts its attempt though it never ran.
  It extends a held job's lease every half lease;
  `Timeout(d)`, a minute unless it says, is the deadline of the handler's
  context. A lease lost while its handler runs, because a stall let it end
  and another claim took the job, is let go and logged once a quiet period.
  A handler stopped by `Work`'s context or by `Close`, returning that
  cancellation, gives its job back without counting the attempt; one that
  returns nil or an error of its own settles as it returned, though `Work`
  ended meanwhile. `Work` starts its workers inside the call and
  waits for them before it returns; `UntilIdle` returns once no job is due and
  none runs.
- **`Claim` does not wait**: it leases the next due job, or says there is
  none. A `Job` from `Claim` is settled by `Ack`, `Retry`, `Fail`, `Snooze` or
  `Extend`; copies of a `Job` share its lease.
- **`MaxRunning(n)` bounds the jobs that run at once**, across every `Work`
  loop and `Claim` of the store: past it a claim takes nothing until a job is
  settled or its lease ends, and a settlement wakes the loops waiting for a
  place. A `Work` loop of such a queue claims no job ahead for a busy worker,
  so that the jobs it holds are the ones running.
- **`Get` says where a job is.** Its `State` is `Waiting` until a handler, or
  a `Claim`'s caller, has it, `Running` while one does, `Failed` when it failed
  for good, and `Done` while `KeepDone` keeps its key; a job a `Work` loop
  holds for a busy worker waits, none ahead of it. `Ahead` counts the jobs that
  run before a waiting one, the rows before it less those running, up to
  10,000, so that a `Get` reads at most that many keys of the queue's order;
  `Scan` leaves it zero. `Ran` and `Took` say when the last run a handler
  finished began and how long it took, acknowledged, retried, failed or
  snoozed, beside `Err`; a run given back records none, and a schedule's `At`
  is its next run.
- **Progress lives in memory.** `Job.Progress(v)` keeps `v` as JSON until the
  attempt is settled, and `Get` and `Watch` show it as `Entry.Progress`. It is
  not written to the file, since an attempt a restart ends runs again from its
  start. A value JSON cannot write, or one past 4 KiB, is dropped and logged
  once a quiet period.
- **A step runs once in a run.** `jobs.Step(ctx, job, "search", fn)` runs
  `fn` and keeps what it answered, as JSON in the job's file, so that the
  attempt after a retry, a lost lease or a restart gets the answer back
  without running `fn` again, and runs only the steps left. A step whose
  attempt ends before its answer is kept runs again, so what `fn` does outside
  the store should bear doing twice. A name is a step's within its run, 1 to
  256 bytes, which a loop numbers (`model:1`, `tool:1`), and an answer at most
  1 MiB of JSON. Only the attempt holding the job's lease keeps a step; one
  whose lease another claim took keeps nothing. A run that ends, done, failed
  for good or cancelled, takes its steps along, and a repeating job's next run
  starts without them. `Job.Kept` and `Job.Keep` are what `Step` is made of,
  for a worker in another process.
- **`Watch` follows a job to its end.** It yields the job under a key as it
  is, then again when its state, place, attempt, time, progress or error
  changes, until the job is done, failed or `Cancelled`, a state only a watcher
  sees, which is the last entry; a later job under the key is another's. A key
  that names no job yields nothing. It reads the job at most five times a
  second, so that a watcher behind a busy queue sees the latest, and ends when
  its context does or the store closes.
- **`Scan` walks keys under a prefix** in the byte order of their text, a page
  at a time, waiting, running and failed jobs alike, or the ones `State` names;
  with `State: jobs.Failed` and no prefix it lists the failed jobs, the last
  failed first. A page holds `Limit` jobs, 100 unless it says and 1,000 at
  most, and ends before the value that would take it past 4 MiB, a spilled
  value counted as a row's is; since memory says which held job runs, a page
  of waiting or running jobs may hold fewer. `All` walks the same pages without
  a snapshot between them. A prefix is text: `"chat:4"` meets chat 42 too.
- **Nothing polls.** Each queue knows the time of its next job and a waiting
  `Work` sleeps until it, or until an `Enqueue` or a settlement brings it
  earlier; a million jobs due next week cost their rows and nothing else.
- **What a queue holds is bounded.** `MaxWaiting(n)`, ten million unless it
  says, refuses the job past it with `ErrLimit` and logs it once a quiet
  period.
- **Values are held in the store's memory.** With `Options.Memory`, an
  `Enqueue` or `Update` takes its turn and the memory its value may hold
  before it encodes it, a `[]byte` its length and JSON the largest value until
  it is written, and holds the value while it waits for the writer; a `Get`
  holds 1 MiB and a `Scan` 4.5 MiB while they read, and a `Work` worker a job's
  value from before it reads it until its handler returns. `Work` also reserves
  the inline rows and keys of a claim batch until their workers take them. A
  call inside a `Tx` waits for no memory, since it holds the writer that the
  writes holding memory wait for: it takes what is free, and past that it is
  `ErrLimit`.
- **Several enqueues, one commit.** `Store.Tx` runs work in one writer
  transaction, nil committing, an error or a panic rolling back; a queue works
  in it through `WithTx`, which is `ErrClosed` after the callback. `Work`
  inside a transaction is `ErrInvalid`. Writes outside it from many goroutines
  commit together, each in a savepoint, with one fsync; a group whose commit
  fails answers `jobs.ErrOutcomeUnknown`.
- **An error names its job.** A call refused because of one job is a
  `*jobs.JobError` with the queue and the key; `errors.Is` finds the store's
  sentinel in it. A queue name is `[a-z0-9][a-z0-9_-]{0,63}` and keeps its
  kind: a schedule does not open as a queue. A second handle on a queue in one
  process takes the same options, or is `ErrInvalid`.
- **Maintenance** removes the failed jobs past `KeepFailed` of the queues this
  process opened, the keys their jobs left behind when they left the queue,
  which name nothing meanwhile, and the done keys past `KeepDone`, every minute
  unless the store is Manual, 10,000 rows a transaction. `Store.Snapshot` copies
  `jobs.db` like any engine's file; settlements a `Work` has not written yet
  are not in the copy.

## Testing without waiting

A Manual store with a clock the test moves runs no background work, and
`UntilIdle` runs what is due and returns:

```go
now := time.Now()
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{Manual: true, Clock: func() time.Time { return now }})
queues, err := jobs.Open(ctx, store, jobs.Options{})
reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")

err = reminders.Enqueue(ctx, Reminder{User: 42}, jobs.After(time.Hour))
err = reminders.Work(ctx, remind, jobs.UntilIdle()) // nothing is due yet
now = now.Add(time.Hour)
err = reminders.Work(ctx, remind, jobs.UntilIdle()) // runs it
_, err = queues.Maintain(ctx)                       // removes failed jobs and done keys past their time
```

## Not in the first version

What [docs/jobs.md](../docs/jobs.md) leaves for later: priority within a
queue, a rate a queue may not pass, a job's history in records.
