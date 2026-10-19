# Metrics API for Rust

Every public type, trait and function of the Metrics engine in `tinystore::metrics`, generated from its source. The [Metrics guide](../../chat/how-fast-it-is.md) explains how to use them, and the [Bun and Node](../bun/metrics.md), [Python](../python/metrics.md) and [Go](../go/metrics.md) pages list the same API.

## Store

```rust
impl Store
```

### Store.metrics

```rust
pub fn metrics(&self) -> Result<Metrics>
```

The store's metrics: series written by `ingest`, read back by `query`.
The engine opens with the first call.

## Aggregate

```rust
pub struct Aggregate {
    pub name: String,
    pub labels: Labels,
    pub buckets: Vec<Bucket>,
}
```

A group of an aggregate: the series of one name that share the labels the
query kept, and their buckets in the order of their times.

## Bucket

```rust
pub struct Bucket {
    pub at: SystemTime,
    pub value: Value,
    pub samples: u64,
    pub resets: u64,
    pub gap: bool,
    pub partial: bool,
}
```

One bucket of an aggregate.

## Op

```rust
pub enum Op {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Increase,
    Rate,
    Delta,
    First,
    Last,
    Percentile(f64),
}
```

What a bucket's value is of its samples.

## Value

```rust
pub enum Value {
    Exact(f64),
    Between { lower: f64, upper: f64, estimate: Option<f64> },
}
```

A bucket's answer: one exact number, or where a timer's percentile lies.

### Value.exact

```rust
pub fn exact(self) -> Option<f64>
```

The number, when the value is one.

## Buckets

```rust
pub enum Buckets {
    Bounds(Vec<f64>),
    Exponential,
}
```

A timer's or a histogram's buckets.

```rust
use tinystore::metrics::{Buckets, exponential_buckets};
let api = stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0, 50.0, 100.0]).open()?;
let jobs = stats.timer("job_ms").buckets(exponential_buckets(1.0, 2.0, 14)).open()?;
let db = stats.timer("db_query_ms").buckets(Buckets::Exponential).open()?;
```

## exponential_buckets

```rust
pub fn exponential_buckets(start: f64, factor: f64, count: usize) -> Vec<f64>
```

`count` bounds from `start`, each `factor` times the one before:
`exponential_buckets(1.0, 2.0, 14)` is 1, 2, 4 … 8192, bounds a program
gives, as Prometheus' `ExponentialBuckets` makes them;
`Buckets::Exponential` lets the store keep a finer grid instead, of
the intervals values fell in.

## Damage

```rust
pub struct Damage {
    pub name: String,
    pub labels: Labels,
    pub error: String,
}
```

A series whose bytes no longer read, as `damaged` lists it and `delete`
takes it.

## Description

```rust
pub struct Description {
    pub unit: String,
    pub help: String,
}
```

A name's unit and help, empty when nothing gave them.

## Exceeds

```rust
pub struct Exceeds {
    pub limit: &'static str,
    pub wanted: u64,
    pub bound: u64,
}
```

A bound a call would stop at, as its `limit` error would name it.

## Plan

```rust
pub struct Plan {
    pub series: u64,
    pub bytes: u64,
    pub decoded: u64,
    pub exceeds: Option<Exceeds>,
}
```

What a call would spend of a query's bounds.

## Builder

```rust
pub struct Builder<T> { /* private fields */ }
```

An instrument being opened: its name and what it is told, which `open`
checks against what the store and its file hold.

### Builder.help

```rust
pub fn help(self, help: impl Into<String>) -> Self
```

A line on what it means, 1024 bytes at most.

### Builder.keep

```rust
pub fn keep(self, keep: Duration) -> Self
```

How long its name's samples stay, as `ingest` takes it.

### Builder.unit

```rust
pub fn unit(self, unit: impl Into<String>) -> Self
```

The unit of its values, 32 bytes at most: `bytes`, `%`.

### Builder.unit

```rust
pub fn unit(self, unit: impl Into<String>) -> Self
```

The unit of its values, 32 bytes at most: `bytes`, `%`.

### Builder.unit

```rust
pub fn unit(self, unit: impl Into<String>) -> Self
```

The unit of its values, 32 bytes at most: `bytes`, `%`.

### Builder.unit

```rust
pub fn unit(self, unit: impl Into<String>) -> Self
```

The unit of its values, 32 bytes at most: `bytes`, `%`.

### Builder.buckets

```rust
pub fn buckets(self, buckets: impl Into<Buckets>) -> Self
```

Its buckets: the upper bounds of its spread, ascending, each a
number of 0 or more, to which the store adds `+Inf`, or
`Buckets::Exponential`, a grid the store keeps.

### Builder.buckets

```rust
pub fn buckets(self, buckets: impl Into<Buckets>) -> Self
```

Its buckets: the upper bounds of its spread, ascending, each a
number of 0 or more, to which the store adds `+Inf`, or
`Buckets::Exponential`, a grid the store keeps.

### Builder.open

```rust
pub fn open(self) -> Result<Counter>
```

### Builder.open

```rust
pub fn open(self) -> Result<Gauge>
```

### Builder.labels

```rust
pub fn labels(self, labels: &[(&str, &str)]) -> Self
```

The set of labels its function reads; another set of the name takes
a function of its own, and a set is read by one.

### Builder.open

```rust
pub fn open(self) -> Result<()>
```

Opens the gauge; its function is read from the next write on, while
the store is open.

### Builder.open

```rust
pub fn open(self) -> Result<Timer>
```

### Builder.open

```rust
pub fn open(self) -> Result<Histogram>
```

## Counter

```rust
pub struct Counter { /* private fields */ }
```

A total that only goes up, which the store keeps.

### Counter.with

```rust
pub fn with(&self, labels: &[(&str, &str)]) -> Counter
```

The same counter with more labels, a series of its own; a label
given again takes its new value, and the same labels in any order
are one series.

### Counter.inc

```rust
pub fn inc(&self)
```

### Counter.add

```rust
pub fn add(&self, n: f64)
```

Adds `n`, a finite number of 0 or more; `add(0.0)` makes the set of
labels known with a total of zero.

## Gauge

```rust
pub struct Gauge { /* private fields */ }
```

A value that goes up and down, written while the process that sets it
is connected.

### Gauge.with

```rust
pub fn with(&self, labels: &[(&str, &str)]) -> Gauge
```

The same gauge with more labels, a series of its own.

### Gauge.set

```rust
pub fn set(&self, value: f64)
```

Sets its value, a finite number.

### Gauge.add

```rust
pub fn add(&self, delta: f64)
```

Moves its value by `delta`, from zero when it was never set.

### Gauge.inc

```rust
pub fn inc(&self)
```

### Gauge.dec

```rust
pub fn dec(&self)
```

## GaugeFunc

```rust
pub struct GaugeFunc { /* private fields */ }
```

What `gauge_func` opens: a gauge its function reads at each write.

## Histogram

```rust
pub struct Histogram { /* private fields */ }
```

A timer of numbers that are not durations, each 0 or more.

### Histogram.with

```rust
pub fn with(&self, labels: &[(&str, &str)]) -> Histogram
```

The same histogram with more labels, a series of its own; `le` is
its buckets'.

### Histogram.record

```rust
pub fn record(&self, value: f64)
```

Records a value, a finite number of 0 or more.

## Reading

```rust
pub trait Reading
```

What a gauge's function gives: its value, or its value or why it has
none.

### Reading.value

```rust
fn value(self) -> Result<f64, String>
```

## Timer

```rust
pub struct Timer { /* private fields */ }
```

How many things took how long: their count, their sum and the longest
since the write before, in milliseconds, and their spread over its
buckets.

### Timer.with

```rust
pub fn with(&self, labels: &[(&str, &str)]) -> Timer
```

The same timer with more labels, a series of its own; `le` is its
buckets'.

### Timer.record

```rust
pub fn record(&self, duration: Duration)
```

Records a duration measured elsewhere.

### Timer.start

```rust
pub fn start(&self) -> Timing
```

A timing that records how long it lived when it drops, for a span
that is no one function: kept in a name, `let _timing =
latency.start()`, since `let _ =` drops it at once.

### Timer.measure

```rust
pub fn measure<T>(&self, work: impl FnOnce() -> T) -> T
```

Runs `work` and records how long it took, a panic's unwinding too.

## Timing

```rust
pub struct Timing { /* private fields */ }
```

A duration being measured, recorded when it drops.

## Latest

```rust
pub struct Latest {
    pub name: String,
    pub labels: Labels,
    pub at: SystemTime,
    pub value: f64,
}
```

A series' newest sample.

## LabelPage

```rust
pub struct LabelPage {
    pub labels: Vec<String>,
    pub next: Option<String>,
}
```

A page of a name's label names.

## List

```rust
pub struct List<P> { /* private fields */ }
```

A page of what a store holds, its options before its `page`: 1000 unless
`limit` says.

### List.prefix

```rust
pub fn prefix(self, prefix: impl Into<String>) -> Self
```

Only those that start with this text.

### List.after

```rust
pub fn after(self, next: impl Into<String>) -> Self
```

The page that starts past this one, as a page's `next` says it.

### List.limit

```rust
pub fn limit(self, most: usize) -> Self
```

How many a page holds at most, 1 to 10,000.

### List.page

```rust
pub fn page(self) -> Result<NamePage>
```

### List.page

```rust
pub fn page(self) -> Result<LabelPage>
```

### List.page

```rust
pub fn page(self) -> Result<ValuePage>
```

## NameInfo

```rust
pub struct NameInfo {
    pub name: String,
    pub kind: Kind,
    pub series_count: u64,
    pub unit: String,
    pub help: String,
    pub keep: Duration,
    pub retained_from: SystemTime,
    pub newest: SystemTime,
}
```

A name the store holds series of, as `names` gives it.

## NamePage

```rust
pub struct NamePage {
    pub names: Vec<NameInfo>,
    pub next: Option<String>,
}
```

A page of the names a store holds.

## ValuePage

```rust
pub struct ValuePage {
    pub values: Vec<String>,
    pub next: Option<String>,
}
```

A page of the values a label of a name holds.

## Maintenance

```rust
pub struct Maintenance {
    pub expired: u64,
    pub reclaimed: u64,
    pub packed: u64,
    pub blocks: u64,
    pub suspended: u64,
}
```

What maintenance did.

## maintain

```rust
pub fn maintain(store: &Store) -> Result<Maintenance>
```

Runs the store's metrics maintenance until nothing is left to do, as a
store opened without background work has to.

## SelfMetrics

```rust
pub struct SelfMetrics {
    pub system: bool,
}
```

What a store measures of itself, when it is opened with its own metrics.

## Condition

```rust
pub enum Condition {
    OneOf(Vec<String>),
    NoneOf(Vec<String>),
    Prefix(String),
}
```

What a label's value must be for a query to take a series.

## Not

```rust
pub struct Not<T>(pub T)
```

A label equal to none of these values, or absent: `Not("dev")`,
`Not(["dev", "test"])`.

## Prefix

```rust
pub struct Prefix<T>(pub T)
```

A label that starts with this text: `Prefix("api-")`.

## Query

```rust
pub struct Query { /* private fields */ }
```

A query being asked: its name, conditions and range, and for an
aggregate its buckets and groups.

### Query.name

```rust
pub fn name(self, name: impl Into<String>) -> Self
```

The series of this name.

### Query.filter

```rust
pub fn filter(self, label: impl Into<String>, condition: impl Into<Condition>) -> Self
```

A label's value under a condition: a value, a list of them, a
`Not` or a `Prefix`.

### Query.since

```rust
pub fn since(self, span: Duration) -> Self
```

The span before now the range covers; or `from` and `to`.

### Query.from

```rust
pub fn from(self, at: SystemTime) -> Self
```

The range's start, which is in it.

### Query.to

```rust
pub fn to(self, at: SystemTime) -> Self
```

The range's end, which is not in it; now unless given, a sample taken
now in the range.

### Query.every

```rust
pub fn every(self, span: Duration) -> Self
```

An aggregate's buckets: spans this long, each beginning at a whole
number of them since 1970 in UTC, the range cutting the first and the
last. Without it the range is one bucket.

### Query.by

```rust
pub fn by(self, labels: &[&str]) -> Self
```

The labels an aggregate keeps: the series of a name that share them
are one group. Without `by` and `without`, every series of a name is
one group.

### Query.without

```rust
pub fn without(self, labels: &[&str]) -> Self
```

The labels an aggregate leaves out, keeping every other: `&[]` keeps
every series apart.

### Query.estimate

```rust
pub fn estimate(self) -> Self
```

A timer's percentile also gives a point between the bounds of the
bucket of its spread that holds the rank, interpolated as Prometheus'
`histogram_quantile` does: an estimate, which no measurement gave.

### Query.read

```rust
pub fn read(&self) -> Result<Vec<Series>>
```

The samples of each series the query finds, in its range: a series
without one is left out. Past a bound of a read it is a `limit`
error, naming the bound, never fewer series or samples.

### Query.aggregate

```rust
pub fn aggregate(&self, op: Op) -> Result<Vec<Aggregate>>
```

The buckets of each group of the series the query finds, each one
exact value of `op`, rounded once: a group without a sample in the
range is left out, and so is a bucket without one. An `op` a series'
kind has not is `invalid`, and so is a bucket over a NaN, an infinity
or a counter below zero; past a bound it is a `limit` error, naming
the bound, never fewer buckets.

### Query.latest

```rust
pub fn latest(&self) -> Result<Vec<Latest>>
```

The newest sample of each series the query finds, when it has one in
the range: a series that stopped writing leaves the answer rather
than show its last value for ever.

### Query.explain

```rust
pub fn explain(&self, op: impl Into<Option<Op>>) -> Result<Plan>
```

What `read`, or `aggregate` when given its `op`, would spend of each
bound of a query, and the bound it would stop at. It decodes no
sample.

```rust
use tinystore::metrics::Op;
let month = stats.query().name("cpu").since(Duration::from_hours(30 * 24));
let plan = month.clone().every(Duration::from_hours(1)).explain(Op::Avg)?;
assert!(plan.exceeds.is_none(), "whole hours answer from their summaries");
let read = month.explain(None)?;
```

## Values

```rust
pub trait Values
```

The values a condition lists: text, or a number or a boolean as its text,
as a label keeps them.

### Values.values

```rust
fn values(self) -> Vec<String>
```

## Recorder

```rust
pub struct Recorder { /* private fields */ }
```

The `metrics` crate's recorder for a store's metrics.

### Recorder.buckets

```rust
pub fn buckets(self, name: impl Into<String>, buckets: impl Into<Buckets>) -> Recorder
```

The buckets of the histogram of `name`, as a timer's are.

## Kind

```rust
pub enum Kind {
    Gauge,
    Counter,
    Histogram,
}
```

What a series' samples are.

### Kind.as_str

```rust
pub fn as_str(self) -> &'static str
```

The kind as the file, the wire and every SDK say it.

## Labels

```rust
pub type Labels = BTreeMap<String, String>
```

The labels of a series beside its name, `route` → `/notes`: text, a number
given as its text.

## Sample

```rust
pub struct Sample {
    pub at: SystemTime,
    pub value: f64,
}
```

A time and a number, kept bit for bit: `-0`, infinities and a NaN's
payload come back as they went in. A time is kept to the millisecond.

## Series

```rust
pub struct Series { /* private fields */ }
```

A series: what `ingest` writes, built a label and a sample at a time, and
what a read gives back.

```rust
use tinystore::metrics::Series;
let at = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
let cpu = Series::gauge("cpu").label("host", "web-1").sample(at, 0.42);
assert_eq!(cpu.to_string(), r#"cpu{host="web-1"}"#);
```

### Series.gauge

```rust
pub fn gauge(name: impl Into<String>) -> Series
```

### Series.counter

```rust
pub fn counter(name: impl Into<String>) -> Series
```

### Series.histogram

```rust
pub fn histogram(name: impl Into<String>) -> Series
```

The `_bucket` series of a histogram written elsewhere, one series per
bound, each with its label `le`, beside its `_sum` and `_count`
counters.

### Series.label

```rust
pub fn label(self, name: impl Into<String>, value: impl Display) -> Series
```

One label more; a number or a boolean is kept as its text, `200` as
`"200"`.

### Series.sample

```rust
pub fn sample(self, at: SystemTime, value: f64) -> Series
```

### Series.keep

```rust
pub fn keep(self, keep: Duration) -> Series
```

How long the name's samples stay: the name's own, kept in the file.
Given none, the name keeps what it has, 30 days until one is given;
another than one given before while the store is open is `invalid`.

### Series.name

```rust
pub fn name(&self) -> &str
```

### Series.kind

```rust
pub fn kind(&self) -> Kind
```

### Series.labels

```rust
pub fn labels(&self) -> &Labels
```

### Series.samples

```rust
pub fn samples(&self) -> &[Sample]
```

## Metrics

```rust
pub struct Metrics { /* private fields */ }
```

The store's metrics, a handle that clones cheaply.

### Metrics.damaged

```rust
pub fn damaged(&self) -> Result<Vec<Damage>>
```

The series whose bytes no longer read, which a read, packing or
publication met: 1000 at most, the first the file holds, each with
what broke. Deleting them shows the next.

### Metrics.delete

```rust
pub fn delete<K: AsRef<str>, V: AsRef<str>>(
    &self,
    name: &str,
    labels: impl IntoIterator<Item = (K, V)>,
) -> Result<bool>
```

Removes the one series of exactly this name and these labels, no more
and no fewer, with every sample it holds, whether they still read or
not, and says whether the store held it: `delete("cpu", [("host",
"web-1")])`. Writing the same labels later starts a new series; the
name keeps its `keep`, unit and help.

### Metrics.describe

```rust
pub fn describe(&self, name: &str, description: Description) -> Result<()>
```

Sets a name's unit and help in place of what it had; empty ones
remove them, and `Description::default()` removes both.

### Metrics.description

```rust
pub fn description(&self, name: &str) -> Result<Description>
```

What a name's unit and help are; both empty when nothing described
it.

### Metrics.counter

```rust
pub fn counter(&self, name: impl Into<String>) -> Builder<Counter>
```

A total that only goes up, which the store keeps: processes counting
into one series count into one total.

### Metrics.gauge

```rust
pub fn gauge(&self, name: impl Into<String>) -> Builder<Gauge>
```

A value that goes up and down: a queue's length, a temperature.

### Metrics.gauge_func

```rust
pub fn gauge_func<R: Reading>(
    &self,
    name: impl Into<String>,
    read: impl Fn() -> R + Send + Sync + 'static,
) -> Builder<GaugeFunc>
```

A gauge whose value `read` gives at each write. It runs on the
thread that writes, the store's own, and must not block: a value
that takes time to read is a gauge the program sets.

### Metrics.timer

```rust
pub fn timer(&self, name: impl Into<String>) -> Builder<Timer>
```

How many things took how long: their count, their sum and the
longest, in milliseconds, and with `buckets` their spread.

### Metrics.histogram

```rust
pub fn histogram(&self, name: impl Into<String>) -> Builder<Histogram>
```

A timer of numbers that are not durations: a response's size.

### Metrics.flush

```rust
pub fn flush(&self) -> Result<()>
```

Writes every instrument's total now, as the store does every 15
seconds and when it closes.

### Metrics.names

```rust
pub fn names(&self) -> List<NamePage>
```

The names the store holds samples of, each with its kind, its count
of series, its unit and help, what it keeps and where that begins, and
its newest sample's time.

### Metrics.label_names

```rust
pub fn label_names(&self, name: impl Into<String>) -> List<LabelPage>
```

A name's label names, but the store's own.

### Metrics.label_values

```rust
pub fn label_values(&self, name: impl Into<String>, label: impl Into<String>) -> List<ValuePage>
```

The values a label of a name's series holds.

### Metrics.recorder

```rust
pub fn recorder(&self) -> Recorder
```

A recorder for the `metrics` crate, whose macros then count into
these metrics.

### Metrics.ingest

```rust
pub fn ingest(&self, series: &[Series]) -> Result<()>
```

Writes series and their samples, every one or none: a refused series
names itself by its labels, so the call can be sent again without it.
A time given twice keeps the value given last, until the store has
packed that stretch of its series; it returns once the samples are
written, and a read finds them at once.

### Metrics.query

```rust
pub fn query(&self) -> Query
```

A query of series by name and labels, over a range of time, which
`read`, `aggregate`, `latest` or `explain` then runs.

<!-- Generated by task reference from crates/tinystore through rustdoc. Edit the doc comments there, not this file. -->
