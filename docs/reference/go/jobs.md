# Jobs API for Go

Every public type, function and constant of the Jobs engine in `github.com/tinyshed/tinystore/jobs`, generated from its source. The [Jobs guide](../../jobs/README.md) explains how to use them, and the [Bun and Node](../bun/jobs.md) and [Python](../python/jobs.md) pages list the same API.

## LimitValueBytes

```go
const (
	LimitValueBytes = "bytes of a job's value"
	LimitStepBytes  = "bytes of a step's answer"
)
```

The names of a job's limits, which a LimitError's Name holds.

## ErrCancelled

```go
var ErrCancelled = errors.New("jobs: the job was cancelled while it ran")
```

ErrCancelled is the cause a handler's context ends with when Cancel takes its job while it runs; what the handler returns then settles nothing.

## ErrOutcomeUnknown

```go
var ErrOutcomeUnknown = sqlite.ErrOutcomeUnknown
```

ErrOutcomeUnknown is returned for a write whose group's commit failed. The write may or may not be in the file, so its caller reads it back before writing again.

## Step

```go
func Step[T, V any](ctx context.Context, job Job[V], name string, fn func(context.Context) (T, error)) (T, error)
```

Step runs fn once for the job under name and keeps what it answered, as JSON: an attempt after a retry, a lost lease or a restart gets the kept answer back without running fn again.

	hits, err := jobs.Step(ctx, job, "search", func(ctx context.Context) ([]Hit, error) {
		return search(ctx, job.Value.Query)
	})

A step whose attempt ends before its answer is kept runs again, so what fn does outside the store should bear doing twice. A name is the step's within one run of the job, which a loop numbers: "model:1", "tool:1", "model:2". A run that ends, done, failed for good or cancelled, takes its steps along, and a repeating job's next run starts without them. An answer is at most 1 MiB of JSON.

## Change

```go
type Change interface {
	Bytes() (int, error)
	Apply(ctx context.Context, file *sqlite.File, w sqlite.Writer) error
	Done(err error)
}
```

Change is a job a batch of the database the queues live in writes with its rows, which Enqueued gives out: see Options.In and sqldb's Batch.Add. A program does not call its methods.

## ClaimOption

```go
type ClaimOption interface {
	// contains filtered or unexported methods
}
```

## Database

```go
type Database interface {
	SQLiteFile() *sqlite.File
}
```

Database is a file whose owner lets the queues live in it, as sqldb's DB does. A program does not implement it: its method names the store's own type.

## EnqueueOption

```go
type EnqueueOption interface {
	// contains filtered or unexported methods
}
```

### Key

```go
func Key(key string) EnqueueOption
```

Key names the job, so that Get, Update and Cancel find it and an Enqueue under a key whose job waits adds nothing; see the queue's rules for keys.

## Entry

```go
type Entry[V any] struct {
	Key   string
	Value V
	// At is the time the job runs for.
	At time.Time
	// Attempt counts the job's attempts, a running one included.
	Attempt int
	State   State
	// Ahead counts the jobs that run before a waiting one, up to 10,000, so
	// that 10,000 reads as that many or more. Get and Watch count it; Scan
	// leaves it zero.
	Ahead int
	// Progress is what the handler of a running job last reported of it, as
	// JSON, until the attempt is settled.
	Progress json.RawMessage
	// Err is the job's last failure.
	Err string
	// Ran is when the last run a handler finished began, and Took how long it
	// took: acknowledged, retried, failed or snoozed. Both are zero before a
	// handler finished one, and a run given back records none.
	Ran  time.Time
	Took time.Duration
	// Repeat is a repeating job's repeat as jobs.db keeps it: cron text and
	// zone, or an interval.
	Repeat string
}
```

Entry is a job as the queue holds it.

## Job

```go
type Job[V any] struct {
	Key   string
	Value V
	At    time.Time
	// Attempt is this attempt's number, one for the first.
	Attempt int
	// contains filtered or unexported fields
}
```

Job is a job in a worker's hands. Its lease is held until the worker settles it with Ack, Retry, Fail or Snooze, or the lease ends and another claim may take it. Copies of a Job share its lease.

### Job.Ack

```go
func (j Job[V]) Ack(ctx context.Context) error
```

Ack settles the job as done and it leaves the queue. A repeating job, or one an Enqueue asked to run again while it ran, waits for its next time instead.

### Job.Extend

```go
func (j Job[V]) Extend(ctx context.Context, d time.Duration) error
```

Extend makes the lease run d from now; Work extends its handlers' leases itself.

### Job.Fail

```go
func (j Job[V]) Fail(ctx context.Context, cause error) error
```

Fail settles the job as failed for good: no more attempts. A repeating job waits for its next time instead, its last error kept.

### Job.Keep

```go
func (j Job[V]) Keep(ctx context.Context, name string, answer json.RawMessage) error
```

Keep keeps answer, JSON, as what the job's run answered under a step's name, which Step does once its step has run: a later attempt of the run reads it with Kept. It returns once the answer is written, and refuses one whose lease another claim has taken.

### Job.Kept

```go
func (j Job[V]) Kept(ctx context.Context, name string) (answer json.RawMessage, found bool, err error)
```

Kept is the answer the job's run kept under a step's name, which Step reads before it runs the step; found is false for a step not yet kept.

### Job.Progress

```go
func (j Job[V]) Progress(v any)
```

Progress keeps v, as JSON, as what the handler reports of this attempt: Get and Watch show it until the attempt is settled. It lives in the store's memory and is not written to the file, since an attempt a restart ends runs again from its start. A value JSON cannot write, or one past 4 KiB, is dropped and logged once a quiet period.

### Job.Retry

```go
func (j Job[V]) Retry(ctx context.Context, cause error, options ...SettleOption) error
```

Retry settles a failed attempt: the job runs again after the queue's backoff, or at jobs.At or jobs.After, and past MaxAttempts it fails for good.

### Job.Snooze

```go
func (j Job[V]) Snooze(ctx context.Context, options ...SettleOption) error
```

Snooze puts the job back until jobs.At or jobs.After without counting the attempt: a provider that asks for a minute, a row that says later.

## JobError

```go
type JobError struct {
	Queue, Key string
	Err        error
}
```

JobError is a call refused because of one job: its queue and key, empty for a job without one. errors.Is finds the store's sentinel in it.

### JobError.Error

```go
func (e *JobError) Error() string
```

### JobError.Unwrap

```go
func (e *JobError) Unwrap() error
```

## LeaseOption

```go
type LeaseOption interface {
	QueueOption
	ClaimOption
}
```

### Lease

```go
func Lease(d time.Duration) LeaseOption
```

Lease is how long a claimed job stays its worker's before another claim may take it: 30 seconds unless it says, for a queue or for one Claim. Work extends it while its handler runs.

## Maintenance

```go
type Maintenance struct {
	Failed int // failed jobs past their queue's KeepFailed
	Done   int // keys past their queue's KeepDone
	Keys   int // keys that jobs gone from the queue left behind
}
```

Maintenance is what one Maintain call removed.

## Options

```go
type Options struct {
	// In keeps the queues in a database's own file, an sqldb DB's, instead of
	// jobs.db, so that a batch of the database commits a job with the rows it
	// is about: see Queue.Enqueued. Its tables are named _tinystore_jobs, and
	// the database's schema leaves them out. Nil keeps jobs.db.
	In Database
}
```

Options says where the store keeps its queues.

## Page

```go
type Page[V any] struct {
	Entries []Entry[V]
	// More says the limit, or the page's 4 MiB of values, ended the page before
	// the jobs did.
	More bool
	// Next is the query that reads on.
	Next Query
}
```

Page is one page of a Scan, from one snapshot.

## Query

```go
type Query struct {
	Prefix string
	// State narrows either listing to Waiting, Running or Failed jobs.
	State State
	// After is where the page before ended. Page.Next carries it.
	After string
	Limit int // jobs a page returns: 100 when zero, at most 1000
}
```

Query asks Scan for a page of the queue's jobs.

With a Prefix it lists the keys under it in the byte order of their text. With State Failed and no Prefix it lists the failed jobs instead, the last failed first, keyed or not.

## Queue

```go
type Queue[V any] struct {
	// contains filtered or unexported fields
}
```

Queue is a handle on the jobs of one type in one queue; Work and Claim hand its due jobs to whoever works them.

### OpenQueue

```go
func OpenQueue[V any](ctx context.Context, store *Store, name string, options ...QueueOption) (*Queue[V], error)
```

OpenQueue opens the queue name of jobs.db, creating it on first use. Its name and kind are kept in the file; its options are the program's, and a second handle on it in one process takes the same ones or is ErrInvalid.

### OpenSchedule

```go
func OpenSchedule(ctx context.Context, store *Store, name string, repeat Repeat, options ...QueueOption) (
	*Queue[struct{}], error,
)
```

OpenSchedule opens a queue whose one job, under the queue's name, repeats: the repeat in the program is the one kept, so a changed Daily takes effect on the next open. Work it as any queue; a job's At is the time it runs for.

### Queue.All

```go
func (q *Queue[V]) All(ctx context.Context, query Query) iter.Seq2[Entry[V], error]
```

All walks what query names a page at a time, holding no snapshot between pages, so a slow loop keeps no reader open. A job written during the walk may or may not be met.

### Queue.Cancel

```go
func (q *Queue[V]) Cancel(ctx context.Context, key string) (bool, error)
```

Cancel removes the job under key, whether it waits, runs or failed, and says whether there was one. A running job's handler sees its context end with ErrCancelled and what it returns settles nothing; one a Work loop keeps for a busy worker never starts. A repeating job stops repeating.

### Queue.Claim

```go
func (q *Queue[V]) Claim(ctx context.Context, options ...ClaimOption) (Job[V], bool, error)
```

Claim leases the next due job to the caller for the queue's Lease, or jobs.Lease's, and says whether one was due; it does not wait. The caller settles it, or the lease ends and another claim may take it. A job whose value no longer reads into V fails for good, and the claim takes the next.

### Queue.Enqueue

```go
func (q *Queue[V]) Enqueue(ctx context.Context, value V, options ...EnqueueOption) error
```

Enqueue adds a job that runs at jobs.At or jobs.After, or now, and returns once the job is in the file. A repeat needs a key.

Under a key whose job waits it adds nothing, and can bring that job forward but never back. Under a key whose job runs it asks for one run more after it. Under a failed one it starts the job again.

### Queue.Enqueued

```go
func (q *Queue[V]) Enqueued(ctx context.Context, value V, options ...EnqueueOption) Change
```

Enqueued is Enqueue as a change of a batch of the database the queues live in, so that the job commits with the batch's rows or not at all:

	err := db.Batch(ctx, func(b *sqldb.Batch) error {
		b.Exec(`update notes set body = ? where id = ?`, body, id)
		b.Add(index.Enqueued(ctx, IndexJob{Note: id}))
		return nil
	})

The queue's store must be opened In that database. The change takes free memory for its value and holds it until the batch ends. Memory that is not free is ErrLimit when the batch weighs the change.

### Queue.Get

```go
func (q *Queue[V]) Get(ctx context.Context, key string) (Entry[V], bool, error)
```

Get reads the job under key from one snapshot: waiting, running or failed, or done while KeepDone keeps its key.

### Queue.Scan

```go
func (q *Queue[V]) Scan(ctx context.Context, query Query) (Page[V], error)
```

Scan reads a page of the queue's jobs from one snapshot; see Query.

### Queue.Update

```go
func (q *Queue[V]) Update(ctx context.Context, key string, value V, options ...EnqueueOption) error
```

Update gives a job that still waits, or failed, a new value, and a new time or repeat when options name one. A failed job waits again with its attempts from zero, now unless they name a time. A job a worker holds, running or claimed ahead for a busy one, a job that ran or was cancelled, or a key that names none, is tinystore.ErrConflict.

### Queue.Watch

```go
func (q *Queue[V]) Watch(ctx context.Context, key string) iter.Seq2[Entry[V], error]
```

Watch yields the job under key as it is, then again each time its state, place, attempt, time, progress or error changes, until it ends: done, failed or cancelled, the last entry it yields. A key that names no job yields nothing. It reads the job at most five times a second, so that a watcher that falls behind sees the latest, and it ends when ctx ends or the store closes.

### Queue.WithTx

```go
func (q *Queue[V]) WithTx(tx *Tx) *Queue[V]
```

WithTx is the queue inside tx: its calls join the transaction. It works in tx's callback only, and is ErrClosed after it.

### Queue.Work

```go
func (q *Queue[V]) Work(ctx context.Context, handle func(context.Context, Job[V]) error, options ...WorkOption) error
```

Work claims the queue's jobs as they fall due and runs handle on each, until ctx ends or the store closes, or, with UntilIdle, until nothing is due and nothing runs. It runs up to Workers goroutines, which it starts itself and waits for before it returns.

A handler's nil acknowledges its job and an error retries it by the queue's policy. A handler that settles its job itself is left as it settled it, and a panic is an error. While a handler runs its lease is extended. A handler stopped by ctx or by Close gives its job back, the attempt not counted.

Each worker's next job is claimed while it runs the one before, and a job claimed is settled in the same write that claims the next ones, so that a queue under load commits once for many jobs.

## QueueOption

```go
type QueueOption interface {
	// contains filtered or unexported methods
}
```

QueueOption changes how a queue treats its jobs; none changes what its bytes mean, so a program may change them between runs.

### Backoff

```go
func Backoff(first, longest time.Duration) QueueOption
```

Backoff is a retry's wait: first after the first failure, doubling after each, never past longest, each a tenth longer or shorter at random. It is one second to an hour unless it says.

### KeepDone

```go
func KeepDone(d time.Duration) QueueOption
```

KeepDone remembers the key of an acknowledged job for d, and an Enqueue under a key that waits, runs or ran within d adds nothing: a key runs once.

### KeepFailed

```go
func KeepFailed(d time.Duration) QueueOption
```

KeepFailed keeps a job that failed for good for d, seven days unless it says.

### MaxAttempts

```go
func MaxAttempts(n int) QueueOption
```

MaxAttempts is how many attempts a job has before it fails for good, 10 unless it says.

### MaxRunning

```go
func MaxRunning(n int) QueueOption
```

MaxRunning is how many of the queue's jobs may run at once, across every Work loop and Claim of the store; past it a claim takes nothing until a job is settled. A Work loop of such a queue claims no job ahead for a busy worker, so that its held jobs are the ones running.

### MaxWaiting

```go
func MaxWaiting(n int64) QueueOption
```

MaxWaiting refuses a job past n in the queue with tinystore.ErrLimit, ten million unless it says. A loop that enqueues without end then stops at a limit and not at a full disk.

## Repeat

```go
type Repeat struct {
	// contains filtered or unexported fields
}
```

Repeat is when a job runs again: a cron expression in a named zone, or an interval. It is kept as text, so that another language reads the same schedule:

	jobs.Daily("03:10", moscow)         10 3 * * * Europe/Moscow
	jobs.Cron("*/15 9-18 * * 1-5", tz)  */15 9-18 * * 1-5 America/New_York
	jobs.Every(15 * time.Minute)        @every 15m

### Cron

```go
func Cron(expr string, zone *time.Location) Repeat
```

Cron repeats a job by a five-field cron expression in zone: minute, hour, day of the month, month and day of the week. A field takes \*, ranges, steps, lists and the names of months and days, or the whole expression is @hourly, @daily, @weekly, @monthly or @yearly. A day of the month and a day of the week both given run on either.

A zone without a name, time.Local, is ErrInvalid, since a schedule kept by it would change meaning on a host in another zone.

### Daily

```go
func Daily(clock string, zone *time.Location) Repeat
```

Daily repeats a job every day at clock, "15:04", in zone.

### Every

```go
func Every(d time.Duration) Repeat
```

Every repeats a job every d, at the multiples of d since the Unix epoch, so that a restart does not shift it: Every(15\*time.Minute) runs at :00, :15, :30 and :45. d is at least a second.

### Repeat.String

```go
func (r Repeat) String() string
```

String is the repeat as jobs.db keeps it.

## SettleOption

```go
type SettleOption interface {
	// contains filtered or unexported methods
}
```

SettleOption names when a retried or snoozed job runs again.

## State

```go
type State int
```

State is where a job is in its life.

### Waiting

```go
const (
	Waiting   State = iota + 1 // until its time, or due and no handler has it yet
	Running                    // a handler, or the caller of Claim, has it
	Failed                     // failed for good, kept KeepFailed
	Done                       // acknowledged; Get finds it while KeepDone keeps its key
	Cancelled                  // taken by Cancel; only a watcher sees it
)
```

### State.String

```go
func (s State) String() string
```

## Store

```go
type Store struct {
	// contains filtered or unexported fields
}
```

### Open

```go
func Open(ctx context.Context, store *tinystore.Store, options Options) (*Store, error)
```

Open opens jobs.db inside the store, or with Options.In a database's file, and gives back the leases a process that died held, counting their attempts. The store closes it and, unless it is Manual, removes every minute what the queues keep no longer.

### Store.Close

```go
func (s *Store) Close(ctx context.Context) error
```

Close stops every Work, which gives back the jobs its handlers had without counting their attempts, waits for the work in flight and closes jobs.db, or leaves a database's file to the database; cancellation stops waiting, not the cleanup. The store calls it: an application closes the store instead.

### Store.Maintain

```go
func (s *Store) Maintain(ctx context.Context) (Maintenance, error)
```

Maintain removes the failed jobs each queue this process opened keeps no longer, the keys its jobs left behind and the done keys past their KeepDone. It removes 10,000 a transaction and runs at most ten transactions of each a call.

The store calls it every minute unless it is Manual. A queue this process has not opened keeps its failed jobs and its keys.

### Store.Snapshot

```go
func (s *Store) Snapshot(ctx context.Context, dir string) ([]tinystore.SnapshotFile, error)
```

Snapshot copies jobs.db into dir while the engine keeps working; queues kept In a database are in that database's copy.

### Store.Tx

```go
func (s *Store) Tx(ctx context.Context, work func(*Tx) error) error
```

Tx runs work in one writer transaction over any queues of jobs.db: nil commits, an error or a panic rolls back. It never spans two engines. A program enqueuing many jobs at once, a push to each member of a group, pays one commit for all of them.

## TimeOption

```go
type TimeOption interface {
	EnqueueOption
	SettleOption
}
```

### After

```go
func After(d time.Duration) TimeOption
```

After runs the job d from now.

### At

```go
func At(t time.Time) TimeOption
```

At runs the job at t by the store's clock; a time in the past runs it now.

## Tx

```go
type Tx struct {
	// contains filtered or unexported fields
}
```

Tx is one transaction of jobs.db, given to the function passed to Store.Tx and used by the goroutine that runs it; a queue works in it through WithTx.

## WorkOption

```go
type WorkOption interface {
	// contains filtered or unexported methods
}
```

### Timeout

```go
func Timeout(d time.Duration) WorkOption
```

Timeout is the deadline of a handler's context, a minute unless it says; past it the attempt failed.

### UntilIdle

```go
func UntilIdle() WorkOption
```

UntilIdle makes Work return once no job is due and none runs, for tests and Manual stores.

### Workers

```go
func Workers(n int) WorkOption
```

Workers is how many handlers Work runs at once, one unless it says; with one a queue runs its jobs in the order of their times.

<!-- Generated by task reference from jobs/. Edit the doc comments there, not this file. -->
