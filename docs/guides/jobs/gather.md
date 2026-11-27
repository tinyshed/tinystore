# Gather and quiet hours

`gather` collects the items that arrive under one id into one job of a Jobs
queue. Quiet hours keep a queue's jobs from running at night. Use them when
many events should become one message that is sent at a reasonable hour.
For example: grades that appear one by one, comments on a post, or a burst
of changes to sync.

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
runs no later than `max` after the gather that started it, no matter how
many gathers follow (contract JOB-23). Each gather returns what it did, so
you don't need a second call:

| Field   | What it means                                       |
|---------|-----------------------------------------------------|
| `added` | `true` if this gather started the job's list        |
| `count` | how many items the job contains now                 |
| `at`    | when the job is due, the same `at` that `get` shows |

The items stay in the order they arrived, and none is lost (contract
JOB-22). The store never decodes an item, so a number such as `2.50` keeps
its exact text.

## What happens while the job runs

A run reads its list when it starts. If a gather comes in while the job
runs, it adds its items after the run's items, and the store remembers how
many items the run took. What happens next depends on how the run ends:

| The run               | The job under the id then contains                        | Due                                            |
|-----------------------|-----------------------------------------------------------|------------------------------------------------|
| succeeds              | only the items gathered while it ran                      | at their settle time                           |
| fails, and will retry | the run's items, then the gathered ones                   | at the retry or the settle, whichever is later |
| fails for good        | only the gathered items, and the failed job keeps its own | at their settle time                           |

After a run succeeds, no item runs twice. When a run fails, no item is lost
(contract JOB-22). If the list grew, the retry runs without the
[steps](steps.md) that the run saved, because those steps worked on the
shorter list. So a step that sends the list sends all of it, including the
run's items a second time. After a failure for good, the new job shows the
failure's error until its first run ends, just as an `add` after a failure
does. The failed job with the run's items is listed under the `failed`
state. An id keeps only one failure: if the new job also fails for good,
its failure replaces the earlier one.

If a job is waiting for its retry, a gather adds its items to that retry.
The retry runs at its backoff time or at the settle time, whichever is
later. A gather never makes a retry run earlier.

## Where a gather is refused

- If the value is not a list, or the job already under the id has a value
  that is not a list, the gather fails with an invalid error that includes
  the queue and the id (`ErrInvalid` in Go, `InvalidError` in Bun and
  Python).
- If the list would grow past 1 MiB, the limit for a job's value, the
  gather fails with a limit error and nothing changes.
- If the job under the id repeats, or the call sets `at`, `delay`, `every`
  or `cron`, the gather fails with an invalid error. A gather gets its time
  from `settle`.

If a done job is still kept by its `dedupe`, it keeps the id, and a gather
adds nothing: `count` is 0. A failed job gives up its id, so the gather
starts a new list, as [`add`](ids.md#four-calls-one-table) does.

In Rust, Bun and Python, you can call `tx.with(batch).gather(…)`
(`tx.with_` in Python) inside a database's transaction. The gather then
commits with the transaction. The SDKs' own store transaction, `store.tx`,
has no gather, because it holds its writes until the commit and returns
their results only after it. Go's transactions refuse a gather. A copy of a
store on another node refuses a gather too, so gather at the store's home.

## Quiet hours

In this example, grades are gathered for five minutes, and nothing is sent
from 23:00 to 07:00 Moscow time. A push that would go out at night goes out
at seven instead.

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

If a job's time falls inside the hours, the store moves it to their end.
Times outside the hours stay as they are (contract JOB-24):

| The store gives a job this time | With 23:00 to 07:00, the job is due |
|---------------------------------|-------------------------------------|
| 22:59                           | 22:59                               |
| 23:00, or any time until 06:59  | 07:00                               |
| 07:00                           | 07:00                               |

This applies to every time the store computes:

- the time that an `add`, a `set`, an `update` or an `upsert` sets with
  `at`, with `delay` or with neither
- a gather's settle time
- a retry's backoff
- a snooze
- a repeat's next time

An explicit `at` inside the hours moves too, because the hours are a
promise to the person who gets the message. `get` and the command line show
the moved time, so you can always see why a job didn't run.

Sometimes a job is due before the hours start, but a worker reaches it only
inside them. For example, it was at the end of a backlog, it waited for a
place in a busy group, or it was added before the queue had hours. That job
doesn't run either. The worker's claim moves it to the end of the hours
instead.

- If `from` is later than `to`, the hours wrap past midnight, as 23:00 to
  07:00 does. Two equal times, a time that is not `HH:MM`, or an unknown
  zone fail with an invalid error.
- The hours follow the wall clock of their zone. If daylight saving time
  skips the end time, such as 02:30 on the night the clock jumps from 02:00
  to 03:00, the job is due at the end of the skip, 03:00, as a
  [cron](schedules.md) time is.
- A call's own `quiet` overrides the queue's, for that job's first run and
  every later one. `false` in Bun and Python, `jobs.NoQuiet()` in Go and
  `Quiet::none()` in Rust mean that no quiet hours apply to the job.
- A call's own hours are saved in its job's row, so they apply in every
  process. A `set`, an `update` or an `upsert` that sets no hours keeps
  them.
- A queue's hours are set when it opens, like its other options. A second
  open in the same process with different hours is refused. The hours apply
  only to the process that opened the queue with them. A process that opens
  the queue without them claims, retries and repeats its jobs at any hour,
  unless a job has its own hours. When a copy of the store passes a write to
  its home, the write runs on the queue as the home opened it. So set the
  hours in the home's process too, or on the call.
- Quiet hours override a gather's `max`: if `max` ends at night, the list is
  sent at the end of the hours.

## See also

- [Ids](ids.md): `add`, `set`, `upsert` and what each does under an id
- [Debounce and throttle](ids.md#debounce-and-throttle), for one job whose
  value the last call replaces
