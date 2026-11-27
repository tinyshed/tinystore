# Compare now with usual

`compare` answers whether a call is slower than usual: the `op` over the
newest window against the median of the same `op` over the windows before
it, in one call that reads one snapshot. The program keeps no baseline of
its own, refreshes nothing and caches nothing.

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

`now` is read once from the store's clock. The newest window is
`[now − window, now]`, the call's own millisecond in it, as in a read
`since` the window; the windows before it are

| Asked      | Windows before                                      | `against: '7d'`, `window: '15m'` |
|------------|-----------------------------------------------------|----------------------------------|
| `sameHour` | `[now − k·24h − window, now − k·24h)`, k = 1 … 7    | the same quarter hour of 7 days  |
| without it | `[now − (k+1)·window, now − k·window)`, k = 1 … 671 | every quarter hour of the week   |

- A day is 24 hours, in no time zone: in the week after the clocks change,
  `sameHour` compares with hours one off from the local ones.
- Without `sameHour` the windows are counted back from the newest, not from
  a grid since 1970, and at most 1000 of them: more is `invalid`, asking for
  a wider window or a shorter `against`. `sameHour` takes a window of a day
  at most and an `against` of a day at least.
- Each window is one aggregate of the series the query finds, merged as an
  aggregate without `by` merges them, so a window inside one packed block is
  answered from its summary. Every bound of a query holds over all the
  windows together: past 1000 series, for one, it is `limit`.
- Each window reads the blocks it reaches on its own. A packed block that
  spans several windows is decoded once for each of them, and its samples
  count against the query's bounds each time: minute windows over blocks
  of hours read each block dozens of times, and reach `limit` sooner than
  one aggregate of the same span would. A window as long as a block, or
  `sameHour`, whose windows lie a day apart, reads each block about once.

## What it answers

| Field     | Is                                             | Absent when                       |
|-----------|------------------------------------------------|-----------------------------------|
| `value`   | the `op` over the newest window                | the newest window holds no sample |
| `usual`   | the median of the `op` over the windows before | no window before holds a sample   |
| `ratio`   | `value / usual`                                | either is absent, or `usual` is 0 |
| `samples` | the newest window's samples                    |                                   |
| `windows` | how many windows the usual stands on           |                                   |

A window without a sample counts in no median, never as zero, and neither
does a partial one, as an aggregate marks a bucket: retention cut it short,
or a `rate`'s or an `increase`'s step into its first sample began at a
sample retention took or after a NaN. `windows` says how many were left. A
NaN sample in any window is `invalid`, as in an aggregate, unless the query
skips NaN (`skipNaN`), so no NaN reaches the median. An even
count's median is the mean of the two middle values.

The `op` is one an [aggregate](aggregates.md) takes for the series' kind,
percentiles among them. A timer's percentile lies between two bounds of its
buckets, which is no one number to compare: `compare` refuses it as
`invalid` unless the query asks for the `estimate` (`estimate: true`,
`estimate=True`, `Estimate: true`, `.estimate()`), and then compares the
estimate, as the aggregate gives it.

## Alerting with what exists

The store does not yet evaluate alert rules or notify a queue by itself.
A schedule does it now: every minute it compares, keeps since when the call
has been slow in a bucket, and adds a job to the program's own queue once
the condition has held five minutes, and once more when it clears. The
bucket keeps the time negative once the alert was added, so that a later
minute adds no second one; the job's id keeps a run that stopped between the
two writes from adding it twice.

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

`Alert` is the program's own type of a job, `{ slow, since }`; the queue's
worker sends it on, to a chat or a pager, as any job's handler does.
