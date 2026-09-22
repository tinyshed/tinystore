# What the engine costs in time, not in bytes

Interpretation correction: [the engine audit](engine-audit-2026-09-22.md)
shows that the harness's WAL counter measures observed file growth, not bytes
written, and Go Sys is not process RSS, including with modernc. The high-cardinality
ingest fixture also registers new series rather than repeatedly updating their
heads. The historical observations below remain, but the write-amplification,
process-memory and causal explanations must be read with those corrections.

Density stopped being the suspicious number: on real telemetry the whole file is
about 1.1 bytes a sample, mutable state, indexes and SQLite included. This is the
first measurement of the other axis — how fast the thing chews, what it allocates,
what it writes to the log, and how it behaves when readers and a writer share it.

Two findings here are defects rather than characteristics, and both are named.
Both are fixed and re-measured in [the selectivity round](selectivity-2026-09-22.md);
the two tables below are the state before that fix.

## Environment

Ryzen 7 7700, Docker Desktop on Windows 11, `golang:1.27` linux/amd64 with 16
CPUs and 15.5 GiB visible, modernc SQLite 1.59.0, the engine at the working tree
that carries the dictionary-id registry. The database lives on a Docker named
volume, not on a Windows bind mount, because a bind mount would measure Docker
Desktop's filesystem. WAL, `synchronous=FULL`, one writer connection, two reader
connections — the engine's own settings, untouched.

**The timed stages ran one at a time.** Only the three databases the read stages
share were filled in parallel, and their own elapsed times are therefore not
results. Percentiles are over every call in a stage.

Windows was rejected as the environment for this: its monotonic clock resolves to
15.7 µs, but scheduler wake-ups quantise to about a millisecond, and every p95 in
a trial run landed on 1003 µs. That is the operating system, not the engine.

`bench/perf` is the harness; `bench/perf/run.sh` names the phases.

## One Ingest is one transaction is one fsync

1000 series, `Maintain` every sixteen batches:

| samples in one call | total samples | samples a second | p50 | p95 | p99 | allocs a sample | WAL bytes a sample |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 5 000 | **336** | 3229 µs | 3558 | 4838 | 259.25 | **834.72** |
| 100 | 200 000 | **31 232** | 3342 µs | 3704 | 6850 | 3.28 | 20.87 |
| 10 000 | 2 000 000 | **298 701** | 7208 µs | 9600 | 11 423 | 1.24 | **2.11** |

The middle column is the whole story: **a call carrying one sample and a call
carrying a hundred cost the same 3.3 ms.** That is one commit and one fsync. A
call carrying ten thousand costs 7.2 ms — twice the latency for ten thousand
times the work.

So the engine's write side is not sample-bound, it is transaction-bound, and the
number a caller has to care about is how many samples it hands over per call.
Between one and ten thousand there is a factor of **889 in throughput and 396 in
write-ahead log bytes**. A caller appending one sample at a time writes 835 bytes
of log for eight bytes of value.

This is not a bug — it is what a durable single-writer file costs — but it is the
first thing to say in the API's documentation, and it says the batch is the unit
this engine is built around.

## Cardinality on the write path

Batch of 10 000, two million samples each time:

| series | samples a second | p50 | p99 | allocs a sample | peak heap |
|---:|---:|---:|---:|---:|---:|
| 1 000 | 303 372 | 7 078 µs | 11 999 | 1.24 | 9.7 MiB |
| 10 000 | **637 118** | 14 938 µs | 21 322 | 1.81 | 12.5 MiB |
| 100 000 | 99 611 | **99 280 µs** | 115 707 | 16.77 | 67.3 MiB |

Ten thousand series is *faster* than one thousand, which looks wrong until the
sealed-block column is read: at 1000 series each series accumulated 2000 samples,
crossed the block threshold and sealed 8000 blocks; at 10 000 series each holds
200 samples and never sealed anything. The comparison is between an engine doing
its compaction work and an engine not doing it yet, not between two cardinalities.

At a hundred thousand series one call of ten thousand samples touches five
hundred series, so it rewrites five hundred packed tails inside one transaction:
99 ms for the call, 16.8 allocations a sample, 67 MiB of heap. Registration is
not the cost — the same run registered all 100 000 series — the cost is that a
tail is rewritten whole.

`Maintain` is bounded but chunky: **324 to 332 ms** when it has 64 series' worth
of full heads to seal, 110 to 140 µs when there is nothing due. Moving it out of
the ingest loop entirely changed the total by 1%, so where it is called does not
matter; that it takes a third of a second does.

## Reading

1000 series of 2000 samples, one reader:

| shape | queries a second | p50 | p95 | p99 | samples a query |
|---|---:|---:|---:|---:|---:|
| point | 4 145 | **214 µs** | 410 | 619 | 1 |
| one hour | 3 705 | 238 µs | 478 | 703 | 360 |
| one day | 2 587 | 332 µs | 695 | 865 | 2 000 |
| one series, whole range | 2 579 | 333 µs | 698 | 865 | 2 000 |
| a host, one hour (20 series) | 524 | 1.85 ms | 2.50 | 2.78 | 7 200 |
| a region, one hour (250 series) | 45 | 21.6 ms | 25.6 | 27.6 | 90 000 |
| every series, whole range | 5.5 | 183 ms | 194 | 195 | 2 000 000 |

A full scan moves **10.9 million samples a second** and holds 81 MiB of heap for
the two million it returns — about 40 bytes a sample in flight, which is the
output slice rather than anything decoded twice.

The floor for a query that returns one point is 214 µs: a read transaction, a
postings match, the group directories, the head, a decode.

## The first defect: the least selective matcher drives the query

The same point read, returning one sample, against three cardinalities:

| series in the file | p50 | p95 | p99 |
|---:|---:|---:|---:|
| 1 000 | 214 µs | 410 | 619 |
| 10 000 | 424 µs | 597 | 820 |
| 100 000 | **2 985 µs** | 3 538 | 3 781 |

Fourteen times slower for the same one-sample answer. The reason is in
`postingsFilter`: matchers arrive canonically ordered, which means **ordered by
label name**, and the first one becomes the driving posting list:

```sql
select p.series_id from postings p
where p.label_id = (__name__ = metric_0)          -- 5000 rows at 100k series
  and exists (select 1 from postings q
              where q.label_id = (host = host_0)  -- would have been 20 rows
                and q.series_id = p.series_id)
```

`__name__` sorts before `host`, so the query drives from the matcher that names
five thousand series and filters it down to one. The scanned rows go 50, 500,
5000 across the three files — a ratio of 1 : 10 : 100 — and the measured times
above, once the fixed 180 µs of transaction and decode is taken off, go
1 : 7 : 82. The arithmetic is the diagnosis.

The fix is to drive from the shortest posting list rather than from the
alphabetically first, or to write the filter as an intersection the planner may
reorder. The query plan itself was not dumped; that is the next step and it is
five minutes of work.

## The second defect: read concurrency stops at two

10 000 series, one-hour windows, the reader count swept:

| readers | queries a second | p50 | p95 | p99 |
|---:|---:|---:|---:|---:|
| 1 | 2 129 | 440 µs | 664 | 874 |
| 2 | **3 544** | 531 µs | 872 | 1 088 |
| 4 | 3 496 | 1 042 µs | 2 033 | 2 678 |
| 8 | 3 488 | 1 825 µs | 5 440 | 8 023 |

**Throughput stops improving after two readers and latency then grows linearly
with the queue.** Eight readers do the same work as two and each waits four times
as long. That is not contention inside SQLite; it is
`f.reader.SetMaxOpenConns(2)` in `internal/sqlite`, a constant with no option
behind it. Two is a reasonable default for a panel; it is not a reasonable
ceiling for a process that wants to answer eight dashboards.

The same sweep on a query that returns ninety thousand samples saturates for a
different and legitimate reason — the work itself — and the numbers show it:
5.2, 9.2, 11.4, 11.4 queries a second, with p99 going 202 ms to 1.75 s.

## A writer and readers together

1000 series, fifteen seconds, 256 series appended per round, `Maintain` every
sixteen rounds:

| readers | samples ingested | queries | ingest p50 | read p50 | read p99 | WAL peak |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 84 992 | 4 470 | 41.6 ms | 3.34 ms | 4.59 | 13.3 MB |
| 2 | 68 096 | 7 592 | 51.1 ms | 3.95 ms | 5.36 | 21.5 MB |
| 4 | 59 392 | 7 936 | 53.7 ms | 7.16 ms | 16.7 | **49.2 MB** |
| 8 | 50 944 | 8 176 | 52.9 ms | 11.7 ms | 50.5 | 44.0 MB |

Two things are visible. Ingest loses 40% of its throughput to eight readers,
because a checkpoint cannot advance past the oldest open snapshot and the writer
ends up appending to a longer log. And the log itself grows with the reader
count — **13 MB with one reader, 49 MB with four** — which is the behaviour the
design document predicted from a synthetic harness, now measured on the engine.

The read deadline that bounds this is `SnapshotTimeout`, five seconds by default,
and no query here came close to it.

## What a series costs when it is short

From the same runs, bytes a sample in a closed file:

| shape | file | bytes a sample |
|---|---:|---:|
| 1 000 series × 2 000 samples | 2 551 808 | **1.2759** |
| 10 000 × 500 | 8 929 280 | 1.7859 |
| 100 000 × 50 | 33 996 800 | **6.7994** |
| 1 000 × 5, one sample a call | 376 832 | 75.3664 |

Nothing about the codec changes across those rows. What changes is how many
samples the per-series structures are divided by, which is the same arithmetic
the real-corpus round ran into and the reason a 25-minute capture flatters
nobody.

## What this does not establish

One machine, one storage stack, one process. No cold-cache reads: every stage ran
against a file the page cache had just written, so these are warm numbers. No
crash or power-loss behaviour, no long-running store, no retention pass under
load, no comparison with another engine. RSS is Go's own accounting, which is
honest here only because modernc SQLite is pure Go and its page cache is inside
that accounting; a cgo driver would need a different instrument.

The reader-pool ceiling and the matcher-ordering defect are both from this run
alone and both have an obvious fix; neither fix is made here.

## Reproduce

```powershell
docker run --rm -v <repo>:/src -v tsperf:/perf `
  -v tinystore-gocache:/go -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache `
  -w /src/bench/perf golang:1.27 sh -c 'go build -o /perf/perf . && sh run.sh populate'
```

then `ingest`, `read_shapes`, `read_scale`, `read_readers` and `mixed` in place of
`populate`, one phase at a time. `bench/perf` is its own module and adds nothing
to the engine's dependency list.
