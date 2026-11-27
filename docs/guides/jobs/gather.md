# Gather and quiet hours

`gather` collects what arrives under one id into one job of a Jobs queue,
and quiet hours keep a queue's jobs from running at night. Use them when
many events should become one message, sent at a decent hour: grades that
appear one by one, comments on a post, a burst of changes to sync.

## Gather what arrives into one job

```ts
interface Notice { course: number; grade: string }
const batch = store.queue<Notice[]>('notify-batch')

await batch.gather('grades:7', [{ course: 1, grade: 'A' }], { settle: '5m', max: '1h' }) // { added: true, count: 1, at }
await batch.gather('grades:7', [{ course: 2, grade: 'B' }], { settle: '5m', max: '1h' }) // { added: false, count: 2, at }

batch.work(async notices => push(7, `${notices.length} new grades`)) // one push with both
```

```python
batch = store.queue("notify-batch", list[Notice])

await batch.gather("grades:7", [Notice(course=1, grade="A")], settle="5m", max="1h")  # Gathered(added=True, count=1, at=…)
await batch.gather("grades:7", [Notice(course=2, grade="B")], settle="5m", max="1h")  # Gathered(added=False, count=2, at=…)


async def notify(notices: list[Notice], run: tinystore.Run) -> None:
    await push(7, f"{len(notices)} new grades")  # one push with both


worker = batch.work(notify)
```

```go
batch, err := jobs.OpenQueue[[]Notice](store, "notify-batch")

every := jobs.Settle(5 * time.Minute).Max(time.Hour)
gathered, err := batch.Gather(ctx, "grades:7", []Notice{{Course: 1, Grade: "A"}}, every) // {Added: true, Count: 1, At: …}
gathered, err = batch.Gather(ctx, "grades:7", []Notice{{Course: 2, Grade: "B"}}, every)  // {Added: false, Count: 2, At: …}

err = batch.Work(ctx, func(ctx context.Context, notices []Notice, run *jobs.Run) error {
	return push(ctx, 7, fmt.Sprintf("%d new grades", len(notices))) // one push with both
})
```

```rust
use tinystore::jobs::Gather;

let batch = store.queue::<Vec<Notice>>("notify-batch").open()?;
let every = Gather::settle(Duration::from_mins(5)).max(Duration::from_hours(1));
let gathered = batch.gather("grades:7", &vec![Notice { course: 1, grade: "A".into() }], every)?; // added, count 1
let gathered = batch.gather("grades:7", &vec![Notice { course: 2, grade: "B".into() }], every)?; // count 2

let worker = batch.work(|notices: Vec<Notice>, _run| push(7, &format!("{} new grades", notices.len())))?;
```

The job runs `settle` after the last gather under its id. With `max`, it
runs no later than `max` after the gather that started it, however many
gathers follow (contract JOB-23). Each gather answers what it did, so you
need no second call:

| Field   | What it says                                           |
|---------|--------------------------------------------------------|
| `added` | `true` if this gather started the job's list           |
| `count` | how many items the job holds now                       |
| `at`    | when the job is due, which `get` shows as its `at` too |

The items stay in the order they arrived, and none is lost (contract
JOB-22). The store never decodes an item, so a number such as `2.50` keeps
its spelling.

## What happens while the job runs

A run reads its list when it starts. A gather while it runs adds its items
after the run's, and the store remembers how many items the run took. What
happens next depends on how the run ends:

| The run               | The job under the id holds                         | Due                            |
|-----------------------|----------------------------------------------------|--------------------------------|
| ends well             | only the items gathered while it ran               | at their settle time           |
| fails, and will retry | the run's items, then the gathered ones            | its retry or the settle, later |
| fails for good        | only the gathered items; the failure keeps its own | at their settle time           |

No item runs twice after a run that ended well, and none is lost when a run
fails (contract JOB-22). A retry of a list that grew runs again without the
[steps](steps.md) its run kept, since they answered for the shorter list: a
step that sends the list sends the whole of it, the run's items a second
time among them. After a failure for good, the new job shows the
failure's error until its first run ends, as an `add` after a failure does,
and the failed job with the run's items is listed under the `failed` state.
An id keeps one failure: if the new job fails for good too, its failure
replaces the earlier one.

A gather into a job that waits for its retry adds its items to the retry,
which runs at the later of its backoff and the settle: a gather never
brings a retry forward.

## Where a gather is refused

- The value is not a list, or the job already under the id holds a value
  that is not a list: an invalid error that names the queue and the id
  (`ErrInvalid` in Go, `InvalidError` in Bun and Python).
- The list would pass the 1 MiB a job's value may hold: a limit error, and
  nothing changes.
- The job under the id repeats, or the call gives `at`, `delay`, `every` or
  `cron`: an invalid error. A gather takes its time from `settle`.

A done job that its `dedupe` still holds keeps the id, and a gather adds
nothing: `count` is 0. A failed job gives its id up, and the gather starts a
new list, as [`add`](ids.md#four-calls-one-table) does.

In Rust, Bun and Python, `tx.with(batch).gather(…)` (`tx.with_` in Python)
inside a database's transaction gathers with the transaction's commit. The store's own transaction in the SDKs,
`store.tx`, keeps its writes for the commit and answers them after it, so it
has no gather either, and Go's transactions refuse one. A copy of a store on another node refuses a
gather too: gather at the store's home.

## Quiet hours

Grades are gathered for five minutes, and nothing is sent from 23:00 to
07:00 Moscow time. A push that would go out at night goes out at seven.

```ts
const night = { from: '23:00', to: '07:00', timeZone: 'Europe/Moscow' }
const pushes = store.queue<Notice[]>('notify-push', { quiet: night })

await pushes.gather('grades:7', [{ course: 1, grade: 'A' }], { settle: '5m' }) // at 23:58: { at: 07:00 the next morning }
await pushes.add([{ course: 0, grade: 'exam moved' }], { quiet: false }) // this job is not held
```

```python
night = tinystore.Quiet(from_="23:00", to="07:00", time_zone="Europe/Moscow")
pushes = store.queue("notify-push", list[Notice], quiet=night)

await pushes.gather("grades:7", [Notice(course=1, grade="A")], settle="5m")  # at 23:58: at 07:00 the next morning
await pushes.add([Notice(course=0, grade="exam moved")], quiet=False)  # this job is not held
```

```go
night := jobs.Quiet("23:00", "07:00", "Europe/Moscow")
pushes, err := jobs.OpenQueue[[]Notice](store, "notify-push", night)

gathered, err := pushes.Gather(ctx, "grades:7", []Notice{{Course: 1, Grade: "A"}}, jobs.Settle(5*time.Minute)) // at 23:58: At 07:00
added, err := pushes.Add(ctx, []Notice{{Course: 0, Grade: "exam moved"}}, jobs.NoQuiet()) // this job is not held
```

```rust
use tinystore::jobs::{Gather, Quiet};

let night = Quiet::hours("23:00", "07:00", "Europe/Moscow");
let pushes = store.queue::<Vec<Notice>>("notify-push").quiet(night).open()?;

let gathered = pushes.gather("grades:7", &vec![Notice { course: 1, grade: "A".into() }], Gather::settle(Duration::from_mins(5)))?;
let added = pushes.id("exam:3").quiet(Quiet::none()).add(&vec![Notice { course: 0, grade: "exam moved".into() }])?;
```

The store moves a time when it falls inside the hours, and leaves it alone
outside them (contract JOB-24):

| The store gives a job this time | 23:00 to 07:00, the job is due |
|---------------------------------|--------------------------------|
| 22:59                           | 22:59                          |
| 23:00, or any time until 06:59  | 07:00                          |
| 07:00                           | 07:00                          |

This holds for every time the store computes: the time an `add`, a `set`,
an `update` or an `upsert` gives with `at`, `delay` or none, a gather's
settle, a retry's backoff, a snooze and a repeat's next time. An explicit
`at` inside the hours moves too, because the hours are a promise to the
person who gets the message. `get` and the command line show the moved
time, so "why didn't it run?" has an answer.

A job due before the hours that a worker reaches only inside them, at the
end of a backlog, after a busy group gave it its place or from before the
queue had hours, does not run either: the worker's claim moves it to the
hours' end instead.

- A `from` after `to` wraps midnight, as 23:00 to 07:00 does. The same two
  times, a time that is not `HH:MM`, or an unknown zone is an invalid error.
- The hours follow the wall clock of their zone. If daylight saving skips
  the end, such as 02:30 on the night the clock jumps from 02:00 to 03:00,
  the job is due at the end of the skip, 03:00, as a [cron](schedules.md)
  time is.
- A call's own `quiet` wins over the queue's, for that job's first run and
  every later one. `false` in Bun and Python, `jobs.NoQuiet()` in Go and
  `Quiet::none()` in Rust hold the job by no hours at all.
- A call's own hours are kept in its job's row and hold it in every
  process. A `set`, an `update` or an `upsert` that gives none keeps them.
- A queue's hours are given when it opens, like the rest of its options. A
  second open in the same process with other hours is refused. They hold
  only what the process that opened the queue with them does: a process
  that opens it without them claims, retries and repeats its jobs at any
  hour, a job's own hours aside. A write a copy of the store passes to its
  home runs on the queue as the home opened it, so give the hours in the
  home's process too, or on the call.
- Quiet hours win over a gather's `max`: a list whose `max` ends at night
  is sent at the hours' end.

## See also

- [Ids](ids.md): `add`, `set`, `upsert` and what each does under an id
- [Debounce and throttle](ids.md#debounce-and-throttle), for one job whose
  value the last call replaces
