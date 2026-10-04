# Instruments

Instruments measure your own program: counters for things that happen, gauges
for values that go up and down, and timers for durations. They keep their
values in memory, and the engine stores the current values every 15 seconds,
so an instrument call is as cheap as incrementing a variable.

## Counters

```ts
const signups = store.metrics.counter('signups_total')

signups.inc()
signups.with({ plan: 'pro' }).inc()
signups.with({ plan: 'pro' }).add(3)
```

```python
signups = store.metrics.counter("signups_total")

signups.inc()
signups.labels(plan="pro").inc()
signups.labels(plan="pro").inc(3)
```

```go
signups := stats.Counter("signups_total")

signups.Inc()
signups.With("plan", "pro").Inc()
signups.With("plan", "pro").Add(3)
```

A counter only goes up. It counts from zero when your program starts, so a
restart looks like a reset. [Aggregates](aggregates.md) such as `increase` and
`rate` handle resets, so you never have to.

Labels follow each language's Prometheus client: `with({...})` in Bun,
`labels(...)` in Python and `With(key, value, ...)` in Go. The same labels in
any order are the same series.

## Gauges

```ts
const depth = store.metrics.gauge('queue_depth')

depth.set(12)
depth.inc()
depth.dec()

store.metrics.gaugeFunc('open_tickets', async () => await db.scalar`select count(*) from tickets where open`)
```

```python
depth = store.metrics.gauge("queue_depth")

depth.set(12)
depth.inc()
depth.dec()

store.metrics.gauge_func("open_tickets", lambda: db.scalar("select count(*) from tickets where open"))
```

```go
depth := stats.Gauge("queue_depth")

depth.Set(12)
depth.Add(1)
depth.Add(-1)

stats.GaugeFunc("open_tickets", func(ctx context.Context) (float64, error) {
	n, err := sqldb.Scalar[int](ctx, db, `select count(*) from tickets where open`)
	return float64(n), err
})
```

A gauge stores its current value at each write, every 15 seconds, so its
resolution is 15 seconds. A gauge function is called at each write instead,
which suits values that you read from somewhere else, such as a database.

## Timers

```ts
const latency = store.metrics.timer('http_request_ms')

const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
latency.record(12.5) // add a duration of 12.5 milliseconds yourself
```

```python
latency = store.metrics.timer("http_request_ms")

async with latency.labels(route="/users").measure():
    user = await users.get(user_id)
latency.record(0.0125)  # add a duration of 12.5 milliseconds yourself
```

```go
latency := stats.Timer("http_request_ms")

defer latency.With("route", "/users").Since(time.Now())
latency.Record(12500 * time.Microsecond) // add a duration yourself
```

`measure` returns what your function returns, or throws what it throws, and
records the duration either way. In Python, `measure()` works with `with` and
`async with`. A bare number is milliseconds in Bun and seconds in Python.

A timer writes three series at every write:

| Series                  | Kind    | Value                                |
|-------------------------|---------|--------------------------------------|
| `http_request_ms_count` | counter | how many durations were measured     |
| `http_request_ms_sum`   | counter | their total, in milliseconds         |
| `http_request_ms_max`   | gauge   | the longest since the previous write |

The mean over any time range is the increase of the sum divided by the
increase of the count. TinyStore has no histograms: a percentile computed from
buckets is an approximation, and this engine only returns exact numbers.

## Describe a metric

```ts
const latency = store.metrics.timer('http_request_ms', { help: 'How long an HTTP request took.' })
const queue = store.metrics.gauge('queue_length', { unit: 'jobs', help: 'Jobs waiting to run.' })

await store.metrics.describe('disk_used', { unit: 'bytes' }) // a name you ingest yourself
await store.metrics.description('queue_length') // { unit: 'jobs', help: 'Jobs waiting to run.' }
```

```python
latency = store.metrics.timer("http_request_ms", help="How long an HTTP request took.")
queue = store.metrics.gauge("queue_length", unit="jobs", help="Jobs waiting to run.")

await store.metrics.describe("disk_used", unit="bytes")  # a name you ingest yourself
await store.metrics.description("queue_length")  # Description(unit="jobs", help="Jobs waiting to run.")
```

```go
latency := stats.Timer("http_request_ms", metrics.Help("How long an HTTP request took."))
queue := stats.Gauge("queue_length", metrics.Unit("jobs"), metrics.Help("Jobs waiting to run."))

err := stats.Describe(ctx, "disk_used", metrics.Unit("bytes")) // a name you ingest yourself
description, err := stats.Description(ctx, "queue_length") // {Unit: "jobs", Help: "Jobs waiting to run."}
```

A description tells a reader of the data what a metric means: the unit of its
values, such as `ms`, `bytes` or `%`, and a line of help. A dashboard can use
the unit to pick an axis. The description belongs to the name, so every series
of the name shares it, and it stays after the series are deleted.

An instrument writes its description at its next write. A timer describes its
three series: the sum and the longest in milliseconds, and the count without a
unit. `describe` replaces the description a name had, and a `describe` with
neither a unit nor help deletes it. A name that was never described has an
empty unit and help.

A unit can have 32 bytes and help 1024 bytes. A longer one is rejected with an
invalid error (`ErrInvalid` in Go, `InvalidError` in Bun and Python). In Bun
and Python, the instrument call itself throws it.

## When a series is rejected

A series can be rejected, for example when its labels are invalid or the store
already has the maximum number of series. Then only that series is left out:
it is logged once, and the other instruments keep being written. In Bun and
Python, the failure is printed once on stderr.

## When values are written

Instruments write every 15 seconds (`Options.Flush` in Go) and when the store
closes, so the last values aren't lost on a clean shutdown. A crash loses at
most the last 15 seconds. In a Go store opened with `Manual`, call
`stats.Flush(ctx)` yourself.

## Limits and defaults

|                   |                                                  |
|-------------------|--------------------------------------------------|
| Write interval    | 15 seconds                                       |
| Series            | 100,000                                          |
| A label name      | 256 bytes; names starting with `__` are reserved |
| A label value     | 4 KiB                                            |
| Labels per series | 128 pairs and 16 KiB in total                    |
| A unit            | 32 bytes                                         |
| Help              | 1024 bytes                                       |

## See also

- [Aggregates](aggregates.md): turn counters into rates and totals.
- [metrics/README.md](../../metrics/README.md#instruments): the full contract
  of instruments.
