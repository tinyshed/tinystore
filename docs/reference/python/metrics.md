# Metrics API for Python

Every public class, function and type of the Metrics engine in `tinystore`, generated from its source. The [Metrics guide](../../metrics/README.md) explains how to use them, and the [Bun and Node](../bun/metrics.md) and [Go](../go/metrics.md) pages list the same API.

## Series

```python
@dataclass(frozen=True, slots=True)
class Series:
    name: str
    kind: str
    labels: dict[str, str]
    times: list[int]  # unix milliseconds, in time order
    values: array[float]  # their bits as they were ingested: -0 and a NaN's payload survive
```

## Bucket

```python
@dataclass(frozen=True, slots=True)
class Bucket:
    from_: datetime | None
    to: datetime | None
    count: int
    resets: int
    value: float
    overflow: bool
    partial: bool  # retention cut the bucket, which counted only its samples from the cutoff on
```

## Aggregate

```python
@dataclass(frozen=True, slots=True)
class Aggregate:
    name: str
    kind: str
    labels: dict[str, str]
    buckets: list[Bucket]
```

## Plan

```python
@dataclass(frozen=True, slots=True)
class Plan:
    series: int
    blocks: int
    summarized: int
    bytes: int
    decoded: int
    limits: dict[str, int]
    stops: LimitError | None
```

What a read or an aggregate would spend, each use beside its limit; stops is the limit it would reach.

## Condition

```python
@dataclass(frozen=True, slots=True)
class Condition:
    kind: Literal['one_of', 'none_of', 'prefix']
    values: tuple[str, ...]
```

What a label's value must be beyond equality, made by one_of, none_of or prefix.

An object of the SDK's own, so that a stored value is never read as a query.

## one_of

```python
def one_of(*values: str) -> Condition: ...
```

The label is one of the values.

## none_of

```python
def none_of(*values: str) -> Condition: ...
```

The label is none of the values, or the series has none.

## prefix

```python
def prefix(start: str) -> Condition: ...
```

The label's value begins with start.

## Metrics

```python
class Metrics:
    ...
```

### Metrics.ingest

```python
async def ingest(*series: Mapping[str, Any]) -> None: ...
```

Stores series and their samples, all or none; a refused series is named by its labels.

A series is a name, a kind, labels when it has any, and samples as
(time, value) pairs, or times in unix milliseconds and values:

    await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"},
                                "samples": [(datetime.now(UTC), 0.42)]})

### Metrics.read

```python
async def read(
    *,
    name: str | None = None,
    match: Mapping[str, str] | None = None,
    where: Mapping[str, Condition | str] | None = None,
    since: Duration | None = None,
    from_: datetime | int | None = None,
    to: datetime | int | None = None,
    limits: Mapping[str, int] | None = None,
) -> list[Series]: ...
```

Every sample of the series a range matches, exactly.

The series of a name, of labels, or of both, each matched exactly; over
the last ``since``, or from ``from_`` to ``to``, in datetimes or unix
milliseconds, ``to`` open when absent:

    await store.metrics.read(name="cpu", since="1h")
    await store.metrics.read(match={"host": "web-1"}, from_=start, to=end)

### Metrics.aggregate

```python
async def aggregate(
    *,
    width: Duration,
    op: Op,
    name: str | None = None,
    match: Mapping[str, str] | None = None,
    where: Mapping[str, Condition | str] | None = None,
    since: Duration | None = None,
    from_: datetime | int | None = None,
    to: datetime | int | None = None,
    limits: Mapping[str, int] | None = None,
    by: Iterable[str] | None = None,
    without: Iterable[str] | None = None,
) -> list[Aggregate]: ...
```

Buckets of a width from the range's start, each computed exactly and rounded once.

avg is the mean of every sample, rate a counter's increase a second,
delta a gauge's last sample less its first. by groups the series by
those labels, without by every label but those, each group one
result; ``by=[]`` joins every series of a name:

    await store.metrics.aggregate(name="http_requests_total", since="1d", width="1h", op="rate", by=["route"])

### Metrics.drop

```python
async def drop(
    name: str,
    labels: Mapping[str, str] | None = None,
) -> tuple[bool, int]: ...
```

Removes one series and everything it holds: whether it was there, and the unreadable groups removed.

### Metrics.explain

```python
async def explain(
    *,
    name: str | None = None,
    match: Mapping[str, str] | None = None,
    where: Mapping[str, Condition | str] | None = None,
    since: Duration | None = None,
    from_: datetime | int | None = None,
    to: datetime | int | None = None,
    limits: Mapping[str, int] | None = None,
    width: Duration | None = None,
    op: Op | None = None,
    by: Iterable[str] | None = None,
    without: Iterable[str] | None = None,
) -> Plan: ...
```

What read, or aggregate when op is given, would spend of its limits, without reading a sample.

The series, the blocks, those an aggregate answers from their
summaries, the bytes and the samples, beside the limits; stops is the
LimitError the call would end with, None when it fits.

### Metrics.counter

```python
def counter(name: str) -> Counter: ...
```

A counter: its total since this process started, ingested every flush; a restart is a reset.

### Metrics.gauge

```python
def gauge(name: str) -> Gauge: ...
```

### Metrics.gauge_func

```python
def gauge_func(name: str, read: Callable[[], float | Awaitable[float]]) -> None: ...
```

A gauge read by a function at each flush; one that raises skips that sample.

### Metrics.timer

```python
def timer(name: str) -> Timer: ...
```

A timer: how many durations it measured and their sum in milliseconds, and the longest.

Every flush ingests the first two as the counters name_count and
name_sum, and the longest since the flush before as the gauge
name_max, left out when it measured none. A range's mean is its sum's
increase over its count's.

### Metrics.flush

```python
async def flush() -> None: ...
```

Ingests every instrument's value now, as the timer does every 15 s.

A series the store refuses is left out from then on, said once, so that
one bad instrument keeps no other out.

### Metrics.stop

```python
async def stop() -> None: ...
```

Stops flushing every 15 s and ingests the instruments' last values, as the store does as it closes.

<!-- Generated by task reference from sdk/python/src/tinystore/metrics.py. Edit the doc comments there, not this file. -->
