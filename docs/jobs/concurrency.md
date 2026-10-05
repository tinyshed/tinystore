# Concurrency and rate limits

The jobs engine can limit how many jobs of a queue run at the same time, how
many run at once for one customer, and how many start per second. Use these
limits to protect a slow database, to share workers fairly between customers,
and to stay under the rate limit of an API such as Telegram's.

```ts
const refreshes = store.jobs.queue<Refresh>('refreshes', { maxRunning: 8, maxRunningInGroup: 2 })
await refreshes.enqueue({ dataset: 3 }, { key: 'refresh:3', group: 'db:42' })

const telegram = store.jobs.queue<Message>('telegram', { rate: '30/s' })
await telegram.enqueue({ chat: 42, text: 'Your report is ready' })
```

```python
refreshes = store.jobs.queue("refreshes", Refresh, max_running=8, max_running_in_group=2)
await refreshes.enqueue(Refresh(dataset=3), key="refresh:3", group="db:42")

telegram = store.jobs.queue("telegram", Message, rate="30/s")
await telegram.enqueue(Message(chat=42, text="Your report is ready"))
```

```go
refreshes, err := jobs.OpenQueue[Refresh](ctx, queues, "refreshes",
	jobs.MaxRunning(8), jobs.MaxRunningInGroup(2))
err = refreshes.Enqueue(ctx, Refresh{Dataset: 3}, jobs.Key("refresh:3"), jobs.Group("db:42"))

telegram, err := jobs.OpenQueue[Message](ctx, queues, "telegram", jobs.Rate(30, time.Second))
err = telegram.Enqueue(ctx, Message{Chat: 42, Text: "Your report is ready"})
```

All three limits apply across every worker of the store: every `work` loop and
every `claim`, in Go, Bun and Python. Every program that uses the queue at the
same time must give it the same options. Otherwise its calls fail with an
invalid error (`ErrInvalid` in Go, `InvalidError` in Bun and Python).

| Limit                       | Bun                    | Python                   | Go                           |
|-----------------------------|------------------------|--------------------------|------------------------------|
| running jobs of the queue   | `maxRunning: 8`        | `max_running=8`          | `jobs.MaxRunning(8)`         |
| running jobs of each group  | `maxRunningInGroup: 2` | `max_running_in_group=2` | `jobs.MaxRunningInGroup(2)`  |
| job starts per second       | `rate: '30/s'`         | `rate="30/s"`            | `jobs.Rate(30, time.Second)` |
| a job's group, on `enqueue` | `group: 'db:42'`       | `group="db:42"`          | `jobs.Group("db:42")`        |

## Limit the jobs of one customer

Give each job a group, for example the customer database that it refreshes.
With `maxRunningInGroup: 2`, at most two jobs of one group run at the same
time. The other groups are not slowed down: when a group is full, workers take
the jobs of other groups.

When a job of a full group finishes, the next job of that group runs, in the
order of their times. A job without a group is limited only by `maxRunning`.

A group's waiting jobs are set aside while the group is full. A customer with
thousands of waiting jobs doesn't slow down the claims of other customers. A
job of another customer runs as soon as a worker is free, even if it was
enqueued after them.

`update` moves a waiting job to another group when you give it a new group. A
failed job keeps its group when you start it again with `update` or
`enqueue`.

## Stay under an API's rate limit

`rate: '30/s'` lets at most 30 jobs of the queue start in any one second. When
30 jobs have started in the last second, workers wait until the oldest of them
is one second old, then take the next job. A retry counts as a start too.

In Bun and Python, a rate is text: `'30/s'`, `'100/m'`, `'5/10s'`. In Go, it is
a count and a duration: `jobs.Rate(30, time.Second)`.

The engine counts starts in memory. After a restart, it doesn't remember the
jobs that started just before, so up to 30 more jobs can start in the first
second.

> [!NOTE]
> **Workers don't hold extra jobs**
> A queue with `maxRunning`, `maxRunningInGroup` or `rate` doesn't claim a job
> ahead for a busy worker, so that every job a worker holds is running. This
> keeps the limits exact, but costs a little throughput for short jobs.

## Test with untilIdle

With `untilIdle`, `work` returns when no job may start now: no job is due, or
the rate allows no more starts for now. In a test with a clock you control,
move the clock forward and call `work` again to run the next jobs. See
[Testing](../running/testing.md).

## Limits and defaults

|                        |                                               |
|------------------------|-----------------------------------------------|
| Running jobs per queue | unlimited, unless you set `maxRunning`        |
| Running jobs per group | unlimited, unless you set `maxRunningInGroup` |
| Job starts per span    | unlimited, unless you set `rate`              |
| A group's name         | 1 to 1,024 bytes                              |
| A rate's span          | at least 1 millisecond                        |

## See also

- [Watching a job](watching.md): show users their place in the queue.
- [jobs/README.md](../../jobs/README.md): the full contract of the limits.
