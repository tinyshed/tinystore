# Compare now with usual

`compare` tells you whether a call is slower than usual. It computes the
`op` over the newest window and compares it with the median of the same `op`
over the windows before it. It does this in one call that reads one
snapshot. Your program keeps no baseline of its own, and nothing is
refreshed or cached.

```ts
const now = await store.metrics.compare({
	name: 'moodle_call_ms',
	where: { source: 'server' },
	op: 'p50',
	window: '15m',
	against: '7d',
	sameHour: true,
})
// { value: 840, usual: 310, ratio: 2.7, samples: 412, windows: 7 }
```

```python
now = await store.metrics.compare(
    "moodle_call_ms", op="p50", window="15m", where={"source": "server"}, against="7d", same_hour=True
)
# Compared(value=840.0, usual=310.0, ratio=2.7, samples=412, windows=7)
```

```go
now, err := metrics.Compare(ctx, store, metrics.Comparison{
	Name: "moodle_call_ms", Where: metrics.Where{"source": "server"}, Op: metrics.Percentile(50),
	Window: 15 * time.Minute, Against: 7 * 24 * time.Hour, SameHour: true,
})
// now.Value, now.Usual and now.Ratio are *float64: nil when there is none
```

```rust
let now = store
    .metrics()?
    .query()
    .name("moodle_call_ms")
    .filter("source", "server")
    .compare(Op::Percentile(50.0), Duration::from_mins(15))
    .against(Duration::from_hours(7 * 24))
    .same_hour()
    .run()?;
// Compared { value: Some(840.0), usual: Some(310.0), ratio: Some(2.7), samples: 412, windows: 7 }
```

## The windows it compares

`compare` reads `now` once from the store's clock. The newest window is
`[now − window, now]`, so it includes the call's own millisecond, as a read
`since` the window does. The windows before it are:

| Asked      | Windows before                                      | `against: '7d'`, `window: '15m'` |
|------------|-----------------------------------------------------|----------------------------------|
| `sameHour` | `[now − k·24h − window, now − k·24h)`, k = 1 … 7    | the same quarter hour of 7 days  |
| without it | `[now − (k+1)·window, now − k·window)`, k = 1 … 671 | every quarter hour of the week   |

- A day is 24 hours, in no time zone. So in the week after a clock change,
  `sameHour` compares with hours that are one hour off from local time.
- Without `sameHour`, the windows are counted back from the newest one, not
  from a grid that starts in 1970. There can be at most 1000 of them. More
  fails with `invalid`, which asks for a wider window or a shorter
  `against`. With `sameHour`, the window can be a day at most, and `against`
  must be a day at least.
- Each window is one aggregate of the series that the query finds, merged
  the way an aggregate without `by` merges them. So a window that lies
  inside one packed block is answered from the block's summary. The query's
  bounds apply to all the windows together. For example, past 1000 series
  the call fails with `limit`.
- Each window reads the blocks it reaches on its own. If a packed block
  spans several windows, it is decoded once per window, and its samples
  count against the query's bounds each time. So minute windows over blocks
  of several hours read each block dozens of times, and reach `limit`
  sooner than one aggregate over the same span would. A window as long as a
  block reads each block about once. So does `sameHour`, because its
  windows lie a day apart.

## What it answers

| Field     | Is                                              | Absent when                       |
|-----------|-------------------------------------------------|-----------------------------------|
| `value`   | the `op` over the newest window                 | the newest window has no samples  |
| `usual`   | the median of the `op` over the earlier windows | no earlier window has samples     |
| `ratio`   | `value / usual`                                 | either is absent, or `usual` is 0 |
| `samples` | the number of samples in the newest window      |                                   |
| `windows` | how many windows `usual` is based on            |                                   |

A window without samples doesn't count in the median, and it never counts
as zero. A partial window doesn't count either. A window is partial in the
same cases in which an aggregate marks a bucket as partial: retention cut it
short, or the step of a `rate` or an `increase` into its first sample
started at a sample that retention deleted, or after a NaN. `windows` says
how many windows were left. A NaN sample in any window fails with
`invalid`, as in an aggregate, unless the query skips NaN (`skipNaN`). So no
NaN reaches the median. If the count is even, the median is the mean of the
two middle values.

The `op` can be any op that an [aggregate](aggregates.md) accepts for the
series' kind, including percentiles. A timer's percentile lies between two
bounds of its buckets, so it isn't one number that you can compare.
`compare` refuses it with `invalid` unless the query asks for the
`estimate` (`estimate: true`, `estimate=True`, `Estimate: true`,
`.estimate()`). Then it compares the estimate, as the aggregate returns it.

## Alerting with what exists

The store doesn't evaluate alert rules or notify a queue by itself yet.
You can do it now with a schedule. Every minute, the schedule calls
`compare` and stores in a bucket the time when the call became slow. Once
the condition has held for five minutes, it adds a job to your program's own
queue. It adds one more job when the condition clears. After the alert is
added, the bucket stores the time as a negative number, so a later minute
doesn't add a second alert. The job's id makes sure that a run that stopped
between the two writes doesn't add the alert twice.

```ts
const slowSince = store.bucket<number>('slow-since') // negative once its alert was added
const alerts = store.queue<{ slow: boolean; since: number }>('alerts')
store.schedule('slow-calls', { every: '1m' }, async run => {
	const now = await store.metrics.compare({
		name: 'moodle_call_ms', op: 'p50', window: '15m', against: '7d', sameHour: true,
	})
	const [since, at] = [await slowSince.get('moodle'), run.at.getTime()]
	if ((now.ratio ?? 0) > 2) {
		if (since === undefined) await slowSince.set('moodle', at)
		else if (since > 0 && at - since >= 5 * 60_000) {
			await alerts.add({ slow: true, since }, { id: `slow:${since}` })
			await slowSince.set('moodle', -since)
		}
	} else if (since !== undefined) {
		if (since < 0) await alerts.add({ slow: false, since: -since }, { id: `cleared:${-since}` })
		await slowSince.delete('moodle')
	}
})
```

```python
slow_since = store.bucket("slow-since", int)  # negative once its alert was added
alerts = store.queue("alerts", Alert)


async def check(run: tinystore.Run) -> None:
    now = await store.metrics.compare("moodle_call_ms", op="p50", window="15m", against="7d", same_hour=True)
    since, at = await slow_since.get("moodle"), int(run.at.timestamp() * 1000)
    if (now.ratio or 0) > 2:
        if since is None:
            await slow_since.set("moodle", at)
        elif since > 0 and at - since >= 5 * 60_000:
            await alerts.add(Alert(slow=True, since=since), id=f"slow:{since}")
            await slow_since.set("moodle", -since)
    elif since is not None:
        if since < 0:
            await alerts.add(Alert(slow=False, since=-since), id=f"cleared:{-since}")
        await slow_since.delete("moodle")


store.schedule("slow-calls", check, every="1m")
```

```go
slowSince, err := kv.OpenBucket[int64](store, "slow-since") // negative once its alert was added
alerts, err := jobs.OpenQueue[Alert](store, "alerts")
check, err := jobs.OpenSchedule(store, "slow-calls", jobs.Every(time.Minute))
err = check.Work(ctx, func(ctx context.Context, run *jobs.Run) error {
	now, err := metrics.Compare(ctx, store, metrics.Comparison{Name: "moodle_call_ms", Op: metrics.Percentile(50),
		Window: 15 * time.Minute, Against: 7 * 24 * time.Hour, SameHour: true})
	if err != nil {
		return err
	}
	since, held, err := slowSince.Get(ctx, "moodle")
	if err != nil {
		return err
	}
	slow, at := now.Ratio != nil && *now.Ratio > 2, run.At.UnixMilli()
	switch {
	case slow && !held:
		return slowSince.Set(ctx, "moodle", at)
	case slow && since > 0 && at-since >= 5*60_000:
		if _, err = alerts.Add(ctx, Alert{Slow: true, Since: since}, jobs.ID(fmt.Sprint("slow:", since))); err == nil {
			err = slowSince.Set(ctx, "moodle", -since)
		}
	case !slow && held:
		if since < 0 {
			_, err = alerts.Add(ctx, Alert{Since: -since}, jobs.ID(fmt.Sprint("cleared:", -since)))
		}
		if err == nil {
			_, err = slowSince.Delete(ctx, "moodle")
		}
	}
	return err
})
```

```rust
let slow_since = store.bucket::<i64>("slow-since").open()?; // negative once its alert was added
let alerts = store.queue::<Alert>("alerts").open()?;
let stats = store.metrics()?;
let checking = store.schedule("slow-calls").every(Duration::from_mins(1)).work(move |run| -> tinystore::Result<()> {
    let now = stats.query().name("moodle_call_ms").compare(Op::Percentile(50.0), Duration::from_mins(15))
        .against(Duration::from_hours(7 * 24)).same_hour().run()?;
    let (since, at) = (slow_since.get("moodle")?, tinystore::unix_millis(run.at()));
    match (now.ratio.is_some_and(|ratio| ratio > 2.0), since) {
        (true, None) => slow_since.set("moodle", &at)?,
        (true, Some(since)) if since > 0 && at - since >= 5 * 60_000 => {
            alerts.id(&format!("slow:{since}")).add(&Alert { slow: true, since })?;
            slow_since.set("moodle", &-since)?;
        }
        (false, Some(since)) => {
            if since < 0 {
                alerts.id(&format!("cleared:{}", -since)).add(&Alert { slow: false, since: -since })?;
            }
            slow_since.delete("moodle")?;
        }
        _ => {}
    }
    Ok(())
})?;
```

`Alert` is your program's own job type, `{ slow, since }`. The queue's
worker sends it on, for example to a chat or a pager, as any job handler
does.
