# Metrics API for Bun and Node

Every public class, function and type of the Metrics engine in `@tinyshed/tinystore`, generated from its source. The [Metrics guide](../../metrics/README.md) explains how to use them, and the [Python](../python/metrics.md) and [Go](../go/metrics.md) pages list the same API.

## SeriesKind

```ts
type SeriesKind = 'gauge' | 'counter'
```

## Labels

```ts
type Labels = Record<string, string>
```

What tells the series of one name apart: a host, a route, a status. A label
whose name begins with __ is the store's own, and refused.

## SeriesInput

```ts
interface SeriesInput {
    /** the metric: 'http_requests_total' */
    name: string
    kind: SeriesKind
    labels?: Labels
    /** samples as [time, value] pairs, or as columns: unix milliseconds and their values */
    samples: readonly (readonly [time: Time, value: number])[] | {
        times: readonly number[] | BigInt64Array
        values: readonly number[] | Float64Array
    }
}
```

## Series

```ts
interface Series {
    name: string
    kind: SeriesKind
    labels: Labels
    /** unix milliseconds, in time order */
    times: number[]
    /** the values, their bits as they were ingested: -0 and a NaN's payload survive */
    values: Float64Array
}
```

## Condition

```ts
class Condition {
    readonly kind: 'one_of' | 'none_of' | 'prefix'
    readonly values: readonly string[]
}
```

What a label's value must be beyond equality, made by oneOf, noneOf or
prefix: an object of the SDK's own, so that a stored value is never read as
a query.

## oneOf

```ts
function oneOf(...values: string[]): Condition
```

The label is one of the values.

## noneOf

```ts
function noneOf(...values: string[]): Condition
```

The label is none of the values, or the series has none.

## prefix

```ts
function prefix(start: string): Condition
```

The label's value begins with the prefix.

## Plan

```ts
interface Plan {
    series: number
    blocks: number
    /** the blocks an aggregate answers from their summaries, decoding none of their samples */
    summarized: number
    bytes: number
    decoded: number
    limits: {
        series: number
        blocks: number
        bytes: number
        decoded: number
        answered: number
    }
    /** the limit the call would reach, which it would end with; undefined when it fits */
    stops: LimitError | undefined
}
```

What a read or an aggregate would spend, each use beside its limit.

## Range

```ts
interface Range {
    name?: string
    /** labels a series holds, each exactly */
    match?: Labels
    /**
     * labels under a condition: `{ status: oneOf('500', '502'), env: noneOf('dev'), host: prefix('api-') }`
     * a plain value is equality. A range of noneOf alone is refused, since it would scan every series.
     */
    where?: Record<string, Condition | string>
    /** the span before now the range covers: '1h', '15m', or milliseconds */
    since?: Duration
    /** the range's start, included; the oldest sample kept when absent */
    from?: Time
    /** excluded; the open end when absent */
    to?: Time
    /** each narrows the server's: series matched, blocks decoded, bytes fetched, samples decoded, answered */
    limits?: {
        series?: number
        blocks?: number
        bytes?: number
        decoded?: number
        answered?: number
    }
}
```

## AggregateOp

```ts
type AggregateOp = 'count' | 'sum' | 'min' | 'max' | 'avg' | 'increase' | 'rate' | 'delta' | 'first' | 'last'
```

Each computed exactly and rounded once, a group's too: avg is the mean of
every sample, rate a counter's increase a second, delta how far a gauge
moved, first and last the bucket's first and last samples, which a group
adds up. An increase, a rate or a delta counts each step between samples in
the bucket it ends in, so buckets add up to the whole range.

## AggregateRange

```ts
interface AggregateRange extends Range {
    width: Duration
    op: AggregateOp
    by?: string[]
    without?: string[]
    /** how far before the range an increase, a rate or a delta looks for its first step: one width when absent */
    lookback?: Duration
}
```

An aggregate's buckets and, to join series, what groups them: by these
labels, or without them, each group one result. `by: []` joins every series
of a name. A group never joins two names or two kinds.

## Bucket

```ts
interface Bucket {
    from: Date
    to: Date
    /** the samples it counted */
    count: number
    /** the resets among them, a counter's */
    resets: number
    /** computed exactly and rounded once */
    value: number
    /** the value overflowed to an infinity */
    overflow: boolean
    /** retention cut the bucket, which counted only its samples from the cutoff on, or the step into its first */
    partial: boolean
    /** its first step started from a sample before the range */
    lookback: boolean
}
```

## Description

```ts
interface Description {
    /** 32 bytes of UTF-8 at most */
    unit?: string
    /** 1024 bytes of UTF-8 at most */
    help?: string
}
```

What the values of a metric's name mean: their unit, such as 'ms', 'bytes' or '%', and a line of help.

## Aggregate

```ts
interface Aggregate {
    name: string
    kind: SeriesKind
    labels: Labels
    /** only buckets holding samples */
    buckets: Bucket[]
}
```

## Metrics

```ts
class Metrics {
    failures: number
    lastFailure: Error | undefined
}
```

### Metrics.ingest

```ts
ingest(series: SeriesInput | readonly SeriesInput[]): Promise<void>
```

Stores series and their samples, all or none; a refused series is named by its labels.

### Metrics.read

```ts
read(range: Range): Promise<Series[]>
```

Every sample of the series a range matches, exactly, read whole before the first leaves the server.

### Metrics.latest

```ts
latest(range: Range): Promise<Series[]>
```

The newest sample of each series a range matches, one sample a series.
The range bounds how old it may be: a series without one in it is left
out, so a series that stopped reads as absent rather than as its last value.

### Metrics.aggregate

```ts
aggregate(range: AggregateRange): Promise<Aggregate[]>
```

Buckets of a width from the range's start, each computed exactly: a
counter's increase counts its resets, and the first bucket the step from
the last sample a lookback before the range.

### Metrics.explain

```ts
explain(range: Range & Partial<Pick<AggregateRange, 'width' | 'op' | 'by' | 'without' | 'lookback'>>): Promise<Plan>
```

What read(range), or aggregate(range) when it names an op, would spend
of its limits, found without a payload fetched or a sample decoded: the
series, the blocks, those an aggregate answers from their summaries, the
bytes and the samples, beside the limits; stops is the LimitError the
call would end with, undefined when it fits.

### Metrics.drop

```ts
drop(series: {
        name: string
        labels?: Labels
    }): Promise<{
        found: boolean
        unreadableGroups: number
    }>
```

Removes one series and everything it holds, whether it still reads or not.

### Metrics.describe

```ts
describe(name: string, description: Description): Promise<void>
```

Keeps what a name's values mean in place of what they meant; with
neither a unit nor help it removes it. Every series of the name shares it.

### Metrics.description

```ts
description(name: string): Promise<{
        unit: string
        help: string
    }>
```

What describe kept for a name, empty strings when it has none.

### Metrics.counter

```ts
counter(name: string, description?: Description): Counter
```

A counter: its total since this process started, ingested every flush;
a restart is a reset. A description is written at the next flush.

### Metrics.gauge

```ts
gauge(name: string, description?: Description): Gauge
```

A gauge: its value at each flush.

### Metrics.gaugeFunc

```ts
gaugeFunc(name: string, read: () => number | Promise<number>, description?: Description): void
```

A gauge read by a function at each flush; a function that throws skips that sample.

### Metrics.timer

```ts
timer(name: string, description?: Description): Timer
```

A timer: how many durations it measured and their sum in milliseconds,
ingested every flush as the counters name_count and name_sum, and the
longest since the flush before as the gauge name_max, left out when it
measured none. A range's mean is its sum's increase over its count's.
Given a description, its sum and longest are in milliseconds, whatever
unit it names.

### Metrics.flush

```ts
flush(): Promise<void>
```

Ingests every instrument's value now, as the timer does every 15 s. A
series the store refuses is left out from then on, said once, so that one
bad instrument keeps no other out.

### Metrics.stop

```ts
stop(): Promise<void>
```

Stops flushing every 15 s and ingests the instruments' last values; the store's close calls it.

### Metrics.instruments

```ts
get instruments(): number
```

## Counter

```ts
class Counter {}
```

### Counter.with

```ts
with(labels: Labels): Counter
```

The counter of these labels besides its own: the same labels in any order are one series.

### Counter.inc

```ts
inc(): void
```

### Counter.add

```ts
add(n: number): void
```

## Gauge

```ts
class Gauge {}
```

### Gauge.with

```ts
with(labels: Labels): Gauge
```

### Gauge.set

```ts
set(value: number): void
```

### Gauge.add

```ts
add(n: number): void
```

### Gauge.inc

```ts
inc(): void
```

### Gauge.dec

```ts
dec(): void
```

## Timer

```ts
class Timer {}
```

A timer of labels; record and measure add a duration to the next flush.

### Timer.with

```ts
with(labels: Labels): Timer
```

The timer of these labels besides its own: the same labels in any order are one series.

### Timer.record

```ts
record(d: Duration): void
```

Adds one duration: milliseconds, their fractions kept, or text such as '250ms'.

### Timer.measure

```ts
measure<R>(fn: () => R | Promise<R>): Promise<R>
```

Runs fn and records how long it took, whether it returned or threw: its
result comes back, and what it threw is thrown again.

    const user = await latency.with({ route: '/users' }).measure(() => users.get(id))

<!-- Generated by task reference from sdk/js/src/metrics.ts. Edit the doc comments there, not this file. -->
