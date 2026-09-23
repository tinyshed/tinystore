# Exact aggregate contract

`Aggregate(ctx, AggregateRequest)` implements the numerical rules below by
decoding raw samples from one snapshot. `AggregateRequest` contains a `Range`,
a positive whole-millisecond `Width`, and an operation: `count`, `sum`, `min`,
`max` or `increase`. The buckets are anchored at the requested `Range.From`;
retention clips contributing samples without shifting those bucket boundaries.
`AggregateResult` holds one series and its nonempty buckets. Each bucket reports
its half-open bounds, count, value, reset count and overflow flag.

Existing directory `sum` and `increase` fields are ordinary float64 diagnostics
and cannot answer the sums specified here. The current aggregate path decodes
raw blocks, including complete ones. A versioned exact summary shortcut remains
unbuilt.

## Range and result

An aggregate query clips its requested half-open range `[from,to)` to the
retention cutoff captured for that query. Each output bucket is also half-open.
Only observed samples in the clipped intersection contribute. Empty buckets have
no value. There is no interpolation, rate extrapolation, downsampling, or change
of bucket boundaries. A query that fails returns no partial result.

Each series is aggregated independently in timestamp order. A whole-block
summary may contribute only when every sample in that block belongs to both the
clipped range and one output bucket. A block cut by a range, retention cutoff,
or bucket boundary must be decoded and filtered first. Current float64 block
summaries are ineligible for exact `sum` and `increase`.

## Gauge arithmetic

`count` is the number of observed samples, with integer overflow reported as a
resource error. For finite values, `sum` is the correctly rounded float64 value
of their exact mathematical sum, rounded once using nearest-even. It is
independent of block boundaries, input grouping and merge order. A mathematical
zero returns `+0`. If nearest-even rounding overflows float64, the result is
the appropriate infinity with a distinct overflow indication. `min` orders
finite values numerically, taking `-0` before `+0`; `max` takes `+0` after
`-0`. Other ties do not change the value's bits.

NaN and infinities remain legal raw samples. An aggregate bucket containing
one reports `ErrNonFinite` for the whole gauge query; it does not drop
the sample, choose a NaN payload, or return a partial answer. No aggregate may
quietly reinterpret an IEEE value as missing data.

## Counter arithmetic

A counter aggregate requires finite, nonnegative observations. Both signs of
zero represent numerical zero. The first observation in a bucket contributes
zero increase. For each following observation `current`, with `previous` the
immediately preceding observation in that bucket:

```text
current >= previous: add current - previous
current <  previous: add current; count one reset
```

Each subtraction and addition is exact mathematical arithmetic, then the
bucket's final increase is rounded once to float64 using nearest-even. If the
exact result overflows float64, return `+Inf` with an overflow indication.
A nonfinite or negative counter observation makes the query fail with
`ErrCounterValue`. Equal samples do not reset. There is no transition from a sample
outside the bucket or clipped range, and no extrapolation to either edge.

The transition between adjacent stored blocks is part of the answer. When
complete blocks contribute their internal increases, their exact `first` and
`last` values supply the transition from the previous included block or edge
sample. A reset across that boundary adds the new block's first value and one
reset. A partial block is decoded before its transitions are counted.

## Summary representation gate

For finite float64 values, an exact sum is an integer count of units of
`2^-1074`. A block of at most 240 samples can store that signed integer after
removing trailing zero bits, with the removed exponent encoded separately.
The representation is bounded by the float64 domain and block sample ceiling;
the decoder must check sign, exponent, magnitude length and canonical form.
The current directory format does not contain it. Introducing it requires a
new directory version and a golden reader for older versions. Legacy blocks
decode raw for exact aggregates now.

The [representation round](reports/aggregate-representation-2026-09-23.md)
compares this candidate with a fixed superaccumulator and floating expansion.
The current API has tests for cancellation after intermediate overflow,
subnormals, signed zero, nonfinite inputs, reset transitions, clipped blocks,
retention and reopen. A new persisted summary version still needs golden old
and new readers, corruption tests and identical aggregate answers before its
shortcut may ship.
