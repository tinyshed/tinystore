# Metrics API for Python

Every public class, function and type of the Metrics engine in `tinystore`, generated from its source. The [Metrics guide](../../guides/metrics/README.md) explains how to use them, and the [Bun and Node](../bun/metrics.md), [Go](../go/metrics.md) and [Rust](../rust/metrics.md) pages list the same API.

## SeriesQuery

```python
class SeriesQuery(TypedDict, total=False):
    name: str  # the series of this name; a timer's name, the series it writes
    where: Mapping[str, Condition]
    since: Duration  # the span before now the range covers; or from_ and to
    from_: datetime  # the range's start, included
    to: datetime  # the range's end, excluded; now, a sample taken now in the range, unless given
```

The series a call finds and its range; a condition given as None is InvalidError, never left out.

## AggregateQuery

```python
class AggregateQuery(SeriesQuery, total=False):
    every: Duration  # the buckets' span, counted from 1970 in UTC; the range is one bucket without it
    calendar: Calendar  # buckets of a calendar in time_zone, in place of every: "day" begins each at a local midnight
    time_zone: str  # the IANA zone a calendar's buckets are in, such as Europe/Moscow or UTC
    op: Op | str
    by: Sequence[str]  # the labels a group keeps; without by and without, every series of a name is one group
    without: Sequence[str]  # the labels a group leaves out, keeping every other; [] keeps every series apart
    estimate: bool  # a timer's percentile also gives a point between its bounds, interpolated: an estimate
```

An aggregate: a query cut into buckets of every, each one exact value of op, by groups.

## Series

```python
@dataclass(slots=True)
class Series:
    name: str
    kind: SeriesKind
    labels: Mapping[str, LabelValue] = field(default_factory=dict[str, LabelValue])
    samples: Sequence[tuple[Time, float]] | None = None
    times: Sequence[Time] | None = None
    values: Sequence[float] | None = None
    keep: Duration | None = None  # how long the name's samples stay: its own, kept in the file; unless given, what it keeps already
```

A series: what ingest writes, and what a read gives back.

Ingest takes its samples as (time, value) pairs, or as times and values
columns, a time an aware datetime or unix milliseconds. A read gives
``times``, unix milliseconds in an ``array('q')``, and ``values``, an
``array('d')`` whose bytes keep every value's bits: -0 and a NaN's
payload survive.

## AggregateBucket

```python
@dataclass(frozen=True, slots=True)
class AggregateBucket:
    at: datetime  # where it begins
    samples: int  # how many samples it comes from; a timer's, how many durations
    resets: int
    partial: bool  # its value comes from less than its span: the range, the clock or retention cut it
    gap: bool  # its first step began before the bucket before it: a whole bucket between had no samples
    value: float | None = None
    lower: float | None = None
    upper: float | None = None  # infinity past the last bound
    estimate: float | None = None
```

A bucket of an aggregate.

A bucket of count … last has its value; a percentile's is a gauge's sample
at the rank, or a timer's bucket of its spread that holds it, from lower
to upper, and with estimate asked a point between them.

## Aggregate

```python
@dataclass(frozen=True, slots=True)
class Aggregate:
    name: str
    labels: dict[str, str]  # the labels by kept or without left
    buckets: list[AggregateBucket]  # a bucket without samples is left out
```

## Point

```python
@dataclass(frozen=True, slots=True)
class Point:
    name: str
    labels: dict[str, str]
    at: datetime
    value: float
```

A series' newest sample in the range.

## Plan

```python
@dataclass(frozen=True, slots=True)
class Plan:
    series: int
    bytes: int
    decoded: int
    exceeds: Exceeds | None
```

What a read or an aggregate would spend of each bound, and the bound it would stop at.

## NameInfo

```python
@dataclass(frozen=True, slots=True)
class NameInfo:
    name: str
    kind: SeriesKind
    series_count: int
    unit: str
    help: str
    keep: str
    retained_from: datetime  # its oldest sample, or where its keep begins while it holds samples from before
    newest: datetime
```

A name the store holds samples of.

## NamePage

```python
@dataclass(frozen=True, slots=True)
class NamePage:
    names: list[NameInfo]
    next: str | None  # where the next page starts; none on the last
```

## MetricsDamage

```python
@dataclass(frozen=True, slots=True)
class MetricsDamage:
    name: str
    labels: dict[str, str]
    error: str
```

A series whose bytes no longer read, which delete removes.

## Metrics

```python
class Metrics:
    ...
```

A store's metrics: series by name and labels, their reads and aggregates, and the program's instruments.

### Metrics.ingest

```python
async def ingest(series: Series | Sequence[Series]) -> None: ...
```

Writes series and their samples, every one or none: a refused series names itself.

### Metrics.ingest_prometheus

```python
async def ingest_prometheus(
    text: str | bytes,
    *,
    labels: Mapping[str, LabelValue] | None = None,
    at: Time | None = None,
    keep: Duration | None = None,
) -> None: ...
```

Reads the text an exporter serves, in Prometheus' format or OpenMetrics', and writes its series.

Every one or none: a counter as a counter, a histogram's _bucket series as histogram. ``labels``
go beside every series' own, ``at`` is the time of a sample the text gives none, the store's
clock unless given, and ``keep`` how long every name of the text keeps its samples. A line it
cannot read is an InvalidError naming the line:

    text = httpx.get("http://web-1:9100/metrics").text
    await store.metrics.ingest_prometheus(text, labels={"instance": "web-1"})

### Metrics.read

```python
async def read(**query: Unpack[SeriesQuery]) -> list[Series]: ...
```

The samples of each series the query finds, in its range: whole, or a LimitError naming its bound.

### Metrics.latest

```python
async def latest(**query: Unpack[SeriesQuery]) -> list[Point]: ...
```

The newest sample of each series the query finds, when it has one in the range.

### Metrics.aggregate

```python
async def aggregate(**query: Unpack[AggregateQuery]) -> list[Aggregate]: ...
```

The buckets of each group of the series the query finds, each one exact value of op, rounded once.

### Metrics.explain

```python
async def explain(**query: Unpack[AggregateQuery]) -> Plan: ...
```

What a read, or an aggregate when given its op, would spend of each bound; it decodes no sample.

### Metrics.names

```python
async def names(
    *,
    prefix: str | None = None,
    limit: int | None = None,
    after: str | None = None,
) -> NamePage: ...
```

A page of the names the store holds samples of, in the byte order of their text.

### Metrics.label_names

```python
async def label_names(
    name: str,
    *,
    prefix: str | None = None,
    limit: int | None = None,
    after: str | None = None,
) -> LabelPage: ...
```

A page of a name's label names, but the store's own.

### Metrics.label_values

```python
async def label_values(
    name: str,
    label: str,
    *,
    prefix: str | None = None,
    limit: int | None = None,
    after: str | None = None,
) -> ValuePage: ...
```

A page of the values a label of a name's series holds.

### Metrics.describe

```python
async def describe(name: str, *, unit: str = '', help: str = '') -> None: ...
```

Sets a name's unit and help in place of what it had: what is not given is removed.

They belong to the name, so a delete of its series leaves them.

### Metrics.description

```python
async def description(name: str) -> Description: ...
```

A name's unit and help; both empty when nothing described it.

### Metrics.delete

```python
async def delete(*, name: str, labels: Mapping[str, LabelValue]) -> bool: ...
```

Removes the one series of exactly this name and these labels, whether its samples still read or not.

True when the store held it; ``labels={}`` is the series without labels.

### Metrics.damaged

```python
async def damaged() -> list[MetricsDamage]: ...
```

The series whose bytes no longer read, 1000 at most, each with what broke; deleting them shows the next.

### Metrics.flush

```python
async def flush() -> None: ...
```

Sends what every instrument changed and has the store write its totals now, as it does every 15 seconds.

### Metrics.counter

```python
def counter(
    name: str,
    *,
    unit: str | None = None,
    help: str | None = None,
    keep: Duration | None = None,
) -> Counter: ...
```

A total that only goes up, which the store keeps: processes counting into one series count into one total.

### Metrics.gauge

```python
def gauge(
    name: str,
    *,
    unit: str | None = None,
    help: str | None = None,
    keep: Duration | None = None,
) -> Gauge: ...
```

A value that goes up and down: a queue's length, a temperature.

### Metrics.gauge_func

```python
def gauge_func(
    name: str,
    read: Callable[[], float | Awaitable[float]],
    *,
    labels: Mapping[str, LabelValue] | None = None,
    unit: str | None = None,
    help: str | None = None,
    keep: Duration | None = None,
) -> None: ...
```

A gauge whose value read gives every 15 seconds, of the set of labels given.

Another set of the name takes a function of its own. It may be async,
is never called again while a call of it runs, and an error or an
answer that is not a number writes nothing that time, said once while
it repeats:

    store.metrics.gauge_func("queue_depth", emails.depth, labels={"queue": "emails"})

### Metrics.timer

```python
def timer(
    name: str,
    *,
    buckets: Sequence[float] | Literal['exponential'] | None = None,
    help: str | None = None,
    keep: Duration | None = None,
) -> Timer: ...
```

How many things took how long: their count, sum and longest in milliseconds, and their spread.

### Metrics.histogram

```python
def histogram(
    name: str,
    *,
    buckets: Sequence[float] | Literal['exponential'] | None = None,
    unit: str | None = None,
    help: str | None = None,
    keep: Duration | None = None,
) -> Histogram: ...
```

A timer of numbers that are not durations: a response's size in bytes.

## exponential_buckets

```python
def exponential_buckets(start: float, factor: float, count: int) -> list[float]: ...
```

Upper bounds from start, each factor times the one before: exponential_buckets(1, 2, 14) is 1, 2, 4 … 8192.

## Instruments

```python
class Instruments:
    ...
```

The instruments of a store's handle, and what sends their changes; ``store.metrics`` makes them.

### Instruments.declare

```python
def declare(
    name: str,
    form: Form,
    *,
    unit: str | None = None,
    help_: str | None = None,
    keep: Duration | None = None,
    buckets: Sequence[float] | Literal['exponential'] | None = None,
    read: bool = False,
) -> _Declared: ...
```

An instrument of name, made the first time; another form or other options than it has are refused.

### Instruments.reader

```python
def reader(
    name: str,
    labels: dict[str, str],
    read: Callable[[], float | Awaitable[float]],
    **told: Any,
) -> None: ...
```

A gauge of name whose set of labels read gives every 15 seconds; each set has a function of its own.

### Instruments.set_of

```python
def set_of(instrument: _Declared, labels: dict[str, str]) -> _Set: ...
```

The set of labels of an instrument, made the first time; past the sets it holds, its __overflow.

### Instruments.pending

```python
def pending(set_: _Set) -> _Change | None: ...
```

### Instruments.change

```python
def change(set_: _Set) -> _Change: ...
```

What a call changes of a set, kept for the next batch.

### Instruments.flush

```python
async def flush() -> bool: ...
```

Sends what changed now and has the store write its totals; says whether there was anything to send.

One the store cannot be reached for fails as a call does, and what it
carried goes once the store can be. What the store refused since the
flush before, and what a store that started again may have lost, fail
it, each once.

### Instruments.stop

```python
async def stop() -> None: ...
```

Stops the timers, sends what is left, and ends the stream; the store's close calls it.

## Counter

```python
class Counter:
    ...
```

A total that only goes up, which the store keeps: processes counting into one series count into one total.

### Counter.labels

```python
def labels(**labels: LabelValue) -> Counter: ...
```

The same counter with more labels, a series of its own; the same labels in any order are one.

### Counter.inc

```python
def inc() -> None: ...
```

### Counter.add

```python
def add(n: float) -> None: ...
```

Adds n, a finite number of 0 or more; add(0) makes the set of labels known with a total of zero.

## Gauge

```python
class Gauge:
    ...
```

A value that goes up and down, written while this program is connected.

### Gauge.labels

```python
def labels(**labels: LabelValue) -> Gauge: ...
```

### Gauge.set

```python
def set(value: float) -> None: ...
```

### Gauge.add

```python
def add(n: float) -> None: ...
```

Moves it by n, from zero when it was never set.

### Gauge.inc

```python
def inc() -> None: ...
```

### Gauge.dec

```python
def dec() -> None: ...
```

## Timer

```python
class Timer:
    ...
```

How many things took how long: their count, sum and longest in milliseconds, and their spread.

### Timer.labels

```python
def labels(**labels: LabelValue) -> Timer: ...
```

### Timer.record

```python
def record(duration: timedelta | Duration) -> None: ...
```

Records a duration measured elsewhere: a timedelta, or text such as "250ms".

### Timer.measure

```python
def measure() -> Timing: ...
```

Measures the block it runs: ``async with latency.measure():``, recorded whether it returns or raises.

### Timer.start

```python
def start() -> Timing: ...
```

A timing that records when it stops, for a span that is no one block.

## Timing

```python
class Timing:
    ...
```

A span a timer measures from its start; stop records it, once.

### Timing.stop

```python
def stop() -> timedelta: ...
```

Records how long since the timing started, the first time it is called, and gives it.

## Histogram

```python
class Histogram:
    ...
```

A timer of numbers that are not durations: a response's size in bytes.

### Histogram.labels

```python
def labels(**labels: LabelValue) -> Histogram: ...
```

### Histogram.record

```python
def record(value: float) -> None: ...
```

Records a value of 0 or more.

<!-- Generated by task reference from sdk/python/src/tinystore/metrics.py, sdk/python/src/tinystore/instruments.py. Edit the doc comments there, not this file. -->
