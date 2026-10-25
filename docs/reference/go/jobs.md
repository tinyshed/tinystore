# Jobs API for Go

Every public type, function and constant of the Jobs engine in `tinyshed.org/tinystore/jobs`, generated from its source. The [Jobs guide](../../guides/jobs/README.md) explains how to use them, and the [Bun and Node](../bun/jobs.md), [Python](../python/jobs.md) and [Rust](../rust/jobs.md) pages list the same API.

## Scheduled

```go
const (
	Scheduled state = "scheduled"
	Waiting   state = "waiting"
	Running   state = "running"
	Failed    state = "failed"
)
```

The states a list may keep to.

## After

```go
func After(next string) after
```

After starts a page after the page before's Next.

## At

```go
func At(t time.Time) at
```

At is when a job runs.

## Attempts

```go
func Attempts(n int) attempts
```

Attempts is how many times a job runs before it fails for good, the first run counted: 10 unless given.

## Cron

```go
func Cron(expr, timeZone string) cron
```

Cron repeats a job by a cron of five fields in a time zone, an IANA name, "UTC" among them: jobs.Cron("0 20 \* \* \*", "Europe/Berlin").

## Dedupe

```go
func Dedupe(d time.Duration) dedupe
```

Dedupe is how long a done job's id stays taken, so that the same id added again meanwhile adds nothing.

## Delay

```go
func Delay(d time.Duration) delay
```

Delay is how long from now a job runs, on the store's clock.

## Every

```go
func Every(d time.Duration) every
```

Every repeats a job every span, which needs an id; a schedule's repeat too.

## Group

```go
func Group(g string) group
```

Group is the group a job runs in, which Concurrency's Group bounds.

## GroupRate

```go
func GroupRate(n int, per time.Duration) groupRate
```

GroupRate bounds how many runs of each group of a queue start a span, beside its Rate: jobs.GroupRate(2, time.Second), each account's own limit. It lives in memory, as Rate does.

## ID

```go
func ID(s string) id
```

ID is a job's id, which finds it again and which an add of a taken one adds nothing under.

## Keep

```go
func Keep(d time.Duration) keep
```

Keep is how long a failed job stays: a week unless given.

## Limit

```go
func Limit(n int) limit
```

Limit is how many jobs a page holds: 100, and 1000 at most.

## MaxWaiting

```go
func MaxWaiting(n int) maxWaiting
```

MaxWaiting is how many jobs a queue holds waiting before an add is a Limit error.

## Prefix

```go
func Prefix(p string) prefix
```

Prefix lists the jobs whose ids start with it.

## Rate

```go
func Rate(n int, per time.Duration) rate
```

Rate bounds how many runs of a queue start a span: jobs.Rate(30, time.Second).

## Step

```go
func Step[R any](ctx context.Context, run *Run, name string, fn func(ctx context.Context) (R, error)) (R, error)
```

Step runs fn once in the job's run and keeps its answer, as JSON, across the job's attempts: the attempt after a retry, a lost worker or a restart gets the answer back without running fn again. A step whose attempt ends before its answer is kept runs again, so what fn does outside the store should bear doing twice. A name is the step's within its job, numbered in a loop: "model:1", "tool:1".

## Timeout

```go
func Timeout(d time.Duration) timeout
```

Timeout is how long one run may take: a minute unless given. Past it the run's ctx ends, and what the handler then returns settles the job.

## AddOption

```go
type AddOption interface {
	// contains filtered or unexported methods
}
```

AddOption is what an Add gives besides its value: an id, a time, a group, a repeat.

## Backoff

```go
type Backoff struct {
	Initial, Max time.Duration
}
```

Backoff is the wait before a retry: Initial, doubling each time up to Max, a tenth longer or shorter at random; 1 s doubling to 1 h unless given.

## Concurrency

```go
type Concurrency struct {
	Total, Group int
}
```

Concurrency is how many jobs of a queue run at once: Total across every worker of the store, Group in each group, so that one customer's backlog holds back no other. Given to Work, Total is how many handlers that worker runs.

## Job

```go
type Job[T any] struct {
	// ID is "" for a job added without one.
	ID    string
	Value T
	State State
	// At is when it runs next; for one done or failed, when its last run was for.
	At time.Time
	// Attempt counts its attempts, a running one included.
	Attempt int
	// Ahead counts the jobs that run before a waiting one, up to 10,000.
	Ahead int
	// Progress is what its handler last reported of a running job, as JSON.
	Progress json.RawMessage
	// Error is why its last run failed.
	Error string
	Group string
	// Repeat is a repeating job's repeat as it keeps it: "@every 30s +6178ms", "10 3 * * * Europe/Berlin".
	Repeat string
	// LastRun is when the last run a handler finished started and ended; nil before one.
	LastRun *LastRun
}
```

Job is a job as its queue holds it.

## LastRun

```go
type LastRun struct {
	StartedAt, EndedAt time.Time
}
```

LastRun is when a run started and ended.

## ListOption

```go
type ListOption interface {
	// contains filtered or unexported methods
}
```

ListOption is how a page of jobs reads.

## Page

```go
type Page[T any] struct {
	Jobs []Job[T]
	Next string
}
```

Page is a page of a queue's jobs, and where the next starts: "" on the last.

## Queue

```go
type Queue[T any] struct {
	// contains filtered or unexported fields
}
```

Queue is a queue of jobs of one type, run in the order of their time.

### OpenQueue

```go
func OpenQueue[T any](home tinystore.Home, name string, opts ...QueueOption) (*Queue[T], error)
```

OpenQueue opens a queue of jobs of type T, kept as JSON, on a store or on a database whose file then keeps it. It opens at its first call; its error is a name or an option it refuses.

	emails, err := jobs.OpenQueue[Email](store, "emails", jobs.Attempts(20), jobs.Backoff{Initial: 5 * time.Second, Max: 30 * time.Minute})

### Queue.Add

```go
func (q *Queue[T]) Add(ctx context.Context, value T, opts ...AddOption) (bool, error)
```

Add adds a job unless its id is taken, and says whether it added one. An id is taken by a job scheduled, waiting, running or failed, or done while the queue's dedupe keeps it. It returns once the job is on disk; a job without an id is always added. Inside the store's transaction it is kept for the commit, and says true.

### Queue.Adding

```go
func (q *Queue[T]) Adding(value T, opts ...AddOption) sqldb.Statement
```

Adding is a job's add for a batch of the database the queue was opened from: db.Batch writes it with its statements, so the job commits with the rows or not at all, and shares the commit with every other write of the file, where db.Tx holds the writer and pays a sync of its own. It adds as Add does, unless its id is taken.

	err = db.Batch(ctx, sqldb.SQL(`update notes set body = ? where id = ?`, body, id), indexing.Adding(Index{NoteID: id}))

### Queue.All

```go
func (q *Queue[T]) All(ctx context.Context, opts ...ListOption) iter.Seq2[Job[T], error]
```

All is every job the options name, a page at a time, holding nothing between pages.

### Queue.Cancel

```go
func (q *Queue[T]) Cancel(ctx context.Context, id string) (bool, error)
```

Cancel takes the job under id, whatever its state, and says whether there was one: a running one's ctx ends, and what its handler returns then settles nothing; a repeating one stops.

### Queue.Get

```go
func (q *Queue[T]) Get(ctx context.Context, id string) (Job[T], bool, error)
```

Get is where the job under id is, and whether there is one.

### Queue.In

```go
func (q *Queue[T]) In(tx tinystore.InTx) *Queue[T]
```

In is the queue inside a transaction: the store's, whose commit makes its writes with the others, all or none, or a database's, whose writes commit with its rows.

### Queue.List

```go
func (q *Queue[T]) List(ctx context.Context, opts ...ListOption) (Page[T], error)
```

List is a page of the queue's jobs whose ids start with a prefix, or of its failed ones, the last first.

### Queue.Name

```go
func (q *Queue[T]) Name() string
```

Name is the queue's name.

### Queue.RunDue

```go
func (q *Queue[T]) RunDue(ctx context.Context, handler func(ctx context.Context, value T, run *Run) error, opts ...WorkOption) error
```

RunDue runs what is due when it starts and what falls due meanwhile, then returns; it never waits for a later job. Tests, scripts and commands use it.

### Queue.Set

```go
func (q *Queue[T]) Set(ctx context.Context, id string, value T, opts ...SetOption) error
```

Set makes the id's job this value at this time, whatever it was: one waiting is made new, a failed one starts again, and a running one runs once more after this run, with this value.

### Queue.Update

```go
func (q *Queue[T]) Update(ctx context.Context, id string, value T, opts ...SetOption) (bool, error)
```

Update changes the id's job when it has not started, and says whether it did.

### Queue.Watch

```go
func (q *Queue[T]) Watch(ctx context.Context, id string) iter.Seq2[Job[T], error]
```

Watch is the job under id as it is, then again each time it changes; it ends with the job, done, failed or cancelled, a state only a watcher sees. An id with no job yields nothing. A watch whose connection is lost watches again on the next; a job that ended meanwhile ends it without its last state.

### Queue.Work

```go
func (q *Queue[T]) Work(ctx context.Context, handler func(ctx context.Context, value T, run *Run) error, opts ...WorkOption) error
```

Work runs handler on each job as it falls due, until ctx ends or the store closes; then it takes no job more and returns once the handlers under way, each bounded by its timeout, have answered. As many run at once as the queue's concurrency lets, one when it sets none, or Concurrency's Total under it. A worker whose connection is lost reaches the store again, the jobs it held running again as a dead worker's would.

## QueueOption

```go
type QueueOption interface {
	// contains filtered or unexported methods
}
```

QueueOption is how a queue opens.

## Run

```go
type Run struct {
	// ID is the job's id, "" for one added without.
	ID string
	// At is when the run was due, which "thirty days ago" counts from even when it starts late.
	At time.Time
	// Attempt is this run's number among the job's attempts, the first being 1.
	Attempt int
	Group   string
	// contains filtered or unexported fields
}
```

Run is this call on a job: what it is, its steps and progress, and the answers its handler returns. Its handler's ctx ends past its timeout, and when a cancel takes the job.

### Run.Fail

```go
func (r *Run) Fail(reason any) error
```

Fail fails the job for good now, its reason kept as its error: return it.

### Run.Retry

```go
func (r *Run) Retry() error
```

Retry runs the job again after its backoff, the run counted as an attempt: return it.

### Run.RetryAfter

```go
func (r *Run) RetryAfter(d time.Duration) error
```

RetryAfter runs the job again d from now, the run counted as an attempt: return it.

### Run.RetryAt

```go
func (r *Run) RetryAt(t time.Time) error
```

RetryAt runs the job again at a time, the run counted as an attempt: return it.

### Run.SetProgress

```go
func (r *Run) SetProgress(progress any)
```

SetProgress shows how far the job got, anything JSON writes within 4 KiB: the latest goes, ten a second at most.

### Run.Snooze

```go
func (r *Run) Snooze(d time.Duration) error
```

Snooze runs the job again d from now, the run not counted: return it.

### Run.SnoozeUntil

```go
func (r *Run) SnoozeUntil(t time.Time) error
```

SnoozeUntil runs the job again at a time, the run not counted: return it.

## Schedule

```go
type Schedule struct {
	// contains filtered or unexported fields
}
```

Schedule is a repeat the code owns: one repeating job under its name, and the worker that runs it. The code's repeat replaces the one kept each time the program opens it.

### OpenSchedule

```go
func OpenSchedule(home tinystore.Home, name string, opts ...ScheduleOption) (*Schedule, error)
```

OpenSchedule opens a schedule of the store: jobs.Every(d) or jobs.Cron(expr, zone), one of them, and its attempts, backoff and timeout.

	cleanup, err := jobs.OpenSchedule(store, "cleanup", jobs.Cron("10 3 * * *", "Europe/Berlin"))

### Schedule.Cancel

```go
func (s *Schedule) Cancel(ctx context.Context) (bool, error)
```

Cancel removes the schedule's job, until the program opens it again; it says whether there was one.

### Schedule.Get

```go
func (s *Schedule) Get(ctx context.Context) (Job[struct{}], error)
```

Get is when the schedule runs next and how its last run went, what a page of cron jobs shows: its At, LastRun and Error.

### Schedule.RunDue

```go
func (s *Schedule) RunDue(ctx context.Context, handler func(ctx context.Context, run *Run) error) error
```

RunDue runs the schedule's job when it is due, as a queue's RunDue does.

### Schedule.Work

```go
func (s *Schedule) Work(ctx context.Context, handler func(ctx context.Context, run *Run) error) error
```

Work runs handler at each of the schedule's times until ctx ends or the store closes, as a queue's Work does.

## ScheduleOption

```go
type ScheduleOption interface {
	// contains filtered or unexported methods
}
```

ScheduleOption is how a schedule opens: its repeat, attempts, backoff and timeout.

## SetOption

```go
type SetOption interface {
	// contains filtered or unexported methods
}
```

SetOption is what a Set or an Update gives besides its id and value: a time, a group, a repeat.

## State

```go
type State string
```

State is where a job stands: done while its queue's dedupe keeps its id, cancelled only a watcher sees.

### StateScheduled

```go
const (
	StateScheduled State = "scheduled"
	StateWaiting   State = "waiting"
	StateRunning   State = "running"
	StateDone      State = "done"
	StateFailed    State = "failed"
	StateCancelled State = "cancelled"
)
```

A job's states.

## WorkOption

```go
type WorkOption interface {
	// contains filtered or unexported methods
}
```

WorkOption is how a worker runs its handlers.

<!-- Generated by task reference from sdk/go/jobs/. Edit the doc comments there, not this file. -->
