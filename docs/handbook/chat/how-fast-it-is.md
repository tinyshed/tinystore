# How fast it is

How long does the model take to answer, and is it slower today than last
week? How many replies fail? This chapter counts replies and times each model
call, then reads the numbers back as rates, averages and percentiles. It uses
the Metrics engine, which keeps samples in `metrics.db` exactly as they were
measured.

## Open the instruments

```ts
const replies = store.metrics.counter<'model' | 'outcome'>('chat_replies_total', { help: 'Replies written.' })
const modelTime = store.metrics.timer<'model'>('chat_model_ms', {
	buckets: [250, 500, 1000, 2500, 5000, 10000, 30000],
})
const waiting = store.metrics.gauge('chat_replies_waiting', { unit: 'jobs' })
```

```python
replies = store.metrics.counter("chat_replies_total", help="Replies written.")
model_time = store.metrics.timer("chat_model_ms", buckets=[250, 500, 1000, 2500, 5000, 10000, 30000])
waiting = store.metrics.gauge("chat_replies_waiting", unit="jobs")
```

```go
replies, err := metrics.OpenCounter(store, "chat_replies_total", metrics.Help("Replies written."))
modelTime, err := metrics.OpenTimer(store, "chat_model_ms", metrics.Buckets(250, 500, 1000, 2500, 5000, 10000, 30000))
waiting, err := metrics.OpenGauge(store, "chat_replies_waiting", metrics.Unit("jobs"))
```

```rust
let stats = store.metrics()?;
let replies = stats.counter("chat_replies_total").help("Replies written.").open()?;
let model_time = stats
    .timer("chat_model_ms")
    .buckets(Buckets::Bounds(vec![250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0]))
    .open()?;
let waiting = stats.gauge("chat_replies_waiting").unit("jobs").open()?;
```

| Instrument | Keeps                                                          | In the chat                           |
|------------|----------------------------------------------------------------|---------------------------------------|
| counter    | a total that only goes up                                      | replies written, by model and outcome |
| timer      | how many things took how long: count, sum, longest, and spread | each model call's duration            |
| gauge      | a value that goes up and down                                  | replies waiting in the queue          |
| histogram  | how many values fell in each range: count, sum, spread         | attachment sizes, below               |

A timer's `buckets` are the upper bounds of its spread in milliseconds. They
make percentiles possible: "95% of model calls took at most 2.5 seconds".

## Measure

In the reply worker:

```ts
const answer = await modelTime.with({ model: 'haiku' }).measure(() => model.answer(history))
replies.with({ model: 'haiku', outcome: 'ok' }).inc()
waiting.set(await replyQueueLength())
```

```python
async with model_time.labels(model="haiku").measure():
    answer = await model.answer(history)
replies.labels(model="haiku", outcome="ok").inc()
waiting.set(await reply_queue_length())
```

```go
timing := modelTime.With("model", "haiku").Start()
answer, err := model.Answer(ctx, history)
timing.Stop()
replies.With("model", "haiku", "outcome", "ok").Inc()
waiting.Set(float64(replyQueueLength()))
```

```rust
let answer = model_time.with(&[("model", "haiku")]).measure(|| model.answer(&history));
replies.with(&[("model", "haiku"), ("outcome", "ok")]).inc();
waiting.set(reply_queue_length() as f64);
```

`with` (`labels` in Python) picks the series by its labels: each set of
labels is a series of its own. A call never waits and never writes: it
changes a number in memory. The instruments send what changed to the store
every 15 seconds, and when the store closes.

Four web processes and a worker that count into `chat_replies_total` count
into one total. The store keeps the totals and adds what each process sends,
so a process that restarts loses only the last 15 seconds it had not sent.

## Sizes, not durations

A histogram is a timer of numbers that are not durations. The chat records
the size of each attachment, to see how large uploads really are:

```ts
const sizes = store.metrics.histogram('chat_attachment_bytes', {
	unit: 'bytes',
	buckets: [10_000, 100_000, 1_000_000, 5_000_000, 20_000_000],
})
sizes.record(file.size)
```

```python
sizes = store.metrics.histogram(
    "chat_attachment_bytes", unit="bytes", buckets=[10_000, 100_000, 1_000_000, 5_000_000, 20_000_000]
)
sizes.record(info.size)
```

```go
sizes, err := metrics.OpenHistogram(store, "chat_attachment_bytes",
	metrics.Unit("bytes"), metrics.Buckets(10_000, 100_000, 1_000_000, 5_000_000, 20_000_000))
sizes.Record(float64(info.Size))
```

```rust
let sizes = stats
    .histogram("chat_attachment_bytes")
    .unit("bytes")
    .buckets(Buckets::Bounds(vec![10_000.0, 100_000.0, 1_000_000.0, 5_000_000.0, 20_000_000.0]))
    .open()?;
sizes.record(info.size as f64);
```

A histogram keeps the same series a timer does: a count, a sum and a count
per bucket, under `chat_attachment_bytes_count`, `_sum` and `_bucket`, so
its percentiles and averages read the same way. A negative value is
refused.

## Read the numbers

How many replies failed in the last hour, by outcome:

```ts
const { groups: outcomes } = await store.metrics.aggregate({
	name: 'chat_replies_total',
	since: '1h',
	op: 'increase',
	by: ['outcome'],
})
// [{ labels: { outcome: 'failed' }, buckets: [{ value: 1, … }] }, { labels: { outcome: 'ok' }, buckets: [{ value: 19, … }] }]
```

```python
outcomes = (await store.metrics.aggregate(name="chat_replies_total", since="1h", op="increase", by=["outcome"])).groups
```

```go
outcomes, err := metrics.Aggregate(ctx, store, metrics.Query{
	Name: "chat_replies_total", Since: time.Hour, Op: metrics.Increase, By: []string{"outcome"},
})
```

```rust
let outcomes = stats.query().name("chat_replies_total").since(Duration::from_hours(1)).by(&["outcome"]).aggregate(Op::Increase)?.groups;
```

How slow the model is, every five minutes:

```ts
const { groups: p95 } = await store.metrics.aggregate({ name: 'chat_model_ms', since: '1h', every: '5m', op: 'p95' })
// [{ labels: {}, buckets: [{ at, lower: 1000, upper: 2500, samples: 312 }, …] }]
const { groups: mean } = await store.metrics.aggregate({ name: 'chat_model_ms', since: '1h', op: 'avg' })
```

```python
p95 = (await store.metrics.aggregate(name="chat_model_ms", since="1h", every="5m", op="p95")).groups
mean = (await store.metrics.aggregate(name="chat_model_ms", since="1h", op="avg")).groups
```

```go
p95, err := metrics.Aggregate(ctx, store, metrics.Query{
	Name: "chat_model_ms", Since: time.Hour, Every: 5 * time.Minute, Op: metrics.Percentile(95),
})
mean, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "chat_model_ms", Since: time.Hour, Op: metrics.Avg})
```

```rust
let p95 = stats.query().name("chat_model_ms").since(Duration::from_hours(1)).every(Duration::from_mins(5)).aggregate(Op::Percentile(95.0))?.groups;
let mean = stats.query().name("chat_model_ms").since(Duration::from_hours(1)).aggregate(Op::Avg)?.groups;
```

An aggregate cuts the range into buckets of `every`, counted from 1970 in
UTC, and answers one exact value per bucket:

| `op`           | Of                 | A bucket's answer                                                   |
|----------------|--------------------|---------------------------------------------------------------------|
| `increase`     | a counter          | how much it grew, resets counted                                    |
| `rate`         | a counter, a timer | that per second                                                     |
| `avg`, `sum`   | a gauge, a timer   | the exact average and sum                                           |
| `min`, `max`   | a gauge, a timer   | the least and the greatest                                          |
| `last`         | a gauge            | its last value in the bucket                                        |
| `p50`, `p95` … | a gauge, a timer   | a gauge's sample at that rank; a timer's bucket, `lower` to `upper` |

A timer's percentile answers with the two bounds of the bucket that holds
it: `lower: 1000, upper: 2500` means "the 95th percentile lies between one
and two and a half seconds". It does not invent a single number between them,
because the store never measured one. `estimate: true` adds an interpolated
point, named so that nobody mistakes it for a measurement.

## The store's own metrics

Open the store with `selfMetrics` and it measures itself into the same
`metrics.db`: its commits and how long each write waited, the size of each
file, its memory, its disk, and each engine's own numbers.

```ts
await using store = await open('./data', { selfMetrics: true })
```

```python
store = await tinystore.open("data", self_metrics=True)
```

```go
store, err := tinystore.Open(ctx, "data", tinystore.SelfMetrics())
```

```rust
let store = Store::open("data", Options { self_metrics: Some(SelfMetrics::default()), ..Options::default() })?;
```

They are read like your own, under names that start with `tinystore_`:
`tinystore_commit_ms`, `tinystore_jobs_waiting`, `tinystore_jobs_run_ms`,
`tinystore_file_bytes`, `tinystore_memory_used_bytes`,
`tinystore_disk_free_bytes`. A program may not write a name that starts with
`tinystore_`.

## Why this design?

**Why a sample is kept bit for bit.** Many time-series databases round,
downsample or average values as they store them, and a chart then shows
numbers nobody measured. TinyStore keeps every sample as it was given, `-0`
and NaN included, and proves every packing gives the same bits back. Whoever
draws the chart may round; nobody can unround.

**Why an aggregate is exact and rounded once.** A sum over a day adds every
sample of the day exactly, whatever blocks they were packed in, and rounds
the result once. An average is the sum of all samples over their count, never
an average of averages. So the same question gives the same answer, however
the data was packed.

**Why a percentile is two bounds.** A timer counts durations into buckets, so
it knows that 95% of calls took at most 2.5 seconds and more than 1 second,
and nothing finer. Prometheus' `histogram_quantile` interpolates a single
number between the bounds, and that number looks measured but is a guess.
TinyStore answers the interval, and gives the interpolated point only when
asked, under the name `estimate`.

**Why buckets add up.** An `increase` counts each step between two samples in
the bucket where the step ends, and the first bucket takes its step from the
last sample before the range. So 12 buckets of 5 minutes add up to exactly the
hour, and two adjacent hours add up to the two hours. A dashboard that sums
its bars gets the total.

**Why instruments send changes, not totals.** If each process wrote its own
total, four processes would make four series, and a restart would look like a
reset. Each process sends what changed since its last send, and the store
keeps one total. A send that a lost connection repeats is counted once.

## Limits and defaults

| What                                 | Limit                                                      |
|--------------------------------------|------------------------------------------------------------|
| a name's `keep`                      | 30 days unless set                                         |
| an instrument's sets of labels       | 2,000 between two sends; more go to `__overflow`           |
| series in a store                    | 100,000 unless the store's `maxSeries` says                |
| instruments' writes                  | every 15 seconds, at `flush` and when the store closes     |
| a series no process sent for an hour | leaves memory; sent again, it starts a new run with a zero |
| an aggregate                         | 100,000 buckets                                            |

## See also

- [Replies in the background](replies.md): the worker the timer measures.
- [What it is doing](what-it-is-doing.md): logs and events beside the
  numbers.
- [Metrics API reference](../../reference/bun/metrics.md): every instrument, read
  and aggregate.
