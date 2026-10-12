# metrics: the API book

Accepted on 11 October 2026 (decision 19), after a newcomer check, a blind
comparison and four readings by a reviewer (below). Numbers that move over
time: a program's requests, its
queue's length, how long its calls take, and samples other programs send;
read back exactly, as buckets of a range and as the newest value of each
series. This book is the API before the code; [dx.md](../dx.md) has the rules
it follows. TypeScript comes first in each section because most callers will
read it; Python, Go and Rust follow when they spell something differently.
The Go engine at `e81a050`, its guides in `docs/metrics/` and research's
[design/metrics.md][design] and [aggregate contract][contract] are what this
ports: its promises are GATES.md's, its storage was measured. Dashbin's asks
for metrics are in it: B1, B2, B3, B4, B6, B7, B12 and B14 with the engine,
A10 with the store's own metrics, and C4, C5 and C6 as the shapes they take
later. How the engine keeps these promises is
[metrics-engine.md](../metrics-engine.md).

[design]: https://github.com/tinyshed/research/blob/main/tinystore/design/metrics.md
[contract]: https://github.com/tinyshed/research/blob/main/tinystore/design/aggregate-contract.md

## What a newcomer learns

| Concept     | In one line                                                                                         |
|-------------|-----------------------------------------------------------------------------------------------------|
| a series    | a name and its labels, `http_requests_total{route="/notes"}`; its samples are times and numbers     |
| a counter   | a total that only goes up, kept by the store, which every process counting into it adds to          |
| a gauge     | a value that goes up and down: a queue's length, a temperature                                      |
| a timer     | how many things took how long: their count, their sum, the longest, and with `buckets` their spread |
| `keep`      | how long a name's samples stay: 30 days unless it says                                              |
| `aggregate` | a range cut into buckets of `every`, each one exact value: an increase, an average, a p95           |
| `latest`    | each series' newest sample, when it has one in the range                                            |

A program that measures itself keeps instruments, which send the store what
changed every 15 seconds; a program that has samples from elsewhere ingests
them. Both write series, and every read finds both.

## Measure the program

```ts
const requests = store.metrics.counter<'route' | 'status'>('http_requests_total', { help: 'Requests served.' })
requests.with({ route: '/notes', status: 200 }).inc()

const depth = store.metrics.gauge('queue_depth', { unit: 'jobs' })
depth.set(12)
store.metrics.gaugeFunc('open_tickets', () => db.scalar`select count(*) from tickets where open`)

const latency = store.metrics.timer('http_request_ms', { buckets: [5, 10, 25, 50, 100, 250, 500, 1000] })
const user = await latency.with({ route: '/users' }).measure(() => users.get(id))
latency.with({ route: '/sync' }).record(performance.now() - started)   // milliseconds
```

```python
requests = store.metrics.counter("http_requests_total", help="Requests served.")
requests.labels(route="/notes", status=200).inc()

latency = store.metrics.timer("http_request_ms", buckets=[5, 10, 25, 50, 100, 250, 500, 1000])
async with latency.labels(route="/users").measure():
    user = await users.get(user_id)
```

```go
requests := metrics.Counter(store, "http_requests_total", metrics.Help("Requests served."))
requests.With("route", "/notes", "status", "200").Inc()

latency := metrics.Timer(store, "http_request_ms", metrics.Buckets(5, 10, 25, 50, 100, 250, 500, 1000))
defer latency.With("route", "/users").Since(time.Now())
```

```rust
let stats = store.metrics()?;
let requests = stats.counter("http_requests_total").help("Requests served.").open()?;
requests.with(&[("route", "/notes"), ("status", "200")]).inc();

let latency = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0]).open()?;
let _timing = latency.with(&[("route", "/users")]).start();   // recorded when it drops
```

- An instrument is made once and kept. `with` gives the same instrument with
  more labels; each set of labels is a series of its own, and the same
  labels in any order are one series. A label's value is text: a number or a
  boolean is kept as its text, `200` as `"200"`.
- A call never waits and never writes: it changes a number in memory. The
  instruments send what changed to the store, which adds it to the totals it
  keeps and writes them every 15 seconds, at `flush()` and when it closes; a
  crash loses at most the last 15 seconds.
- What an SDK sends counts once: a send whose connection was lost goes again
  on the next and counts once; a store that crashed loses what it had not
  written, and what it had no time to answer, rather than count it twice.
  `flush()` resolves once the store has written; when the store cannot be
  reached it fails as a call does, and what it carried goes once it can be.
  What the store refused since the flush before, and what a store that
  started again may have counted, fail the next flush, `invalid` and
  `outcome_unknown`, each once.
- In TypeScript an instrument made with its label names,
  `counter<'route' | 'status'>(…)`, takes only those in `with`, checked as
  they are written; without them it takes any. A duration's text, an `op`
  and a percentile are offered as they are typed, `'15m'`, `'p99'`, and one
  misspelled, `'1hr'`, does not compile. A bucket of `count` … `last` has its
  `value` as a number; a percentile's may have a timer's bounds in its place.
- A value an instrument is given is a finite number: `NaN` or an infinity
  throws `invalid` at once, where the program made it, rather than failing
  every average of its bucket for as long as the name keeps it. `ingest`
  keeps them, bit for bit. In Rust a call returns nothing, as the `metrics`
  crate's do: what it refuses, a value or a label, changes nothing and is
  said once in the store's log.
- A counter's `inc()` adds one and `add(n)` adds `n`; a negative `n` throws
  `invalid` at once. The store keeps a counter's total, and a timer's count,
  sum and buckets, so that processes sharing a store, through a sidecar or a
  server, count into one series: four workers counting
  `http_requests_total{route="/notes"}` count every request once. A program
  that restarts loses only what it had not sent. `add(0)` makes a set of
  labels known with a total of zero, so that a chart shows 0 rather than
  nothing before the first count.
- Each run of a total starts with a zero written at the moment it began, the
  store's start or a series sent again after an hour unused, so that a reset
  is seen even when the new count already passed the old: `2` at 11:00, `0`
  at 12:00 and `5` at 12:15 are an increase of 5, where `2` and `5` alone
  would say 3. The zero never shares a millisecond with the run's first
  count, and a timer's `_count`, `_sum` and `_bucket` start together the same
  way. OpenTelemetry's start time of a point and OpenMetrics' created time
  mean the same, though neither writes it as a sample.
- A gauge's `set`, `inc`, `dec` and `add` change its value, and the store
  keeps its value at each write, so its resolution is 15 seconds. A gauge is
  the state of the process that sets it, written while that process is
  connected and no longer once it leaves: two processes setting one gauge
  under the same labels overwrite each other, so each gives it a label of its
  own, `instance`, and an aggregate adds or ranks them as it asks.
- A timer keeps how many durations it measured and their sum, in
  milliseconds, and the longest of those measured since its last write.
  `measure` runs a function, awaits what it returns, returns or throws what it
  does, and records how long it took either way; `record` takes a duration
  measured elsewhere, a
  number of milliseconds as `performance.now()` counts them, where Python,
  Go and Rust take their language's durations; `start()` gives a timing that
  records when it stops, for a span that is no one function.
- A timer named `http_request_ms` writes the series `http_request_ms_count`
  and `http_request_ms_sum`, counters, and `http_request_ms_max`, a gauge of
  the longest since the write before, left out of a write that measured
  nothing. A query of `http_request_ms` reads them together.
- `histogram(name, { unit, buckets })` is a timer of numbers that are not
  durations, a response's size in bytes, `record(bytes.byteLength)`, with the
  same series.

### Buckets

- `buckets` gives a timer the upper bounds of its spread, and a percentile of
  it becomes possible: a timer without them answers its count, its sum, its
  mean and its longest, and no percentile. `exponentialBuckets(1, 2, 14)`
  gives 1, 2, 4 … 8192: the start, the factor and how many.
- `buckets: 'exponential'`, `Buckets::Exponential` in Rust, lets the store
  keep a timer's spread, an experiment before Prometheus' native histograms:
  each value counts in an interval of their grid, eight a doubling, about 9%
  wide, and an interval no value fell in costs no series. A percentile then
  lies within one interval, with no bound to choose. Past 128 intervals a
  set of labels the store halves the resolution, from that write on. The
  intervals are counted each apart, under a label of the store's own, so the
  heatmap by `le` below is not theirs yet.
- The store adds the bound `+Inf`, so a duration past the last bound is
  counted above it rather than lost.
- A bound is written as JavaScript's `String(n)` writes it, the fewest
  digits that read back as the same number, so that two languages write one
  series: `5` whether a program wrote `5` or `5.0`, `0.25`, `1e+21` past
  10²¹, and `+Inf`.
- With buckets a timer also writes `http_request_ms_bucket`, one series a
  bound with the label `le`, each counting the durations at most that bound,
  as Prometheus writes a histogram's.
- A name's buckets are its own, as its `keep` is: another set while the store
  is open is `invalid`. A program that changes them starts the new set at its
  next write.
- A percentile takes each bucket of time with the bounds its samples were
  written with. Where two sets meet, in the bucket of time of a change or
  while processes of both write, it uses only the bounds both have, `+Inf`
  at least, and its interval widens to say so: `[10, 20, 50, 100]` beside
  `[5, 25, 75, 150]` share none, and give `lower: 0, upper: Infinity`.
  Nothing is moved from one set's buckets into another's.
- The lowest bucket starts at 0: a timer's durations and a histogram's
  values are never below it, and a negative one is `invalid` at once.

### A gauge read from a function

- `gaugeFunc` calls its function at each write, for a value the program reads
  from elsewhere; it may be async, and the write waits for it no longer than
  it waits for the rest.
- A function that has not answered by then writes nothing that time, and
  holds up no other instrument; it is never called again while a call of it
  runs.
- An error, or an answer that is not a number, writes nothing that time,
  never a zero, and is said once while it repeats.
- In Rust the function runs on the store's thread and must not block: a value
  that takes time to read is a gauge the program sets.
- A function reads one set of labels, none unless given; another set of the
  name takes a function of its own, and a set already read is `invalid`, as
  is a name a function reads for any other instrument:

```ts
store.metrics.gaugeFunc('queue_depth', () => emails.depth(), { labels: { queue: 'emails' } })
store.metrics.gaugeFunc('queue_depth', () => sms.depth(), { labels: { queue: 'sms' } })
```

```rust
stats.gauge_func("queue_depth", move || emails.depth()).labels(&[("queue", "emails")]).open()?;
stats.gauge_func("queue_depth", move || sms.depth()).labels(&[("queue", "sms")]).open()?;
```

### How many series

- An instrument takes at most 2000 sets of labels between two sends. A new
  set past them is
  counted in the series whose one label is `__overflow`, `true`, and said
  once, so a label holding a user's id costs no memory past the bound and a
  total over the name stays whole.
- A grouping shows that series as a group of its own, `{ __overflow: 'true'
  }`, whatever `by` names: the groups still add up to the total, and what
  lost its labels is in sight rather than missing from every route.
- A counter's, a timer's or a histogram's series that nothing sent for an
  hour leaves the store's memory and frees its place; sent again, it starts a
  new run with its zero, which `increase` counts as a reset, so nothing is
  lost. A gauge stays while the process that sets it is connected, since its
  last value stands until it is set again.
- A store holds at most 100,000 series. A new series past them is left out
  of the instruments' write and said once, and an `ingest` of one is `limit`.

### What an instrument is told

| Option    | Unless given | What it does                                                                  |
|-----------|--------------|-------------------------------------------------------------------------------|
| `unit`    | none         | the unit of its values, 32 bytes at most: `'bytes'`, `'%'`; a timer's is `ms` |
| `help`    | none         | a line on what it means, 1024 bytes at most                                   |
| `keep`    | the name's   | how long the name's samples stay, as `ingest` takes it                        |
| `buckets` | none         | a timer's or a histogram's upper bounds, ascending                            |

## Ingest

```ts
await store.metrics.ingest({
	name: 'cpu',
	kind: 'gauge',
	labels: { host: 'web-1' },
	samples: [[new Date('2026-10-11T09:00:00Z'), 0.42], [new Date('2026-10-11T09:00:15Z'), 0.47]],
})
await store.metrics.ingest([cpu, memory, disk])                                   // all or none
await store.metrics.ingest({ name: 'cpu', kind: 'gauge', labels, times, values })   // as two columns
await store.metrics.ingest({ name: 'signups_total', kind: 'counter', labels: {}, samples, keep: '730d' })
```

```python
await store.metrics.ingest(name="cpu", kind="gauge", labels={"host": "web-1"}, samples=[(at, 0.42), (later, 0.47)])
```

```go
err := metrics.Ingest(ctx, store, metrics.Series{
	Name: "cpu", Kind: metrics.Gauge, Labels: metrics.Labels{"host": "web-1"},
	Samples: []metrics.Sample{{At: at, Value: 0.42}, {At: later, Value: 0.47}},
})
```

```rust
stats.ingest(&[Series::gauge("cpu").label("host", "web-1").sample(at, 0.42).sample(later, 0.47)])?;
```

- A series is a name, a kind and its labels: `gauge`, `counter`, or
  `histogram` for the `_bucket` series of a histogram written elsewhere,
  each with its `le`, beside its `_sum` and `_count` counters. An `le` is
  written as JavaScript writes its number, whatever spelling it came in,
  `5.0` as `5`, so that two exporters' bounds are one; a histogram's series
  without one, a number of 0 or more or `+Inf`, is `invalid`.
- An ingested series has one writer: two programs ingesting one counter
  under the same labels write over each other's totals, so each gives it a
  label of its own, `instance`. The instruments' totals are the store's, and
  add up; an ingested total is its writer's.
- A counter ingested from a program that started again sends a zero at the
  time it started, as the instruments write one. Without it, a restart whose
  count already passed the old one is counted short, as Prometheus counts
  it.
- A sample is a time and a number, kept bit for bit: `-0`, infinities and a
  NaN's payload come back as they went in. A time is kept to the millisecond;
  a finer one is cut to its millisecond.
- `samples` takes pairs, and `times` with `values` take the same as two
  columns: unix milliseconds or dates, and numbers.
- One call is written whole or not at all. A refused series names itself by
  its labels, so the call can be sent again without it.
- Samples may come in any order, and a time given twice keeps the later
  value, until the store has packed that stretch of the series; a sample
  behind what its series has packed is `too old`, since a packed counter's
  increase would change. A series that comes back after a day away sends its
  backlog: what it has packed follows its own newest sample, not the clock.
- A time is taken from as long ago as its name keeps to ten minutes ahead of
  the store's clock; outside it is `too old` or `too new`.
- `ingest` returns once the samples are written, and a read finds them at
  once. A crash of the program loses none; a power loss may take the last
  ones, since the metrics' file is `'os'` unless the store opened `'full'`.

## A name's own

A name's kind, `keep` and `buckets` are kept in the file with it, as a
stream's `keep` is in records: an instrument, an `ingest` and a scrape that
give one set it for every series of the name.

- What gives none changes nothing. A name nobody gave a `keep` keeps 30
  days.
- While the store is open, giving the same again is fine and giving another
  is `invalid`, naming both, so that one part of a program never shortens
  what another keeps or mixes another spread into its buckets. To change
  one, change the code and start again. A name has one kind while it has
  series: a gauge and a counter of one name is `invalid`.
- A sample older than its name keeps is never read, whether or not it has
  left the file yet, and never written. A bucket that retention cut is
  `partial`.
- A unit and a help are the name's too, and `describe` replaces them at any
  time.

## Read

```ts
const series = await store.metrics.read({ name: 'cpu', where: { host: 'web-1' }, since: '1h' })
for (const { labels, times, values } of series) chart.line(labels.host, times, values)

const failing = await store.metrics.read({
	name: 'http_requests_total',
	since: '1h',
	where: { status: ['500', '502'], env: { not: 'dev' }, host: { prefix: 'api-' } },
})
const host = await store.metrics.read({ where: { host: 'web-1' }, since: '10m' })   // every name of one host
```

```python
failing = await store.metrics.read(name="http_requests_total", since="1h", where={"status": ["500", "502"], "env": {"not": "dev"}})
```

```go
failing, err := metrics.Read(ctx, store, metrics.Query{Name: "http_requests_total", Since: time.Hour,
	Where: metrics.Where{"status": metrics.OneOf("500", "502"), "env": metrics.Not("dev")}})
```

```rust
let failing = stats.query().name("http_requests_total").since(Duration::from_hours(1)).filter("status", ["500", "502"]).filter("env", Not("dev")).read()?;
```

| In a query             | Finds                                                                  |
|------------------------|------------------------------------------------------------------------|
| `name`                 | the series of this name; a timer's name, the series it writes          |
| `where: { host: 'a' }` | a label equal to this value                                            |
| `['500', '502']`       | a label equal to one of these                                          |
| `{ not: 'dev' }`       | a label equal to none of these, `{ not: ['dev', 'test'] }`, or absent  |
| `{ prefix: 'api-' }`   | a label that starts with this text                                     |
| `since`                | the span before now, `'1h'`; or `from` and `to`                        |
| `from`, `to`           | the range's start, included, and its end, excluded; `to` now if absent |

- A query is data: the same object is kept in a dashboard's settings, sent
  over the wire, and read the same way in every language.
- A number or a boolean in `where` is its text, as in `with`: `{ status: 200
  }` finds `"200"`.
- A series comes back with its name, its labels and its samples as two
  columns: `times`, unix milliseconds in a `Float64Array`, which a chart and
  `new Date` take, and `values`, a `Float64Array` whose bytes keep every
  value's bits. Python gives the same two arrays; Go and Rust a slice of
  samples, one allocation a series. A NaN in `values` is a value that was
  stored, never a missing one; read out as a JavaScript number it may lose
  its payload, as the language allows, while the array's bytes keep it.
- Every condition holds. A query needs a `name`, or a label's value, a list
  or a prefix in `where`, to find its series: `not` alone is `invalid`, since
  it would read every series. There are no regular expressions.
- A condition given as `undefined` is `invalid`, never a condition left out,
  as sql's and records' are.
- A read is whole or an error: past what one read may hold, it is `limit`,
  naming which bound, never fewer series or samples. `explain` says what a
  read would cost before it runs.

## Aggregate

```ts
const traffic = await store.metrics.aggregate({
	name: 'http_requests_total',
	since: '24h',
	every: '1h',
	op: 'increase',
	by: ['route'],
})
// [{ name, labels: { route: '/notes' }, buckets: [{ at, value: 1840, samples: 240, resets: 0 }, …] }, …]
// at: where each hour begins

const total = await store.metrics.aggregate({ name: 'http_requests_total', since: '1h', every: '5m', op: 'rate' })
// one answer: without `by`, every series of the name is one

const p95 = await store.metrics.aggregate({ name: 'http_request_ms', since: '1h', every: '5m', op: 'p95' })
// [{ name, labels: {}, buckets: [{ at, lower: 25, upper: 50, samples: 3812 }, …] }]

const mean = await store.metrics.aggregate({ name: 'http_request_ms', since: '1h', op: 'avg', by: ['route'] })
```

```python
traffic = await store.metrics.aggregate(name="http_requests_total", since="24h", every="1h", op="increase", by=["route"])
```

```go
traffic, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "http_requests_total", Since: 24 * time.Hour},
	metrics.Every(time.Hour), metrics.Increase, metrics.By("route"))
```

```rust
let traffic = stats.query().name("http_requests_total").since(Duration::from_hours(24)).every(Duration::from_hours(1)).by(&["route"]).aggregate(Op::Increase)?;
```

| `op`            | Of                        | A bucket's answer                                                                         |
|-----------------|---------------------------|-------------------------------------------------------------------------------------------|
| `count`         | any                       | how many samples; a timer's, how many durations                                           |
| `sum`, `avg`    | a gauge, a timer          | their exact sum; that over their count                                                    |
| `min`, `max`    | a gauge; `max` of a timer | the least and the greatest sample; a timer's longest                                      |
| `increase`      | a counter                 | how much it grew, resets counted                                                          |
| `rate`          | a counter, a timer        | that a second of the bucket; a timer's durations a second                                 |
| `delta`         | a gauge                   | how much it moved: its last sample less the one before its first                          |
| `first`, `last` | a gauge, a counter        | the bucket's first and last sample, bit for bit                                           |
| `p50`, `p95` …  | a gauge, a timer          | a gauge's sample at that rank as `value`; a timer's bucket holding it, `lower` to `upper` |

- A query takes `read`'s conditions and range, and `every`, `op`, `by` and
  `without`. Without `every` the range is one bucket.
- A timer's name reads the series it writes: `count` and `rate` its count's,
  `sum` its sum's, `avg` the one over the other, `max` its longest, a
  percentile its buckets'. A bucket in which it measured nothing counts 0 for
  `count`, `sum` and `rate`, and is left out of `avg`, `max` and a
  percentile, which have nothing to take.
- Buckets are spans of `every` counted from 1970 in UTC, as records' are; a
  range that starts or ends inside one cuts it, and its `rate` divides by
  what is left. `at` is where a bucket begins. A bucket without samples is
  left out.
- `value` is a bucket's answer, but a timer's percentile's, which is two
  bounds. `samples` is how many samples it comes from: a gauge's or a
  counter's samples, a timer's durations. `resets` counts a counter's resets.
- `partial: true` says a bucket's value comes from less than its span of
  `every`: the range cut it, the store's clock has not reached its end, or
  retention cut it. A chart shows such a bucket apart, or leaves the
  unfinished edge out. Its `rate` is a second of what is left of it, and no
  speed the counter ran at: `since: '1h'` asked exactly on the hour ends with
  a bucket of the one millisecond that holds the sample taken now, and the
  step it counts began fifteen seconds before.
- Every value is exact and rounded once: a sum is the exact sum of its
  samples, rounded to the nearest float, whatever order or blocks they were
  kept in. A sum past what a float holds is `Infinity`.
- A gauge's bucket holding a NaN or an infinity is `invalid`, and so is a
  counter's below zero: no aggregate takes a value for missing.
- An answer is whole or an error: past 100,000 buckets, or past a bound of
  `read`'s, it is `limit`.

### Buckets add up to the range

```text
a counter that starts at 10:00 and grows by 1 a second, sampled every 15 s, read 10:00 to 11:00
1h buckets   3585
5m buckets   285 + 300 + 300 + … + 300 = 3585
1m buckets    45 +  60 +  60 + … +  60 = 3585

one already running, with a sample at 09:59:45: the first bucket takes its step from it
1h buckets   3600
5m buckets   300 + 300 + 300 + … + 300 = 3600
```

- An increase, a rate and a delta count each step between two samples in
  the bucket where the step ends. The first bucket takes its step from the
  newest sample before the range, so buckets of any width add up to the
  range, and two adjacent ranges to the range they make.
- An increase is what the samples show the counter grew between them, and
  it is counted where it was seen. Nothing is interpolated or extrapolated
  to a bucket's edges.
- A step from a sample that retention removed is not counted, and its bucket
  is `partial`.

### Across a gap

```text
http_requests_total   100 on 1 October, nothing since, 300 at 11:59 on 11 October
11:00 to 12:00        { value: 200, gap: true }
```

- A bucket whose first step began before the bucket before it says `gap:
  true`: a whole bucket of `every` between had no samples, and what the
  counter grew over it is counted here, since when it grew is not known.
- The yardstick is the query's `every`, not a guess at how often a series
  writes: samples every 15 seconds in buckets of a minute never make a gap;
  hourly samples in hourly buckets make one only where an hour is missing;
  hourly samples in buckets of 15 minutes mark every bucket, which says the
  `every` is finer than the samples.
- A `rate` there is that increase over the bucket's width, which is no speed
  the counter ran at. A chart shows such a bucket as a gap, or asks a wider
  `every`.

### Percentiles

- `p95` of a gauge is the sample at that rank among the bucket's samples,
  nearest rank, ceil(0.95 × n): a value that was stored, exact, as `value`.
  It ranks samples, not the time each value held, so a gauge written
  unevenly weighs its dense stretches more. It reads every sample of the
  bucket and never a summary, so it counts against the samples a query may
  decode.
- `p95` of a timer is the bucket of its spread that holds that rank: `lower`
  and `upper` are its bounds, both exact, and a bucket of the answer gives no
  `value`, since any one number would be read as the percentile. Past the
  last bound `upper` is `Infinity`. With `estimate: true` a bucket also gives
  `estimate`, a point between them, interpolated as Prometheus'
  `histogram_quantile` does, and named so that nobody takes it for a
  measurement.
- A timer without `buckets` has no percentile: `invalid`, naming it.
- `p50` through `p99.9` and any rank between: `p` and a number above 0, at
  most 100, with 15 decimals at most. The rank is the decimal the query
  wrote: `p99.9` ranks at 999 in 1000, never at a float near 0.999.
- A set of a timer's bounds is what its writer wrote together: in a bucket
  of time a timer's labels hold the bounds that counted as many samples as
  their `+Inf`. A process that measured nothing in the bucket narrows no
  other's bounds.
- A timer's percentile answers a block of its buckets inside one bucket of
  time from its summary, as `increase` does; a gauge's reads every sample.
- In Rust a percentile is `Op::Percentile(95.0)`, and a bucket's answer a
  `Value`: `Value::Exact(v)` as every other `op` gives, or a timer's
  `Value::Between { lower, upper, estimate }`; `.estimate()` on the query
  asks for the estimate, which past the last bound is that bound, and with
  no bound below it none.

### Groups

- Without `by` and `without`, every series of the name is one group, and one
  answer. `by` keeps the labels it names, and `without` all but those; the
  series that share the labels kept are one group. `without: []` keeps every
  series apart. A group never joins two names.
- Each series' bucket is computed exactly, then joined, and the group's
  value rounded once: counts, sums, increases and deltas add; `min` and `max`
  take the least and the greatest; `avg` divides every sample's sum by every
  sample's count, never averaging averages; `p95` ranks every sample of the
  group, or adds a timer's bucket counts before it ranks.
- `first` and `last` add up too: the last of several queues' depths is their
  total length at the bucket's end. A series adds the last sample it has in
  the bucket, and one without a sample there adds nothing: a server that
  stopped writing leaves the total at the next bucket rather than adding its
  last depth for ever, and an `every` finer than the samples makes the total
  dip.

```ts
const backlog = await store.metrics.aggregate({ name: 'queue_depth', since: '1d', every: '1h', op: 'last' })
// every queue's length at the end of each hour, added up
```

### A heatmap

```ts
const heat = await store.metrics.aggregate({ name: 'http_request_ms_bucket', since: '1h', every: '1m', op: 'increase', by: ['le'] })
```

How many durations fell under each bound, each minute: a timer's buckets are
counters, one a bound, each counting the durations at most its `le`, the last
`+Inf`.

## The newest value

```ts
const cpu = await store.metrics.latest({ name: 'cpu', since: '5m' })   // [{ name, labels, at, value }, …]
```

```rust
let cpu = stats.query().name("cpu").since(Duration::from_mins(5)).latest()?;
```

- `latest` gives the newest sample of each series in the range, one a series,
  in `read`'s order. A series without one is left out, so the range bounds
  how stale an answer may be: a host that went down leaves the answer rather
  than showing its last value for ever.
- It reads one small piece of each series, however long the range.

## Cost

```ts
const plan = await store.metrics.explain({ name: 'cpu', since: '30d', every: '1h', op: 'avg' })
// { series: 40, bytes: 1_310_720, decoded: 960, exceeds: undefined }
// blocks cut at whole hours answer from their summaries; only samples not packed yet are decoded

const heavy = await store.metrics.explain({ name: 'cpu', since: '30d', every: '1h', op: 'p95' })
// { series: 40, bytes: 14_680_064, decoded: 6_912_000, exceeds: { limit: 'samples decoded', wanted: 6_912_000, bound: 1_048_576 } }
```

```rust
let month = stats.query().name("cpu").since(Duration::from_hours(30 * 24));
let plan = month.clone().every(Duration::from_hours(1)).explain(Op::Avg)?;
let read = month.explain(None)?;   // of a read
```

- `explain` takes what `read` or `aggregate` takes, and says what the call
  would spend of each bound: the series it finds, the bytes it reads, the
  samples it decodes; and in `exceeds`, the bound it would stop at, as the
  `limit` error would name it. It reads what the file keeps about each
  series and decodes no sample. Past `samples decoded` it goes on counting,
  so `wanted` is every sample the call would decode; at `series` and `bytes
  read` it stops where the call would.
- The store cuts a series' packed samples into blocks where whole hours
  begin, or the longer spans that divide a day for a series that writes less
  often, and each block keeps an exact summary. A bucket that holds whole
  blocks answers from their summaries, so the hours and days a dashboard
  draws over a month decode almost nothing, a timer's percentile among them;
  a bucket finer than a block, and a gauge's percentile always, decode its
  samples.

## What a store holds

```ts
const page = await store.metrics.names({ prefix: 'http_' })
// { names: [{ name: 'http_requests_total', kind: 'counter', seriesCount: 12, unit: '', help: 'Requests served.', keep: '30d', retainedFrom, newest }, …], next }
const more = await store.metrics.names({ prefix: 'http_', after: page.next })
const { labels } = await store.metrics.labelNames('http_requests_total')                                // ['route', 'status']
const { values } = await store.metrics.labelValues('http_requests_total', 'route', { prefix: '/api' })   // ['/api/notes', …]

await store.metrics.describe('cpu', { unit: '%', help: 'Busy time of a core.' })
const { unit, help } = await store.metrics.description('cpu')
```

```rust
let page = stats.names().prefix("http_").page()?;                                             // NamePage { names, next }
let labels = stats.label_names("http_requests_total").page()?.labels;                         // ["route", "status"]
let routes = stats.label_values("http_requests_total", "route").prefix("/api").page()?.values;   // ["/api/notes", …]

stats.describe("cpu", Description { unit: "%".into(), help: "Busy time of a core.".into() })?;
let Description { unit, help } = stats.description("cpu")?;
```

- `names` gives a page of the names with samples, in the byte order of
  their text, each with its kind, how many series it has, its unit and
  help, what it keeps, `retainedFrom`, where that begins, and its newest
  sample's time. `prefix` narrows them, `limit` bounds a page, 1000 unless
  given, and `next`, when there is more, is where the next page starts:
  `after: page.next`, as kv's and blobs' pages are. `retainedFrom` is its
  oldest sample's time, or where its `keep` begins while it holds samples
  from before, which no read finds: what it keeps is gone a block at a time,
  so the oldest sample it holds may lie before it, an hour at most for a
  series that writes every 15 seconds, as records' streams say it.
- `labelNames` gives a page of a name's label names, and `labelValues` of a
  label's values, each as `names` gives its own. The store's own labels,
  `__overflow` and `__bin`, are not among them.
- None of them reads a sample: the file keeps what they give beside the
  series. What a write or a `delete` changes they give at once.
- In Rust each is a builder that `page()` runs, `limit` and `after` beside
  `prefix`; `describe` takes a `Description` whole, as `description` gives
  one.
- `describe` sets a name's unit and help, in place of what it had, as the
  Rust `metrics` crate's `describe_counter!` does; both absent remove them,
  `{}` and `Description::default()` alike.
  They belong to the name, not to a series, so `delete` leaves them, and a
  name with no series yet keeps them for its first. An instrument given
  `unit` or `help` describes its name at its first write after it opens,
  and a `describe` after that stands; a timer describes its sum and its
  longest in `ms`.

## Remove a series

```ts
await store.metrics.delete({ name: 'cpu', labels: { host: 'web-1' } })   // true if it held that series

for (let damaged = await store.metrics.damaged(); damaged.length > 0; damaged = await store.metrics.damaged()) {
  for (const series of damaged) await store.metrics.delete(series)
}
```

```rust
let held = stats.delete("cpu", [("host", "web-1")])?;   // true if it held that series

for damage in stats.damaged()? {
    stats.delete(&damage.name, &damage.labels)?;
}
```

- `delete` removes the one series of exactly these labels, no more and no
  fewer, and all its samples, whether they still read or not: `labels: {}`
  is the series without labels, and the store's own, `__overflow` and
  `__bin`, name their series too. The labels are always given, so that a
  delete never reads as one of every series of a name. Writing the same labels later starts a new series,
  as an instrument that counts into them does at its next write: a delete
  stops no writer. A name keeps its `keep`, unit and help.
- A series whose bytes changed under it reads `corrupt`, naming it, and
  stops taking samples, `suspended`; its neighbours go on, and a read that
  does not reach the bytes that changed still works. `damaged` lists such
  series, `{ name, labels, error }`, 1000 at most, and `delete` removes one
  with every sample it held: read what still reads first, if it matters.
  Deleting those listed shows the next.
- In Rust `delete` takes a series' name and its labels, as pairs or as the
  `Labels` a read or `damaged` gave.

## Rust: the metrics crate

```rust
metrics::set_global_recorder(store.metrics()?.recorder())?;

metrics::counter!("http_requests_total", "route" => "/notes").increment(1);
metrics::gauge!("queue_depth").set(12.0);
metrics::histogram!("http_request_ms").record(started.elapsed().as_secs_f64() * 1000.0);
```

- The recorder makes the `metrics` crate's macros write to the store, as the
  logger's layer does `tracing`'s: a counter is a counter, a gauge a gauge,
  and a histogram a histogram of its own unit, which `describe_histogram!`
  gives, with the buckets the recorder was given for its name. A process has
  one global recorder, which the crate lets be set once: a program that
  opens two stores, as a test may, records into the first.
- The recorder's buckets are given by name, as a timer's are:
  `store.metrics()?.recorder().buckets("http_request_ms", &[5.0, 25.0, 100.0])`,
  or `Buckets::Exponential`.
- A program that uses the crate calls the store's handle something else
  than `metrics`, as this book's examples do, so that the two read apart.
- Rust spells the rest as a builder each: `stats.gauge_func("open_tickets",
  || pool.len() as f64).open()?`, whose function gives a number or a
  `Result` of one and runs on the store's thread;
  `exponential_buckets(1.0, 2.0, 14)`; `stats.flush()?`.
- A timing from `start()` records when it drops, so it is kept in a name,
  `let _timing = latency.start()`. `#[must_use]` makes the compiler warn of
  `latency.start();` alone; `let _ = latency.start()`, which Rust reads as a
  value thrown away on purpose, records at once without a word, and clippy's
  `let_underscore_must_use` is what catches it.

## Tests

```ts
const store = await open(dir, { private: true, background: false, clock: new Date('2026-10-11T09:00:00Z') })
requests.inc()
await store.metrics.flush()                         // written now
await store.clock.advance('15s')
```

A store opened with `background: false` writes instruments only at
`flush()` and when it closes.

## Prometheus in

```ts
const text = await (await fetch('http://db-1:9100/metrics')).text()
const { samples, skipped } = await store.metrics.ingest(text, { format: 'prometheus', labels: { instance: 'db-1' }, keep: '7d' })
```

- The text Prometheus' exporters serve, and OpenMetrics': a counter is a
  counter, a gauge and an untyped value a gauge, a histogram its `_bucket`,
  `_sum` and `_count`, and `# HELP` and `# UNIT` describe their names. A
  counter's created time, `_created` in OpenMetrics 1.0, is the zero its run
  began with.
  `labels` are added to every series, and one the text gives as well is
  `invalid`; a sample without a time is taken at `at`, now unless given; and
  `keep` is every name's in it.
- A summary's quantiles are another program's estimates and a native
  histogram needs a format of its own: both are left out and counted in
  `skipped`, rather than refusing the scrape, since every Go program's
  `go_gc_duration_seconds` is a summary. A summary's `_sum` and `_count` are
  kept as counters.
- After the engine: the parser, then Prometheus' remote write in
  `tinystore serve`.

## Later: rollups

```ts
const up = store.metrics.gauge('probe_up', { keep: '30d', rollup: { every: '1h', keep: '400d' } })
const bars = await store.metrics.aggregate({ name: 'probe_up', since: '90d', every: '1d', op: 'avg' })
```

- A name may keep exact summaries of its samples, a bucket of `every` each,
  for longer than its samples: count, sum, least, greatest, first, last and
  increase, each exact. An aggregate past what the samples keep answers from
  them, a bucket so answered says `rollup: '1h'`, and an `every` that is not
  a whole number of the rollup's is `invalid` there, as is a percentile,
  which no summary holds.
- After `v0.1.0`; nothing above changes for it.

## Later: copy them elsewhere

```ts
const batch = await store.metrics.poll(cursor)   // { series, cursor, expired }
```

- Every sample once, in the order the store took them, from a cursor the
  program keeps, as records' `poll` gives events, and `expired` counts the
  samples `keep` removed before the cursor reached them: for a small store
  beside a database that forwards to a hub, which takes the backlog since
  packing follows each series. It needs the store to keep what it took for a
  while, so it is not built until a caller needs more than reading by time,
  which forwards whatever its writers send in order.

## Errors

| Error                | When                                                                                                                                                                                                                                                                                                                                                                | What to do                                                   |
|----------------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------|
| `invalid`            | a bad name, label or option; a name given two kinds, two `keep`s or two sets of buckets; a negative `add` to a counter, a NaN or an infinity to any instrument; an `op` the kind has not; a percentile of a timer without buckets; a gauge bucket with a NaN or an infinity; a counter below zero; a histogram's series without its `le`, or buckets without `+Inf` | fix the call, or ask another `op`                            |
| `too old`, `too new` | a sample older than its name keeps or behind what its series has packed, or more than ten minutes ahead                                                                                                                                                                                                                                                             | send the time it was taken, within the window                |
| `limit`              | a series past 100,000; an ingest past its bound; a query past a bound, which it names                                                                                                                                                                                                                                                                               | ask for less: a shorter range, a wider `every`, fewer series |
| `corrupt`            | a read over a series whose bytes changed                                                                                                                                                                                                                                                                                                                            | `damaged`, then `delete`                                     |
| `suspended`          | an ingest into a damaged series                                                                                                                                                                                                                                                                                                                                     | `delete` it                                                  |
| `closed`             | the store closed                                                                                                                                                                                                                                                                                                                                                    | open it again                                                |

Every error names the series as Prometheus prints it, and the sample:
`metrics cpu{host="web-1"}: sample at 2026-10-11T09:00:00Z: too old`. An
instrument's calls throw only for what is wrong in the call: what the store
refuses at a write is said once, and the other instruments are written.

## Bounds

| What                                 | Bound                                                                           |
|--------------------------------------|---------------------------------------------------------------------------------|
| a series                             | 128 labels with its name, 16 KiB of them; a label's name 256 B, its value 4 KiB |
| sets of labels an instrument holds   | 2000, the set without labels none of them; past them, `__overflow`              |
| a timer's or a histogram's buckets   | 128 bounds, `+Inf` added; exponential, 128 intervals a set before they halve    |
| a percentile's rank                  | above 0, at most 100, 15 decimals                                               |
| series in a store                    | 100,000                                                                         |
| one `ingest`                         | 10,000 samples or 4 MiB                                                         |
| a sample's time                      | from `keep` ago to ten minutes ahead                                            |
| `keep`                               | 30 days unless the name says                                                    |
| a query's series                     | 1000                                                                            |
| a query's bytes read                 | 16 MiB                                                                          |
| a query's samples decoded            | 1,048,576                                                                       |
| an answer                            | 100,000 samples or buckets                                                      |
| `names`, `labelNames`, `labelValues` | 1000 a page, 10,000 at most                                                     |
| `damaged`                            | 1000 an answer                                                                  |
| a unit, a help                       | 32 bytes, 1024 bytes                                                            |
| instruments                          | written every 15 seconds and when the store closes                              |

A `limit` error names its bound as the table does: `series`, `bytes read`,
`samples decoded`, `answer`.

## What the engine chooses

Nothing here is an option: how samples wait before they are packed, the
codec that packs them, blocks and their summaries, which summary answers a
bucket, how far out of order a sample may arrive before its stretch is
packed, how many readers the file has and how long a read may hold its
snapshot. A program meets them only as the bounds above. `durability` is the
store's, `'os'` for this file unless it says `'full'`.

Names that start with `tinystore_` are the store's own: the store writes
there what it measures of itself, its writers' waits, its queues' depths,
its expirations (Dashbin's A10, phase 4). A label that starts with `__` is
the store's too.

## Gates the engine adds

Each becomes a line of GATES.md when its test lands, beside the Go engine's
lines it ports.

- a run's zero and a first count of the same millisecond both survive, and
  packing keeps the zero —
  `a_runs_zero_survives_a_first_count_of_the_same_millisecond`
- `2`, `0`, `5` increase by 5 in the head, in one block or across three, and
  under every range that cuts them — `a_restart_counts_the_same_across_blocks`
- two connections counting into one series add up, and a gauge of one that
  left is no longer written — `instruments_of_two_connections_add_into_one_total`
- an overflowed set is a group of its own, and the groups add up to the
  total — `the_overflow_is_a_group_of_its_own`
- a step across a whole empty bucket says `gap`, and none else does —
  `only_a_step_across_an_empty_bucket_is_a_gap`
- a percentile keeps each bucket of time's own bounds, and only the shared
  ones where two sets meet — `a_percentile_uses_the_bounds_two_sets_share_where_they_meet`
- `last` over a group adds only the series with a sample in the bucket —
  `last_adds_only_series_with_a_sample_in_the_bucket`
- a gauge function that hangs or fails holds up no other instrument and
  writes nothing — `a_hung_gauge_function_holds_up_nothing`
- a timer's percentile past its last bound is `upper: Infinity` —
  `a_rank_past_the_last_bound_has_no_upper_bound`

## Dashbin's asks

| Ask                                  | Where it is                                                                                                                    |
|--------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|
| B1: buckets that add up to the range | each step counts in the bucket it ends in, the first from the newest sample before the range; a bucket after a gap says so     |
| B2: histograms, exact                | `p95` of a gauge by rank; a timer's `buckets` with `+Inf`, its percentile `lower` to `upper`, `estimate` on request; a heatmap |
| B3: first, last and the latest value | `op: 'first'`, `op: 'last'`, `latest`                                                                                          |
| B4: retention per name               | `keep`, a name's own, kept in the file                                                                                         |
| B6: what a store holds               | `names`, `labelNames`, `labelValues`, none reading a sample                                                                    |
| B7: columns                          | `read` gives `times` and `values`                                                                                              |
| B12: units and help                  | `unit`, `help`, `describe`; `names` gives them                                                                                 |
| B14: limits by name                  | a `limit` error names its bound; `explain` says it first                                                                       |
| A10: the store's own metrics         | the names `tinystore_…`, phase 4                                                                                               |
| C1, C2: Studio, a read token         | every call that reads is a read method of the protocol, which a `read` token may call; a query is data a panel keeps           |
| C4: tiers that show their resolution | later: `rollup`, whose buckets say so                                                                                          |
| C5: Prometheus in                    | `ingest(text, { format: 'prometheus' })`; remote write in the server                                                           |
| C6: forwarding from an edge store    | later: `poll`                                                                                                                  |

## Newcomer check and comparison, 11 October

Two Haiku agents read the first draft with no other context: one said what
each of 36 call sites does and returns, one compared thirteen spellings
blind. None of the call sites was read wrongly; these made a reader stop.

| What they found                                                                                    | What the book does                                                                                 |
|----------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------------------------|
| a timer's p95 as `{ value: 50, lower: 25 }` read as a point, and `estimate: true` then as a riddle | `{ lower, upper }` and no `value`: any one number would be read as the percentile                  |
| `by: []` beside no `by`, which a reader took for the same; records' `aggregate` joins without `by` | without `by` every series is one group, as in records; `without: []` keeps them apart              |
| `op: 'last'` over a group: one series' last, or their sum?                                         | said, with the example of several queues' total length                                             |
| `explain`'s `samples: 960` for 30 days of 15-second samples, and `stops`                           | `decoded`, the comment says whole hours answer from their summaries; `exceeds`                     |
| `oneOf()`, `noneOf()`, `prefix()` beside `prefix` as `names`' option; a query a panel cannot keep  | conditions as data: a list, `{ not }`, `{ prefix }`, the same in JSON, in Python and over the wire |
| `delete({ name, labels })` read as every series with those labels                                  | the one series of exactly these labels, and the answer says whether it held it                     |
| `record(0.25)` meant as seconds                                                                    | a number is milliseconds, as `performance.now()` counts; Python, Go and Rust take typed durations  |
| `damaged` read as a damaged block, `delete` then losing good samples                               | a series; deleting it takes every sample it held, said                                             |
| `'5y'`, `body.length` of a string, `signups_daily` as a counter, a timer's `record` without labels | the examples: `'730d'`, `byteLength`, `signups_total`, `record` with its labels                    |

Read right at once: `store.metrics.counter`, `with`, `measure`, `ingest`,
`read`, `latest`, `names`, `labelNames`, `labelValues`, `flush`, the step
into the first bucket, `keep` given twice as an error. The comparison chose
the book's spelling in eight of thirteen: `store.metrics` over `store.counter`
beside kv's `store.counters`, `gaugeFunc`, `op`, `delete`, `ingest`, buckets
only when asked, `labelNames` and `labelValues`, columns for a read.

Kept as they are: `op: 'p95'`, where the comparison chose `op: 'quantile',
quantile: 0.95`, since the newcomer read `p95` at once and every dashboard
writes it so; `every`, where it chose `width`, since records' `aggregate`
says `every` and a reader of both learns one word; `describe` as a setter,
which one reader took for a getter, as the Rust `metrics` crate's
`describe_counter!` sets; `expired`, which one reader would have called
`skipped`, as records' `poll` says it.

## Review, 11 October

A reviewer the owner works with read the draft for Dashbin and asked for
these before it is accepted:

| They asked                                                          | What the book does                                                                                   |
|---------------------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| a step over ten days counted in the last hour, and called a rate    | the step rule stays, Dashbin's B1; the bucket after a gap says `gap: true`, and its rate is no speed |
| a duration past the last bound must not be lost                     | `+Inf` is added, and a percentile past the last bound has `upper: Infinity`                          |
| a histogram's buckets that change between processes cannot be added | a name's buckets are its own; across a change a percentile uses the bounds both share                |
| a gauge's function that hangs, fails or runs into its next call     | it holds up nothing else, never runs twice at once, and an error writes nothing, said once           |
| a gauge's p95 ranks samples, not time                               | said                                                                                                 |
| `with({ userId })` making a million series                          | 2000 sets of labels an instrument, then `__overflow`; 100,000 series a store                         |
| what `_max` is the longest of                                       | of the durations since the write before                                                              |

Its second reading accepted the calls as they stand and asked for four
rules, which the book now gives:

| They asked                                                                  | What the book does                                                                                                                                |
|-----------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------------------|
| bounds changed between runs that share none: a p95 from zero to infinity    | each bucket of time keeps its own bounds; only where two sets meet do the shared ones serve, and the interval widens, to `[0, Infinity]` at worst |
| `__overflow` keeps the total but takes requests away from their route       | it is a group of its own in every grouping, so the groups add up; an idle set frees its place after an hour, a counter's as a reset               |
| what counts as a gap: a quarter of an hour between samples, or a lost hour? | a whole bucket of the query's `every` without samples; the engine guesses no series' cadence                                                      |
| `last` over gauges adding a stopped server's depth for ever                 | a series adds only its last sample inside the bucket                                                                                              |

Its third reading accepted the book but for one sum: a counter that starts
again and passes its old count before its first write, `2` then `5`, shows
no reset, and its increase comes out 3 where it is 5. Each run of a counter
now begins with a zero written at its start, as the start time of an
OpenTelemetry point and OpenMetrics' created time mean, though neither
writes it as a sample, so `2`, `0`, `5` count 5; a timer's parts start
together.

Its fourth reading accepted the book and asked for tests rather than another
round: a zero and a first count in one millisecond, and `2`, `0`, `5` cut
across blocks, are among the gates below. It asked to keep one writer to a
series. That holds for an ingested series and for a gauge; a counter's and a
timer's totals are the store's, which every process adds to, since several
workers through one sidecar are how a TinyStore program commonly runs, and
their totals written over each other would be wrong without a word.

## While building, 12 October

| The owner asked                                                           | What the book does                                                                            |
|---------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| a rate over the millisecond a range leaves of a bucket, read as an hour's | the rate still divides by what is left; the bucket says `partial`, as one retention cut does  |
| an hour with ten minutes of data, shown as a whole hour's                 | a bucket the clock has not finished is `partial` too, which was `open: true` in the open list |

Building the instruments raised these, which the book now settles:

| What came up                                                       | What the book does                                                            |
|--------------------------------------------------------------------|-------------------------------------------------------------------------------|
| Rust's call sites, `with(…).inc()`, and the `metrics` crate's      | a Rust call returns nothing; what it refuses changes nothing and is said once |
| "the shortest text" of a bound, which languages write differently  | as JavaScript's `String(n)` writes it: `1e+21`, where Rust writes 22 digits   |
| the set without labels, opened with the instrument, as one of 2000 | it takes none of the places                                                   |
| a timer of a thousand bounds, a series each a set of labels        | 128 bounds at most                                                            |

Building the percentiles raised these:

| What came up                                                                     | What the book does                                                                         |
|----------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------|
| a rank of 7%, which floats make 7.000000000000001 of 100 samples                 | the decimal the query wrote, 15 decimals at most                                           |
| which bounds are one set, in a bucket of a change or of two processes            | those written together: as many samples as `+Inf`; a process idle there narrows no other's |
| `le="5.0"` from one exporter and `le="5"` from another                           | an ingested `le` is written as JavaScript writes its number                                |
| a timer's percentile over a month of hours                                       | answered from summaries, as `increase` is                                                  |
| a timer's bucket in which it measured nothing                                    | 0 for `count`, `sum` and `rate`; left out of `avg`, `max` and a percentile                 |
| Prometheus' bounds for 2^(1/2) and two more, a unit below the float nearest them | taken as Prometheus writes them, so that its native histograms merge exactly               |

## Review of the percentiles, 12 October

Three Haiku agents read the new calls with no other context: a newcomer
check of 18 call sites, a blind comparison of six spellings, and a free
review as a developer drawing latency dashboards.

| They found                                                                   | What the book does                                                              |
|------------------------------------------------------------------------------|---------------------------------------------------------------------------------|
| `value: None` for a timer's percentile, read as no data: a loop drew nothing | `Value::Exact` beside `Value::Between`, which the blind comparison chose        |
| `.buckets(&[5.0, 10.0])` warned by clippy, since an array by value fit too   | an array by reference, a slice or a `Vec`                                       |
| `exponential_buckets()` beside `Buckets::Exponential`                        | both kept, the first as Prometheus' `ExponentialBuckets`; each names the other  |
| a timer's quiet bucket: zero, or missing?                                    | said: 0 for `count`, `sum` and `rate`, left out of `avg`, `max` and percentiles |

The comparison chose what the book had: `'exponential'` over `'auto'`,
`'native'` and `{ exponential: 8 }`, whose 8 it read as a factor;
`Op::Percentile(95.0)` over `Op::P` and `Op::Quantile(0.95)`, whose scale is
another; `estimate` over `interpolated`; and `.estimate()` on the query, one
word for the call and the field. The free review's asks for exponential
buckets by default and for `upper: Infinity`, which `JSON.stringify` writes
as `null`, are in the open list.

## Review of what a store holds, 12 October

Three Haiku agents read `names`, `labelNames`, `labelValues`, `describe`,
`delete` and `damaged` with no other context: a newcomer check of 16 call
sites, a blind comparison of six spellings, and a free review as a
developer building a metrics browser and a page that cleans up series.

| They found                                                                      | What the book does                                                                  |
|---------------------------------------------------------------------------------|-------------------------------------------------------------------------------------|
| a list whose `after` is its last name, which says no more whether there is more | a page and its `next`, as kv's, blobs' and records' are, which the comparison chose |
| `delete` of no labels: every series of the name, or the one without labels?     | said: the one without labels; the store's own labels name their series too          |
| a series deleted while an instrument still counts into it comes back            | said: a delete stops no writer                                                      |
| who wins, `describe` or an instrument's `help`                                  | said: the instrument's first write after it opens, and a `describe` after that      |
| `sorted`, by what                                                               | the byte order of their text                                                        |

The comparison chose what the book had in four of six: `NameInfo`,
`describe` taking a `Description` whole, since a builder that leaves `unit`
out would erase it, `delete("cpu", [("host", "web-1")])`, and `damaged`.

Kept as they are, against them: a name's `oldest` where its `keep` begins
while it holds samples from before, where the comparison and the free review
expected the earliest sample a read returns, since that moves with the clock
inside a block and finding it would decode a block of every series of the
name; `describe` replacing both, where the free review asked for each part
alone; `stats` in the Rust examples, where two asked for `metrics`, the
crate's own name; `describe` beside `description`, which the free review
would have made `setDescription`; and `description` of a name nothing
described, empty rather than an error. Its asks for a list of series
without their samples, a delete by a name or a condition with what it took
and a dry run, and values narrowed by time and other labels are in the open
list.

## Review of the Bun SDK over the wire, 12 October

Three Haiku agents read the TypeScript surface of slice 7 and an example
program with no other context: a newcomer check of eight tasks, a blind
comparison with prom-client, OpenTelemetry, StatsD clients and Prometheus'
and InfluxDB's queries, and a free review. What two or more of them asked
for is built; the owner also asked for types that offer and check what a
program writes.

| They found                                                                 | What the SDK does                                                                    |
|----------------------------------------------------------------------------|--------------------------------------------------------------------------------------|
| a label misspelled in `with` makes a new series; no label names to declare | `counter<'route' \| 'status'>(…)`: `with` compiles with those alone                  |
| `delete({ name })` reads as every series of the name                       | `labels` always given, `{}` the series without labels, as Rust's `delete` takes them |
| `damaged()`'s one-line loop stops at 1000                                  | the loop asks again until none is left, in the book and the SDK                      |
| `stop()` beside the store's close, and beside a timing's `stop()`          | gone from `store.metrics`: the store's close stops the instruments                   |
| `rate` with no unit; whether a write without `keep` resets it              | said: a second's increase; what the name keeps already                               |

Built at the owner's ask: a duration's text, an `op` and a percentile are
offered as they are typed and one misspelled does not compile, and a bucket
of `count` … `last` has its `value` as a number. TypeScript folds a literal
into a template beside it that takes it in, `'1h'` into `${number}h`, and
offers neither; an intersection keeps them apart. Found while answering
them: a second `gaugeFunc` of a name kept the first's function and dropped
the second without a word; it throws.

What the owner decided the same day, after an outside reviewer's reading:

| Asked                                                               | By    | Decided                                                                                  |
|---------------------------------------------------------------------|-------|------------------------------------------------------------------------------------------|
| `describe` merging, `null` removing a part                          | three | kept: `describe` replaces both                                                           |
| `flush()` answering what was dropped, overflowed or is still unsent | three | no count, whose three causes are not one; a flush fails with what it was not told, below |
| `buckets: 'exponential'` beside `exponentialBuckets()` as one thing | three | kept                                                                                     |
| a gauge's function with labels, and a way to stop one               | two   | built: a function a set of labels, in Rust and the SDK, a set read by one                |
| a delete by a condition or of a name whole, with what it took       | two   | later, a feature of its own with a dry run, a bound and what it does to instruments      |
| a percentile as one number                                          | two   | kept: a timer's answer is the bucket that holds it, exact; `estimate` asks for a point   |
| `labels` for `with`, OpenTelemetry's `forceFlush` and `shutdown`    | one   | kept: the book's vocabulary                                                              |
| `since` and `from`, `by` and `without` apart in the types           | one   | kept: each pair is refused at the call                                                   |
| a series' columns that `JSON.stringify` writes as objects           | one   | kept: `Float64Array` keeps every value's bits                                            |

A flush's success means the store confirmed what it carried. What it was
not told fails it: what the store refused since the flush before, an
instrument it would not open, is `invalid`; the batches a store that
started again may have counted, which the SDK dropped rather than count
twice, are `outcome_unknown`; each fails one flush, the next. A call that
would take what a send carries past the largest number throws `invalid`
where the program made it, since the store would refuse it with nobody
told. Counters of what background sends failed or left unconfirmed,
`metrics.diagnostics()`, are in the open list.

## Free review, 11 October

A third Haiku agent reviewed the first draft as a developer choosing a store
for a service with dashboards. Half of what it found the rounds above had
already changed: a timer's percentile as `lower` and `upper`, `gap`, a run's
zero, `explain`'s numbers, a group without `by`. Of the rest:

| It found                                                                         | What the book does                                                                                      |
|----------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------|
| an hourly panel over a day decodes every sample, since a block straddles hours   | the store cuts blocks where whole hours begin, so a bucket of whole blocks answers from their summaries |
| `gauge.set(NaN)` after a division by zero fails every average of its bucket      | an instrument refuses a NaN or an infinity where it is given; `ingest` keeps them                       |
| `go_gc_duration_seconds` is a summary, so a Go program's scrape would be refused | a summary's quantiles and a native histogram are left out and counted, a summary's sums kept            |
| `5` from TypeScript and `5.0` from Python as two `le`                            | a bound is written as the shortest text of its number                                                   |
| `where: { status: 200 }` finding nothing                                         | a number in `where` is its text, as in `with`                                                           |
| 3585 holds only for a counter that starts with the range                         | both cases are shown                                                                                    |
| a NaN's payload read out of a `Float64Array`                                     | said: the bytes keep it, a JavaScript number may not                                                    |
| `let _ = latency.start()` in Rust records at once                                | `#[must_use]` for a timing left alone, and the book says `let _` throws it away at once                 |
| a counter with labels nobody counted yet shows nothing                           | `add(0)` makes it known with a zero                                                                     |

Kept as they are, against it: the percentile's interpolation, where the
comparison and the owner's reviewer chose bounds; `every`, records' word,
where it chose `step`; `ingest` whole or not at all, as records' `append`
is, where it asked for a partial success; rollups after `v0.1.0`, since
blocks cut at whole hours answer a month of hours or days from summaries;
`timer` beside `histogram`, the word a newcomer looks for when measuring
time. Its asks for a second step of a group, a bucket still filling, a
heatmap with its own bounds, retries that count as written, declared label
names, NaN by IEEE's rules and a read past a damaged series are in the open
list below.

## Open

- `metrics.diagnostics()`: how many of the background sends failed, and how
  many batches a restart left unconfirmed, for a program that watches its
  own instruments beside its flushes.
- Whether a timer has buckets unless told otherwise: a percentile then works
  at once, at a dozen series more a set of labels. The comparison chose
  buckets only when asked.
- `gap` as a flag, or the time the bucket's first step began, which a
  reader asked for and which says how long the gap was.
- A flag on a percentile whose interval widened where two sets of bounds
  met, beside the interval that already says so.
- A gauge's set of labels the program removes, as prom-client's `remove`,
  for gauges of connections that come and go.
- A gauge that several processes add to, in-flight requests across workers,
  kept by the store as their sum, with what a process added taken back when
  it leaves, rather than an `instance` each.
- `lookback`, which the Go engine had and dx.md lists among the worst names,
  is gone: the first bucket's step comes from the newest sample before the
  range however far, so that adjacent ranges add up, and a bucket after a
  gap says so. The Go engine looked one bucket back unless told.
- The bounds as options: a hub that keeps more than 100,000 series, a panel
  that reads more than 1000, an instrument of more than 2000 sets of labels.
- An interval of the program's own for instruments, `flushEvery`, as kv's
  counters take it.
- `keep` by prefix, for names a program never writes itself; a scrape gives
  its `keep` for every name it brings.
- Buckets by a time zone's days; UTC now.
- An aggregate's buckets as columns, as `read`'s samples are.
- A read past 100,000 samples as an iterator, a series at a time.
- Prometheus' native histograms, whose grid the exponential buckets already
  are: a scrape's and a remote write's, and a set's buckets kept in one
  sample.
- Exponential buckets for a timer that names none, which a free review
  asked for, once their cost in series is measured beside given bounds'.
- A heatmap of an exponential timer's intervals, which `op: 'bins'` below
  would give.
- `upper: Infinity` over the wire and in JSON, where `JSON.stringify` writes
  `null`.
- A second step for a group, `across`: the sum of each host's average, the
  greatest of each series' rate, where a group now joins by the rule of its
  `op`.
- What a `partial` bucket does cover, `covers`, for a chart that says "ten
  minutes of this hour" where it now only shows the bucket apart.
- `op: 'bins'`, a heatmap with each bound's own count and numeric bounds,
  where the book gives the cumulative counters by `le`.
- An `ingest` sent again after a lost answer: a sample of the same time and
  bits as one the store holds counted as written, rather than `too old`.
- Label names an instrument declares, `labels: ['route', 'status']`, so that
  a misspelt key is `invalid` where it is written.
- A gauge's bucket holding a NaN or an infinity answered by IEEE's rules
  and marked, rather than failing the whole answer.
- A read that leaves a damaged series out and says so, rather than failing.
- A timer's series of one set of labels deleted by its name, its count, sum,
  longest and buckets at once, where `delete` takes one series.
- `labelValues` of the series that meet a query's conditions, as
  Prometheus' `match[]` narrows them, where it now takes a name alone.
- A page of a name's series by their labels, without their samples, which a
  free review asked for a cleanup page; `latest` gives each series with its
  newest sample now.
- `delete` by a name or a condition, saying how many series and samples it
  took, with a dry run, where it now takes one series.
- A step across a gap given as an amount of its own beside the bucket's
  value, which a free review asked for, where the bucket now counts it and
  says `gap`.

## Was, in Go

| Was                                                                                        | Now                                                         |
|--------------------------------------------------------------------------------------------|-------------------------------------------------------------|
| `metrics.Open(ctx, store, Options{…})`, eighteen options                                   | `store.metrics`; the engine opens with its first call       |
| `Options.Retention`, `RetentionOf` by prefix                                               | `keep`, a name's own                                        |
| `Options.Flush`                                                                            | 15 seconds, and `flush()`                                   |
| `Lateness`, `ClockSkew`, `MaxBlockSpan`, `MaxHead*`, `MaxBatch*`, `MaxReaders`, and others | the engine's                                                |
| an instrument's total kept by its process, one writer to a series                          | the store's total, which every process's instrument adds to |
| `Counter`, `Gauge`, `Timer`, `GaugeFunc`                                                   | the same, `histogram`, and `buckets` on a timer             |
| `Ingest(ctx, []Batch{{Series, Samples}})`                                                  | `ingest({ name, kind, labels, samples })`                   |
| `Range{Name, Match, Where, Since, From, To}`                                               | `{ name, where, since, from, to }`; `where` is data         |
| `OneOf`, `NoneOf`, `Prefix`                                                                | a list, `{ not }`, `{ prefix }`                             |
| `Read`, `Stream`                                                                           | `read`                                                      |
| `Latest`, a `Result` of one sample                                                         | `latest`, `{ name, labels, at, value }`                     |
| `AggregateRequest{Range, Width, Op, By, Without, Lookback}`                                | `aggregate({ …, every, op, by, without })`                  |
| `By: []string{}` joining every series; no `By` keeping them apart                          | no `by` joins them; `without: []` keeps them apart          |
| `AggregateCount` … `AggregateLast`                                                         | `op: 'count'` … `'last'`, and `'p95'`                       |
| `AggregateBucket{From, To, Count, Resets, Value, Overflow, Partial, Lookback}`             | `{ at, value, samples, resets, partial, gap }`              |
| a timer's mean as the increase of its sum over its count's                                 | `op: 'avg'` of the timer                                    |
| no histograms                                                                              | `buckets`, `p95` as `lower` and `upper`, `histogram`        |
| `ExplainRead`, `ExplainAggregate`, `Plan{Series, Blocks, Summarized, …, Stops}`            | `explain`, `{ series, bytes, decoded, exceeds }`            |
| `Describe`, `Description`, `Unit`, `Help`                                                  | `describe`, `description`, `{ unit, help }`                 |
| `DropSeries`                                                                               | `delete`                                                    |
| `ListMaintenanceFailures`, `RetryFailedMaintenance`, `ErrSuspended`                        | `damaged()`, then `delete`                                  |
| `Maintain`, `Stats`, `Report`, `WriteSelf`, `Snapshot`                                     | the store's                                                 |
| times as unix milliseconds                                                                 | dates and durations; `read`'s columns as unix milliseconds  |
