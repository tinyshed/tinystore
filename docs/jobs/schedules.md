# Schedules

A schedule is a job that repeats, such as a cleanup every night at 03:10 or a
report every Monday. A repeating job never runs twice at the same time, and if
your program was down for a while, it runs once when it comes back, not once
for every time it missed.

## Run a job every night

```ts
const cleanup = store.jobs.schedule('cleanup', { daily: '03:10', zone: 'Europe/Berlin' })

await cleanup.work(async job => {
	const cutoff = new Date(job.at.getTime() - 30 * 24 * 3600 * 1000)
	await db.exec`delete from messages where deleted_at < ${cutoff.getTime()}`
})
```

```python
cleanup = store.jobs.schedule("cleanup", tinystore.daily("03:10", "Europe/Berlin"))


async def clean(job: tinystore.Job[None]) -> None:
    cutoff = job.at - timedelta(days=30)
    await db.exec("delete from messages where deleted_at < ?", int(cutoff.timestamp() * 1000))


await cleanup.work(clean)
```

```go
berlin, err := time.LoadLocation("Europe/Berlin")
cleanup, err := jobs.OpenSchedule(ctx, queues, "cleanup", jobs.Daily("03:10", berlin))

err = cleanup.Work(ctx, func(ctx context.Context, job jobs.Job[struct{}]) error {
	_, err := db.Exec(ctx, `delete from messages where deleted_at < ?`, job.At.AddDate(0, 0, -30))
	return err
})
```

A schedule is a queue with one repeating job. `job.at` is the time the run is
for, which is the right time to compute "30 days ago" from, even if the run
starts late.

If you change the schedule in your code, the new schedule applies the next
time the program opens it.

## Repeats

| Repeat | Bun | Python | Go |
|---|---|---|---|
| every day at a time | `{ daily: '03:10', zone }` | `tinystore.daily("03:10", zone)` | `jobs.Daily("03:10", loc)` |
| a cron expression | `{ cron: '0 9 * * 1', zone }` | `tinystore.cron("0 9 * * 1", zone)` | `jobs.Cron("0 9 * * 1", loc)` |
| every interval | `{ every: '15m' }` | `tinystore.every("15m")` | `jobs.Every(15 * time.Minute)` |

A cron expression has five fields: minute, hour, day of the month, month and
day of the week. It supports `*`, ranges, steps and lists, and the shortcuts
`@hourly`, `@daily`, `@weekly` and `@monthly`. An interval runs at multiples of
its length, counted from the Unix epoch, so `every 15m` runs at :00, :15, :30
and :45.

A time zone must have a name, such as `Europe/Berlin`. On a day when daylight
saving time skips an hour, a job scheduled in that hour runs at the first
minute after it. On a day when an hour repeats, the job runs once.

## Never overlaps, never piles up

When a run finishes, the job moves to its next time that is after both its own
time and now. This has two effects:

- A run that takes longer than the interval doesn't start a second run in
  parallel. The next run starts after this one.
- If your program was down all night, the job runs once when it starts, not
  once for every missed time.

A failed run is retried with backoff, but never later than the next scheduled
time. A failure doesn't stop the schedule.

## A repeating job per user

```ts
const digests = store.jobs.queue<Digest>('digests')

await digests.enqueue({ userId: 42 }, {
	key: 'user:42',
	repeat: { daily: '20:00', zone: user.timeZone },
})
```

```python
digests = store.jobs.queue("digests", Digest)

await digests.enqueue(Digest(user_id=42), key="user:42", repeat=tinystore.daily("20:00", user.time_zone))
```

```go
digests, err := jobs.OpenQueue[Digest](ctx, queues, "digests")

err = digests.Enqueue(ctx, Digest{UserID: 42}, jobs.Key("user:42"), jobs.Daily("20:00", userZone))
```

Any job can repeat. A million users' daily digests are a million rows in the
queue, each moving to its next evening when it is done, in each user's own
time zone. A repeating job needs a key, because only the key can stop it:
`cancel` ends it, and `update` changes its value or its repeat.

## See the last run

```ts
const entry = await cleanup.get('cleanup')
entry.at    // the next run
entry.ran   // when the last run started
entry.took  // how long it took, in milliseconds
entry.error // why it failed, if it did
```

```python
entry = await cleanup.get("cleanup")
entry.at     # the next run
entry.ran    # when the last run started
entry.took   # how long it took, in seconds
entry.error  # why it failed, if it did
```

```go
entry, found, err := cleanup.Get(ctx, "cleanup")
entry.At   // the next run
entry.Ran  // when the last run started
entry.Took // how long it took
entry.Err  // why it failed, if it did
```

A schedule's key is its name. This is everything a page of cron jobs shows:
the next run, the last run, its duration and its error.

## See also

- [Keys](keys.md): cancel or change a repeating job by its key.
- [jobs/README.md](../../jobs/README.md): the full contract of repeats.
