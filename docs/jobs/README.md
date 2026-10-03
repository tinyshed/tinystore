# Jobs

The jobs engine runs work later, or now but outside the request that asked for
it: a reminder at six, an email in the background, an account deleted in 30
days, a cleanup every night. Jobs are saved to disk, survive crashes, retry
with a growing delay, and run at least once. The engine keeps its data in
`data/jobs.db`.

## Enqueue a job

```ts
type Reminder = { userId: number; text: string }

const reminders = store.jobs.queue<Reminder>('reminders')

await reminders.enqueue({ userId: 42, text: 'Call mom' }, { at: evening })
await reminders.enqueue({ userId: 42, text: 'Drink water' }, { after: '1h' })
```

```python
@dataclass
class Reminder:
    user_id: int
    text: str


reminders = store.jobs.queue("reminders", Reminder)

await reminders.enqueue(Reminder(user_id=42, text="Call mom"), at=evening)
await reminders.enqueue(Reminder(user_id=42, text="Drink water"), after="1h")
```

```go
type Reminder struct {
	UserID int64
	Text   string
}

queues, err := jobs.Open(ctx, store, jobs.Options{}) // data/jobs.db
reminders, err := jobs.OpenQueue[Reminder](ctx, queues, "reminders")

err = reminders.Enqueue(ctx, Reminder{UserID: 42, Text: "Call mom"}, jobs.At(evening))
err = reminders.Enqueue(ctx, Reminder{UserID: 42, Text: "Drink water"}, jobs.After(time.Hour))
```

`enqueue` returns after the job is saved to disk. A job without a time runs as
soon as a worker is free, and a time in the past also means now. The value is
stored as JSON, so a worker in another language can read it.

## Run jobs

```ts
await reminders.work(async job => {
	await push(job.value.userId, job.value.text)
}, { workers: 4 })
```

```python
async def remind(job: tinystore.Job[Reminder]) -> None:
    await push(job.value.user_id, job.value.text)


await reminders.work(remind, workers=4)
```

```go
err = reminders.Work(ctx, func(ctx context.Context, job jobs.Job[Reminder]) error {
	return push(ctx, job.Value.UserID, job.Value.Text)
}, jobs.Workers(4))
```

`work` runs your handler for each job when it is due, with up to `workers`
jobs at the same time. It keeps running until you cancel it or close the
store, so start it once when your program starts.

What the handler does decides what happens to the job:

| The handler | The job |
|---|---|
| returns | is done and removed from the queue |
| throws, or returns an error | is retried later, with a longer delay each time |
| panics (Go) | is retried, with the panic as its error |
| calls `job.fail(err)` | fails for good, without more attempts |
| calls `job.snooze({ after: '1m' })` | runs again later, without counting an attempt |

Jobs run in the order of their time. Jobs with the same time run in the order
you enqueued them. With one worker, the default, a queue runs one job at a
time in that order.

## Retries

A failed job waits 1 second before its next attempt, then 2, then 4, and so
on, up to one hour. Each wait is randomly up to 10% longer or shorter, so that
jobs that failed together don't all retry in the same second. After 10
attempts, the job fails for good.

```ts
const emails = store.jobs.queue<Email>('emails', {
	maxAttempts: 20,
	backoff: { first: '5s', most: '30m' },
})
```

```python
emails = store.jobs.queue("emails", Email, max_attempts=20, backoff=("5s", "30m"))
```

```go
emails, err := jobs.OpenQueue[Email](ctx, queues, "emails",
	jobs.MaxAttempts(20), jobs.Backoff(5*time.Second, 30*time.Minute))
```

A failed job is kept for 7 days with its last error, so you can find it, fix
the problem and start it again. See [Keys](keys.md).

## At least once

A job runs at least once, whatever happens to the process. It can run twice if
the process crashes after the handler did its work but before the job was
marked as done. Make handlers safe to run twice, for example by inserting with
`on conflict do nothing`:

```sql
insert into messages (id, chat, text) values (?, ?, ?) on conflict (id) do nothing
```

Every attempt is counted before it starts. A job that crashes the whole
process every time still fails for good after its attempts, instead of
crashing your program forever.

## Leases

When a worker takes a job, it gets a lease, 30 seconds by default. While the
handler runs, `work` extends the lease every 15 seconds, so a long handler
keeps its job. If the worker's process dies, the lease runs out and another
worker takes the job. The lease limits how long a dead worker holds a job, not
how long a handler may run.

A handler has one minute by default (`timeout` in Bun and Python,
`jobs.Timeout` in Go). After that, its context or signal is cancelled and the
attempt fails.

## In this section

- [Keys](keys.md): find, update and cancel a job, and enqueue a job only once.
- [Schedules](schedules.md): jobs that repeat, by cron or every day at a time.
- [Watching a job](watching.md): show users their place in the queue and the
  progress of their job.
- [Steps](steps.md): keep the finished steps of an AI agent across retries.
- [Jobs and your data](your-data.md): commit a job together with your rows.

## Limits and defaults

| | |
|---|---|
| A value | 1 MiB of JSON |
| A key | 1 to 1,024 bytes |
| Attempts | 10 |
| Retry delay | 1 second, doubling up to 1 hour, ±10% |
| Lease | 30 seconds |
| Handler timeout | 1 minute |
| Failed jobs kept | 7 days |
| Waiting jobs per queue | 10,000,000 |
| A queue name | `[a-z0-9][a-z0-9_-]{0,63}` |

## See also

- [jobs/README.md](../../jobs/README.md): the full contract of the jobs
  engine.
- [design/jobs.md](https://github.com/tinyshed/research/blob/main/tinystore/design/jobs.md)
  in the research repository: why jobs work this way, with measurements.
