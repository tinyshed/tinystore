# Metrics API for Bun and Node

Every public class, function and type of the Metrics engine in `@tinyshed/tinystore`, generated from its source. The [Metrics guide](../../guides/metrics/README.md) explains how to use them, and the [Python](../python/metrics.md), [Go](../go/metrics.md) and [Rust](../rust/metrics.md) pages list the same API.

## SeriesKind

```ts
type SeriesKind = 'gauge' | 'counter' | 'histogram'
```

What a series' samples are.

## Labels

```ts
type Labels = Record<string, string>
```

What tells the series of one name apart: a host, a route, a status.

## Condition

```ts
type Condition = LabelValue | readonly LabelValue[] | {
    not: LabelValue | readonly LabelValue[]
} | {
    prefix: string
}
```

What a label's value must be: a value, one of a list, none of them with
`not` (or the label absent), or a prefix. A number or a boolean is its
text, as in an instrument's `with`.

## Query

```ts
interface Query {
    /** the series of this name; a timer's name, the series it writes */
    name?: string | undefined
    where?: Record<string, Condition> | undefined
    /** the span before now the range covers; or from and to */
    since?: Duration | undefined
    /** the range's start, included */
    from?: Time | undefined
    /** the range's end, excluded; now, a sample taken now in the range, unless given */
    to?: Time | undefined
    /** a page of read or aggregate: this many samples or buckets at most, 100,000 at most; without it, the whole answer or a limit error */
    limit?: number | undefined
    /** the page after the one whose next this is, read with the same query and limit */
    after?: string | undefined
}
```

The series a call finds and its range: `{ name: 'cpu', where: { host:
'web-1' }, since: '1h' }`. A query is data, the same in every language.

## SeriesInput

```ts
interface SeriesInput {
    name: string
    kind: SeriesKind
    labels?: Record<string, LabelValue>
    samples?: readonly (readonly [time: Time, value: number])[]
    times?: readonly Time[] | Float64Array
    values?: readonly number[] | Float64Array
    /** how long the name's samples stay: its own, kept in the file; unless given, what it keeps already */
    keep?: Duration | undefined
}
```

A series to write: its samples as [time, value] pairs, or as times and values columns.

## PrometheusOptions

```ts
interface PrometheusOptions {
    /** labels every series gets beside its own, such as the instance scraped; one the text gives too is refused, naming its line */
    labels?: Record<string, LabelValue> | undefined
    /** the time of every sample the text gives none; the store's clock unless given */
    at?: Time | undefined
    /** how long every name of the text keeps its samples */
    keep?: Duration | undefined
}
```

What an ingest of an exporter's text gives every series of it.

## Series

```ts
interface Series {
    name: string
    kind: SeriesKind
    labels: Labels
    /** unix milliseconds, in the order of their times */
    times: Float64Array
    /** the values, whose bytes keep each one's bits: -0 and a NaN's payload survive */
    values: Float64Array
    /** a page cut the series here: the page its next reads begins with the rest of it */
    more: boolean
}
```

A series as a read gives it: its samples as two columns.

## SeriesPage

```ts
interface SeriesPage extends AsyncIterable<Series> {
    series: Series[]
    /** where the page after it begins; absent on the last */
    next?: string
}
```

One page of a read: its series, and next while there is more. Walking it
with for await gives its series, then those of each page after it, one
page held at a time. Without limit it is the whole answer.

## Percentile

```ts
type Percentile = 'p50' | 'p75' | 'p90' | 'p95' | 'p99' | 'p99.9' | (`p${number}` & Record<never, never>)
```

p and a rank: 'p95', 'p99.9'. The ranks offered are those asked most; any other is written the same way.

## Op

```ts
type Op = 'count' | 'sum' | 'avg' | 'min' | 'max' | 'increase' | 'rate' | 'delta' | 'first' | 'last' | Percentile
```

What a bucket answers: count … last, or a percentile; rate is a second's increase.

## AggregateQuery

```ts
interface AggregateQuery<O extends Op = Op, F extends boolean = boolean> extends Query {
    /** the buckets' span, counted from 1970 in UTC; the range is one bucket without it */
    every?: Duration | undefined
    /** buckets of a calendar in timeZone, in place of every: 'day' begins each at a local midnight */
    calendar?: Calendar | undefined
    /** the IANA zone a calendar's buckets are in: 'Europe/Moscow', 'UTC' */
    timeZone?: string | undefined
    op: O
    /** the labels a group keeps; without by and without, every series of a name is one group */
    by?: readonly string[] | undefined
    /** the labels a group leaves out, keeping every other; [] keeps every series apart */
    without?: readonly string[] | undefined
    /** a timer's percentile also gives a point between its bounds, interpolated: an estimate */
    estimate?: boolean | undefined
    /** a NaN sample counts in no bucket, which says how many it passed over in skipped; an infinity is still refused */
    skipNaN?: boolean | undefined
    /** every bucket of the range retention keeps, one without samples with samples 0 and no value; with since or from, not limit */
    fill?: F | undefined
}
```

An aggregate: a query cut into buckets of every, each one exact value of op, by groups.

## CompareQuery

```ts
interface CompareQuery {
    /** the series of this name, merged into one answer as an aggregate without by merges them */
    name: string
    where?: Record<string, Condition> | undefined
    op: Op
    window: Duration
    /** how far back the windows compared with reach */
    against: Duration
    /** the same hours of each earlier day alone: against '7d' compares with 7 windows */
    sameHour?: boolean | undefined
    /** a timer's percentile compares its estimate; without it such a percentile is refused */
    estimate?: boolean | undefined
    skipNaN?: boolean | undefined
}
```

A comparison: op over the newest window, `[now − window, now]`, against
the usual, the median of op over the windows before it within against:
the same hours of each earlier day with sameHour, a day 24 hours in no
time zone, or every window before otherwise, 1000 at most.

## Compared

```ts
interface Compared {
    /** op over the newest window; absent when it holds no sample */
    value?: number
    /** the median over the windows before that hold samples; absent when none does */
    usual?: number
    /** value / usual; absent when either is absent or usual is 0 */
    ratio?: number
    /** the samples of the newest window */
    samples: number
    /** the windows the usual stands on */
    windows: number
}
```

What compare found; a value it could not give is absent, never zero.

## ValueBucket

```ts
interface ValueBucket extends Spanned {
    value: number
}
```

A bucket of count … last: its one exact value.

## PercentileBucket

```ts
interface PercentileBucket extends Spanned {
    value?: number
    lower?: number
    /** Infinity past the last bound */
    upper?: number
    estimate?: number
}
```

A bucket of a percentile: a gauge's sample at the rank, as value, or a
timer's bucket of its spread that holds it, from lower to upper, and with
estimate asked a point between them.

## Filled

```ts
type Filled<B extends Spanned> = Omit<B, 'value'> & {
    value?: number
}
```

A bucket an aggregate given fill may answer empty: samples 0 and no value, which is no zero.

## Bucket

```ts
type Bucket<O extends Op = Op, F extends boolean = false> = true extends F ? Filled<Answered<O>> : Answered<O>
```

A bucket of an aggregate of op: a value, or what a percentile gives; one of fill may be empty.

## Aggregate

```ts
interface Aggregate<O extends Op = Op, F extends boolean = false> {
    name: string
    /** the labels by kept or without left */
    labels: Labels
    /** a bucket without samples is left out, and so is one of NaN alone, unless the query gave fill */
    buckets: Bucket<O, F>[]
    /** a page cut the group here: the page its next reads begins with the rest of its buckets */
    more: boolean
}
```

## AggregatePage

```ts
interface AggregatePage<O extends Op = Op, F extends boolean = false> extends AsyncIterable<Aggregate<O, F>> {
    groups: Aggregate<O, F>[]
    /** where the page after it begins; absent on the last */
    next?: string
}
```

One page of an aggregate: its groups, and next while there is more,
walked as a SeriesPage is. A page ends between two whole buckets.

## Newest

```ts
interface Newest {
    name: string
    labels: Labels
    at: Date
    value: number
}
```

A series' newest sample in the range.

## Plan

```ts
interface Plan {
    series: number
    bytes: number
    decoded: number
    exceeds: {
        /** series, bytes read, samples decoded or answer */
        limit: string
        wanted: number
        bound: number
    } | undefined
}
```

What a read or an aggregate would spend of each bound, and the bound it would stop at.

## PageOptions

```ts
interface PageOptions {
    prefix?: string | undefined
    /** 1000 unless given, 10,000 at most */
    limit?: number | undefined
    /** a page's next, so that a page's options can be the one before's */
    after?: string | undefined
}
```

A page's options: a prefix, how many, and where the page before ended.

## NameInfo

```ts
interface NameInfo {
    name: string
    kind: SeriesKind
    seriesCount: number
    unit: string
    help: string
    keep: DurationText
    /** its oldest sample, or where its keep begins while it holds samples from before */
    retainedFrom: Date
    newest: Date
}
```

A name the store holds samples of.

## NamePage

```ts
interface NamePage {
    names: NameInfo[]
    /** where the next page starts; none on the last */
    next?: string
}
```

## LabelPage

```ts
interface LabelPage {
    labels: string[]
    next?: string
}
```

## ValuePage

```ts
interface ValuePage {
    values: string[]
    next?: string
}
```

## Description

```ts
interface Description {
    unit?: string
    help?: string
}
```

A name's unit and help; empty ones remove them.

## Damage

```ts
interface Damage {
    name: string
    labels: Labels
    error: string
}
```

A series whose bytes no longer read, which delete removes.

## Metrics

```ts
class Metrics {}
```

A store's metrics: series by name and labels, their reads and aggregates,
what the store holds of them, and the program's instruments.

### Metrics.ingest

```ts
ingest(series: SeriesInput | readonly SeriesInput[]): Promise<void>
```

Writes series and their samples in one write, every one or none: a
refused series names itself. One call takes 100,000 samples, as one
exporter's text does; one past a message goes up in pieces.

### Metrics.replace

```ts
replace(series: SeriesInput, range: {
        from: Time
        to: Time
    }): Promise<void>
```

Replaces a gauge's samples from `from` to `to`, `to` excluded, with the series' own,
whether the store had packed them or not: a time given takes its value, a time not there
before is added, and a time the series does not give goes. One write: a read sees the
range as it was or as it is now, never half, and the same samples sent again write
nothing. A counter's or a histogram's is an InvalidError.

    // the last three days read again from the shop's database, a refund taken off yesterday
    await store.metrics.replace(
      { name: 'revenue', kind: 'gauge', labels: { shop: 'msk' }, samples: [[day(3), 1000], [day(2), 1000], [day(1), 900]] },
      { from: day(3), to: day(0) },
    )

### Metrics.ingestPrometheus

```ts
ingestPrometheus(text: string | Uint8Array, options?: PrometheusOptions): Promise<void>
```

Reads the text an exporter serves, in Prometheus' format or OpenMetrics', and writes its
series, every one or none: a counter as a counter, a histogram's _bucket series as
histogram. A line it cannot read is refused, naming the line.

    const text = await (await fetch('http://web-1:9100/metrics')).text()
    await store.metrics.ingestPrometheus(text, { labels: { instance: 'web-1' } })

### Metrics.read

```ts
read(query: Query): Promise<SeriesPage>
```

The samples of each series the query finds, in its range: without limit
the whole answer, or a limit error naming its bound; with limit a page,
which may hold fewer samples than limit and still have next.

### Metrics.latest

```ts
latest(query: Query): Promise<Newest[]>
```

The newest sample of each series the query finds, when it has one in the range.

### Metrics.aggregate

```ts
aggregate<O extends Op, F extends boolean = false>(query: AggregateQuery<O, F>): Promise<AggregatePage<O, F>>
```

The buckets of each group of the series the query finds, each one exact
value of op, rounded once; a bucket of count … last has its value.

### Metrics.explain

```ts
explain(query: Query & Partial<Omit<AggregateQuery, keyof Query>>): Promise<Plan>
```

What a read, or an aggregate when given its op, would spend of each bound; it decodes no sample.

### Metrics.compare

```ts
compare(query: CompareQuery): Promise<Compared>
```

op over the newest window against the median of the windows before it:
`{ name: 'moodle_call', op: 'p50', window: '15m', against: '7d', sameHour: true }`.

### Metrics.names

```ts
names(options?: PageOptions): Promise<NamePage>
```

A page of the names the store holds samples of, in the byte order of their text.

### Metrics.labelNames

```ts
labelNames(name: string, options?: PageOptions): Promise<LabelPage>
```

A page of a name's label names, but the store's own.

### Metrics.labelValues

```ts
labelValues(name: string, label: string, options?: PageOptions): Promise<ValuePage>
```

A page of the values a label of a name's series holds.

### Metrics.describe

```ts
describe(name: string, description: Description): Promise<void>
```

Sets a name's unit and help in place of what it had, as the Rust metrics
crate's describe_counter! does: what is not given is removed, and `{}`
removes both. They belong to the name, so a delete of its series leaves them.

### Metrics.description

```ts
description(name: string): Promise<{
        unit: string
        help: string
    }>
```

A name's unit and help; both empty when nothing described it.

### Metrics.delete

```ts
delete(series: {
        name: string
        labels: Record<string, LabelValue>
    }): Promise<boolean>
```

Removes the one series of exactly this name and these labels, no more
and no fewer, with every sample it holds, whether they still read or
not; true when the store held it. `labels: {}` is the series without labels.

### Metrics.damaged

```ts
damaged(): Promise<Damage[]>
```

The series whose bytes no longer read, 1000 at most, each with what
broke; deleting them shows the next.

    for (let damaged = await store.metrics.damaged(); damaged.length > 0; damaged = await store.metrics.damaged()) {
        for (const series of damaged) await store.metrics.delete(series)
    }

### Metrics.flush

```ts
flush(): Promise<void>
```

Sends what every instrument changed and has the store write its totals now, as it does every 15 seconds.

### Metrics.counter

```ts
counter<L extends string = string>(name: string, options?: InstrumentOptions): Counter<L>
```

A total that only goes up, which the store keeps: processes counting
into one series count into one total. Its label names, when given,
`counter<'route' | 'status'>(…)`, are the only ones `with` compiles with.

### Metrics.gauge

```ts
gauge<L extends string = string>(name: string, options?: InstrumentOptions): Gauge<L>
```

A value that goes up and down: a queue's length, a temperature.

### Metrics.gaugeFunc

```ts
gaugeFunc(name: string, read: () => number | Promise<number>, options?: GaugeFuncOptions): void
```

A gauge whose value read gives every 15 seconds, of the set of labels
given; another set of the name takes a function of its own, and a set
is read by one. It may be async, is never called again while a call of
it runs, and an error or an answer that is not a number writes nothing
that time, said once while it repeats.

    store.metrics.gaugeFunc('queue_depth', () => emails.depth(), { labels: { queue: 'emails' } })

### Metrics.timer

```ts
timer<L extends string = string>(name: string, options?: TimerOptions): Timer<L>
```

How many things took how long: their count, sum and longest in milliseconds, and with buckets their spread.

### Metrics.histogram

```ts
histogram<L extends string = string>(name: string, options?: HistogramOptions): Histogram<L>
```

A timer of numbers that are not durations: a response's size in bytes.

## LabelValue

```ts
type LabelValue = string | number | boolean
```

A label's value as an instrument takes it: a number or a boolean is kept as its text.

## InstrumentOptions

```ts
interface InstrumentOptions {
    /** the unit of its values, 32 bytes at most: 'bytes', '%' */
    unit?: string | undefined
    /** a line on what it means, 1024 bytes at most */
    help?: string | undefined
    /** how long its name's samples stay, as ingest takes it */
    keep?: Duration | undefined
    /** the sets of labels it holds at once, 1 to 2000, 2000 unless given; past them, its series of __overflow, said once */
    maxSeries?: number | undefined
}
```

What an instrument is told when it is made.

## Buckets

```ts
type Buckets = readonly number[] | 'exponential'
```

A timer's or a histogram's upper bounds, ascending, or the store's grid.

## TimerOptions

```ts
interface TimerOptions {
    help?: string | undefined
    keep?: Duration | undefined
    /** the sets of labels it holds at once, 1 to 2000, 2000 unless given; a timer's set is one, whatever it writes */
    maxSeries?: number | undefined
    /** the upper bounds of its spread, to which the store adds +Inf, or 'exponential' */
    buckets?: Buckets | undefined
}
```

## HistogramOptions

```ts
interface HistogramOptions extends InstrumentOptions {
    buckets?: Buckets | undefined
}
```

## GaugeFuncOptions

```ts
interface GaugeFuncOptions extends Omit<InstrumentOptions, 'maxSeries'> {
    /** the set of labels its function reads; another set of the name takes a function of its own */
    labels?: InstrumentLabels | undefined
}
```

## InstrumentLabels

```ts
type InstrumentLabels<L extends string = string> = {
    readonly [K in L]?: LabelValue
}
```

The labels `with` takes: any, or, when the instrument was made with their
names, `counter<'route' | 'status'>(…)`, those alone, checked as they are written.

## Instruments

```ts
class Instruments {}
```

The instruments of a store's handle, and what sends their changes. A store
has one; `store.metrics` makes its instruments through it.

### Instruments.declare

```ts
declare(name: string, form: Form, options: HistogramOptions, read?: boolean): Declared
```

An instrument of name, made the first time, read by functions when
`read` says so; another form or other options than it has are refused.

### Instruments.reader

```ts
reader(name: string, labels: Record<string, string>, options: InstrumentOptions, read: () => unknown): void
```

A gauge of name whose set of labels `read` gives every 15 seconds; each
set has a function of its own, made once while the store is open.

### Instruments.setOf

```ts
setOf(instrument: Declared, labels: Record<string, string>): LabelSet
```

The set of labels of an instrument, made the first time; past the sets it holds, its __overflow.

### Instruments.pending

```ts
pending(set: LabelSet): Change | undefined
```

What calls changed of a set since the last batch, if any.

### Instruments.change

```ts
change(set: LabelSet): Change
```

What a call changes of a set, kept for the next batch.

### Instruments.flush

```ts
flush(): Promise<boolean>
```

Sends what changed now and has the store write its totals; resolves
once it has, and says whether there was anything to send it with. One
the store cannot be reached for fails as a call does, and what it
carried goes once the store can be. What the store refused since the
flush before, and what a store that started again may have lost, fail
it, each once.

### Instruments.sent

```ts
sent(): Promise<void>
```

Sends what changed and waits for the store to apply it, without asking
it to write: the store writes its totals before a read that finds an
instrument that changed, so a read after this sees what the calls
counted. It waits for nothing when nothing is on its way, and fails
never, since the read meets what kept the batch from the store.

### Instruments.stop

```ts
stop(): Promise<void>
```

Stops the timers, sends what is left, and ends the stream; the store's close calls it.

## Counter

```ts
class Counter<L extends string = string> {}
```

A total that only goes up, which the store keeps: processes counting into one series count into one total.

### Counter.with

```ts
with(labels: InstrumentLabels<L>): Counter<L>
```

The same counter with more labels, a series of its own; the same labels in any order are one.

### Counter.inc

```ts
inc(): void
```

### Counter.add

```ts
add(n: number): void
```

Adds n, a finite number of 0 or more; add(0) makes the set of labels known with a total of zero.

## Gauge

```ts
class Gauge<L extends string = string> {}
```

A value that goes up and down, written while this program is connected.

### Gauge.with

```ts
with(labels: InstrumentLabels<L>): Gauge<L>
```

### Gauge.set

```ts
set(value: number): void
```

### Gauge.add

```ts
add(n: number): void
```

Moves it by n, from zero when it was never set.

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
class Timer<L extends string = string> {}
```

How many things took how long: their count, sum and longest in milliseconds, and their spread over its buckets.

### Timer.with

```ts
with(labels: InstrumentLabels<L>): Timer<L>
```

### Timer.record

```ts
record(duration: Duration): void
```

Records a duration measured elsewhere: milliseconds, as performance.now() counts them, or text such as '250ms'.

### Timer.measure

```ts
measure<R>(fn: () => R | Promise<R>): Promise<R>
```

Runs fn, awaits what it returns, returns or throws what it does, and
records how long it took either way.

    const user = await latency.with({ route: '/users' }).measure(() => users.get(id))

### Timer.start

```ts
start(): Timing
```

A timing that records when it stops, for a span that is no one function.

## Timing

```ts
class Timing {}
```

A span a timer measures from its start; stop records it, once.

### Timing.stop

```ts
stop(): number
```

Records how long since the timing started, the first time it is called; its milliseconds.

## Histogram

```ts
class Histogram<L extends string = string> {}
```

A timer of numbers that are not durations: a response's size in bytes.

### Histogram.with

```ts
with(labels: InstrumentLabels<L>): Histogram<L>
```

### Histogram.record

```ts
record(value: number): void
```

Records a value of 0 or more.

## exponentialBuckets

```ts
function exponentialBuckets(start: number, factor: number, count: number): number[]
```

Upper bounds from start, each factor times the one before: exponentialBuckets(1, 2, 14) is 1, 2, 4 … 8192.

<!-- Generated by task reference from sdk/js/src/metrics.ts, sdk/js/src/instruments.ts. Edit the doc comments there, not this file. -->
