# Schedules and cron

A job can repeat: every 30 seconds, or at 20:00 each day in a user's time
zone. A repeat that the data owns, a user's daily digest, is a job with an id
and `every` or `cron`. A repeat that the code owns, the nightly cleanup, is a
schedule: a name, a time and its handler.

```ts
await probes.set(`probe:${site.id}`, { url: site.url }, { every: '30s' })
await digests.set(`user:${user.id}`, { userId: user.id }, { cron: '0 20 * * *', timeZone: user.timeZone })
await digests.cancel(`user:${user.id}`) // the repeat ends

store.schedule('cleanup', { cron: '10 3 * * *', timeZone: 'Europe/Berlin' }, async run => {
	await purgeDeletedBefore(daysBefore(run.at, 30))
})
```

```python
await probes.set(f"probe:{site.id}", Probe(url=site.url), every="30s")
await digests.set(f"user:{user.id}", Digest(user_id=user.id), cron="0 20 * * *", time_zone=user.time_zone)
await digests.cancel(f"user:{user.id}")  # the repeat ends


async def cleanup(run: tinystore.Run) -> None:
    await purge_deleted_before(run.at - timedelta(days=30))


store.schedule("cleanup", cleanup, cron="10 3 * * *", time_zone="Europe/Berlin")
```

```go
err = probes.Set(ctx, "probe:"+site.ID, Probe{URL: site.URL}, jobs.Every(30*time.Second))
err = digests.Set(ctx, fmt.Sprintf("user:%d", user.ID), Digest{UserID: user.ID}, jobs.Cron("0 20 * * *", user.TimeZone))
_, err = digests.Cancel(ctx, fmt.Sprintf("user:%d", user.ID)) // the repeat ends

cleanup, err := jobs.OpenSchedule(store, "cleanup", jobs.Cron("10 3 * * *", "Europe/Berlin"))
err = cleanup.Work(ctx, func(ctx context.Context, run *jobs.Run) error {
	return purgeDeletedBefore(ctx, run.At.AddDate(0, 0, -30))
})
```

```rust
probes.id(&format!("probe:{}", site.id)).every(Duration::from_secs(30)).set(&probe)?;
digests.id(&format!("user:{}", user.id)).cron("0 20 * * *", &user.time_zone).set(&digest)?;
digests.cancel(&format!("user:{}", user.id))?; // the repeat ends

let cleanup = store.schedule("cleanup").cron("10 3 * * *", "Europe/Berlin").work(|run| purge(run.at()))?;
```

## `every` and `cron`

| Repeat            | Runs                                                       | For                               |
|-------------------|------------------------------------------------------------|-----------------------------------|
| `every: '30s'`    | every 30 seconds                                           | work whose moment does not matter |
| `cron` and a zone | at the wall-clock times the five fields name, in that zone | "at 20:00 for each user"          |

- Many ids repeating `every` 30 seconds do not run in the same instant: each
  id has its own phase within the interval, the same across restarts.
- `cron` is five fields, minute, hour, day of the month, month and day of the
  week, or `@hourly`, `@daily`, `@weekly` and `@monthly`. It needs a time
  zone, an IANA name such as `Europe/Berlin` or `UTC`. A zone given as
  `undefined` is `invalid`, not UTC.
- A time that daylight saving skips runs at the first minute after it, and
  one it repeats runs once.

## How a repeat behaves

- A repeat moves to its next time after it ran, and never overlaps itself.
- After a night down, it runs once when the program comes back, not once for
  every missed time. It does so even when the deploy that brought it back
  changed its repeat: the missed run runs now, then the new repeat takes over.
- A failed run retries with its backoff, never past the next time, and the
  repeat goes on. So does a run whose handler killed its process on every
  attempt: past its `attempts`, the next time starts with the error kept.
- A schedule's handler gets the run alone; `run.at` is when the run was due,
  so "thirty days ago" counts from the night it belongs to even if it starts
  late.
- The code's schedule replaces the one kept each time the program opens it,
  so a schedule changed in the code is the one that runs. `stop`, `get` and
  `runDue` work on a schedule as on a queue.

## A repeat that ends: ttl and expiresAt

A job can have a term. `ttl` counts from the job's last write: an `upsert`
renews it, so a job upserted on every sighting stops 14 days after the last
one. `expiresAt` is a fixed time. These are the words a KV key uses for the
same thing.

```ts
await polls.upsert(`group:${g}`, { group: g }, { every: '3m', ttl: '14d' }) // each sighting renews the 14 days
await reports.add({ term }, { id: `report:${term}`, every: '1d', expiresAt: endOfTerm })
```

```python
await polls.upsert(f"group:{g}", Poll(group=g), every="3m", ttl="14d")
await reports.add(Report(term=term), id=f"report:{term}", every="1d", expires_at=end_of_term)
```

```go
_, err = polls.Upsert(ctx, "group:"+g, Poll{Group: g}, jobs.Every(3*time.Minute), jobs.TTL(14*24*time.Hour))
_, err = reports.Add(ctx, Report{Term: term}, jobs.ID("report:"+term), jobs.Every(24*time.Hour), jobs.ExpiresAt(endOfTerm))
```

```rust
polls.id(&format!("group:{g}")).every(Duration::from_mins(3)).ttl(Duration::from_hours(24 * 14)).upsert(&poll)?;
reports.id(&format!("report:{term}")).every(Duration::from_hours(24)).expires_at(end_of_term).add(&report)?;
```

- A job doesn't start at or after its term. A run under way when the term
  comes finishes; then the job ends instead of moving to its next time. A
  retry past the term doesn't run (contract JOB-19).
- A job that reaches its term leaves the queue as a cancelled one does: its
  id is free, `dedupe` or not, and a watcher's last state is `cancelled`.
- `upsert`, `set` and `update` renew a `ttl` they name. An `add` of a taken
  id writes nothing and renews nothing, so call `upsert` on every sighting.
  An `upsert` writes a renewal at most once per thirtieth of the `ttl`, so a
  hot path doesn't write each time.
- A term works on a job that doesn't repeat too: a push not sent within the
  hour is dropped. Neither option needs an id; a repeat always does.
- A `ttl` of zero, and `ttl` beside `expiresAt`, are `invalid`. An
  `expiresAt` already past is written, and the job is gone at once.
- `get` and pages show the term as `expiresAt` (`expires_at` in Python and
  Rust, `ExpiresAt` in Go). A job past its term is gone for them at once; a
  claim or the minute's maintenance deletes its row.

A different interval by night is two ids, each with a cron for its hours,
both upserted with the same `ttl`:

```ts
await polls.upsert(`group:${g}:day`, { group: g }, { cron: '*/3 7-21 * * *', timeZone: 'Europe/Moscow', ttl: '14d' })
await polls.upsert(`group:${g}:night`, { group: g }, { cron: '*/30 22-23,0-6 * * *', timeZone: 'Europe/Moscow', ttl: '14d' })
```

## Run a schedule now

`runNow` runs a schedule once as soon as a worker is free: after a first
deploy that should not wait for the night, or when an admin asks.

```ts
const night = store.schedule('night', { cron: '30 3 * * *', timeZone: 'Europe/Moscow' }, crawl)
await night.runNow() // one run as soon as a worker is free; 03:30 stays its time
if (!(await night.get())?.lastRun) await night.runNow() // the first deploy: run once if it never ran
```

```python
night = store.schedule("night", crawl, cron="30 3 * * *", time_zone="Europe/Moscow")
await night.run_now()
if not (job := await night.get()) or job.last_run is None:
    await night.run_now()
```

```go
night, err := jobs.OpenSchedule(store, "night", jobs.Cron("30 3 * * *", "Europe/Moscow"))
err = night.RunNow(ctx)
```

```rust
let night = store.schedule("night").cron("30 3 * * *", "Europe/Moscow").work(crawl)?;
night.run_now()?;
```

- The run follows the schedule's own rules: one at a time, its run recorded
  in `lastRun`, `run.at` the time you asked for it.
- Its later times stay as they were. A `runNow` at 03:29 of a schedule at
  03:30 runs again at 03:30.
- A schedule that is running runs once more right after the run under way,
  never beside it. One waiting for a retry runs that retry now, its attempts
  going on (contract JOB-21).
- It needs no worker in the same process: whichever worker runs the
  schedule picks it up.
- A schedule whose job `cancel` took is `not_found` until the program opens
  it again. On a queue, run a job now with `update(id, value, { delay: '0s' })`.

## See also

- [Jobs](README.md): the engine's overview.
- [Every night](../../handbook/chat/keeping-it.md#every-night): a nightly
  backup with a schedule.
