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
| `every: '30s'`    | every 30 seconds, each id at a phase of its own            | work whose moment does not matter |
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
  every missed time.
- A failed run retries with its backoff, never past the next time, and the
  repeat goes on.
- A schedule's handler gets the run alone; `run.at` is when the run was due,
  so "thirty days ago" counts from the night it belongs to even if it starts
  late.
- The code's schedule replaces the one kept each time the program opens it,
  so a schedule changed in the code is the one that runs. `stop`, `get` and
  `runDue` work on a schedule as on a queue.

## See also

- [Jobs](README.md): the engine's overview.
- [Every night](../../handbook/chat/keeping-it.md#every-night): a nightly
  backup with a schedule.
