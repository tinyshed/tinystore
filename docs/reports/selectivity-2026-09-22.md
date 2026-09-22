# Closing the two defects the performance round named

Follow-up: [prepared reads](prepared-reads-2026-09-22.md) profiles the remaining
cost and compares both revisions in one session. It supersedes the unprofiled
diagnosis below and corrects the attribution of broad-query overhead to matcher
count; the historical measurements below are unchanged.

[The performance round](performance-2026-09-22.md) named two findings as defects
rather than characteristics: a match driven by the alphabetically first matcher,
and a read concurrency ceiling that was a constant. Both are fixed here and
measured on the same harness. One of them moved a great deal, the other moved
less than its table suggested it would, and the fix costs a fixed amount on
every query — all three are below.

## Environment

Ryzen 7 7700, Docker Desktop on Windows 11, `golang:1.27` linux/amd64, modernc
SQLite 1.59.0, klauspost/compress 1.19.0, the database on a Docker named volume.
The engine is the working tree that became `a2f376a` and `43c6f30`. Same
`bench/perf` harness and the same phases, one stage at a time.

**This is not a same-run comparison.** The baseline column is the earlier
round's published table, measured in a separate session on the same machine.
Differences under about ten percent are therefore not evidence on their own, and
the fixed per-query cost named below is stated with that in mind.

The read stages now open the store with `MaxReaders` equal to the reader count
they are about to apply; the baseline always had a pool of two.

## The matcher fix worked, and the curve is flat

The same point read, returning one sample, against three cardinalities:

| series in the file | p50 before | p50 after | p99 before | p99 after |
|---:|---:|---:|---:|---:|
| 1 000 | 214 µs | 241 µs | 619 | 716 |
| 10 000 | 424 µs | **269 µs** | 820 | 710 |
| 100 000 | **2 985 µs** | **277 µs** | 3 781 | 668 |

At a hundred thousand series the same one-sample answer is **10.8 times faster**.
More important than the ratio is the shape: 241, 269, 277 across a hundredfold
increase in cardinality, where the old numbers were 214, 424, 2 985. A point
read no longer gets slower as the file grows, which is what driving from the
shortest posting list was supposed to buy.

## What it costs, on every query

At a thousand series the same read is **slower**: 214 µs becomes 241 µs. That is
the ranking itself — two extra index lookups before the match, one per matcher.
The cost shows up as a constant rather than a ratio, and the whole shape table
at a thousand series agrees:

| shape | p50 before | p50 after | difference |
|---|---:|---:|---:|
| point | 214 µs | 241 µs | +27 µs |
| one hour | 238 µs | 274 µs | +36 µs |
| one day | 332 µs | 367 µs | +35 µs |
| one series, whole range | 333 µs | 372 µs | +39 µs |
| a host, one hour (20 series) | 1.85 ms | 1.95 ms | +97 µs |
| a region, one hour (250 series) | 21.6 ms | 22.4 ms | +794 µs |
| every series, whole range | 183 ms | 185 ms | +2 ms |

Two matchers cost about 30 µs to rank; the selector shapes carry more matchers
and pay proportionally more. So the trade is a fixed tens-of-microseconds charge
against a cost that grew with cardinality — 30 µs paid at a thousand series to
save 2.7 ms at a hundred thousand. For a store whose stated target is a million
series that is the right side of the trade, but it is a trade and not a free win.

The probe is bounded at 1024 rows a matcher, because the matchers are being
ranked and not counted; the exact length of a long posting list is precisely the
work being avoided.

## The reader ceiling is gone; a real one is behind it

10 000 series, one-hour windows, the reader count swept:

| readers | qps before | qps after | p99 before | p99 after |
|---:|---:|---:|---:|---:|
| 1 | 2 129 | 3 056 | 874 µs | 766 |
| 2 | 3 544 | 5 293 | 1 088 | 995 |
| 4 | **3 496** | 5 816 | 2 678 | 1 387 |
| 8 | **3 488** | 5 874 | 8 023 | 2 490 |

The old row is flat after two readers — 3 544, 3 496, 3 488 is no gain at all,
which is what a pool of two looks like. The new row keeps climbing, but barely:
**+10% from two readers to four, +1% from four to eight.** The constant was the
ceiling; it is not the only one. Something else saturates at about four, and
this run does not say what — one SQLite file, one page cache and one machine are
all candidates, and none was isolated.

Latency at eight readers improved anyway, from 8 023 µs to 2 490 µs at p99,
because the queue is no longer eight deep on two connections.

The heavy query scales better than the light one:

| readers | qps before | qps after | peak heap after |
|---:|---:|---:|---:|
| 1 | 5.2 | 4.9 | 43.7 MiB |
| 2 | 9.2 | 8.1 | 84.1 MiB |
| 4 | 11.4 | 13.5 | 164.3 MiB |
| 8 | **11.4** | **17.3** | **244.9 MiB** |

Where the baseline stopped at four readers, this one is still climbing at eight,
for **52% more throughput**. The heap column is the price and it is not small:
eight concurrent queries each returning nine hundred thousand samples hold a
quarter of a gigabyte in flight. Widening the pool widens that too, which is why
the default stays at two and the width is the caller's decision.

## What this does not establish

One machine, one storage stack, warm cache, and a baseline from a different
session rather than the same run. The four-reader saturation is named but not
diagnosed. No mixed writer-and-reader sweep was repeated, so what a wider pool
does to the write-ahead log under this engine is still only the earlier round's
measurement — and that round showed the log growing from 13 MB to 49 MB as
readers went from one to four, which a larger `MaxReaders` can only make worse.

## Reproduce

```powershell
docker run --rm -v <repo>:/src -v tsperf:/perf `
  -v tinystore-gocache:/go -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache `
  -w /src/bench/perf golang:1.27 sh -c 'go build -o /perf/perf . && sh run.sh populate'
```

then `read_shapes`, `read_scale` and `read_readers` in place of `populate`, one
phase at a time.
