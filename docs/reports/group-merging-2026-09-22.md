# Incremental sealing approaches the grouped layout

The research grouped up to 32 microblocks. The engine could write that layout
in a bulk ingest, but frequent maintenance left one-block groups indefinitely.
This round adds bounded merging during publication, preserving existing payload
addresses, raw bits, summaries and the sealed frontier.

## Environment and fixture

Ryzen 7 7700, Windows 11, Docker Desktop, Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0, klauspost/compress 1.19.0. Database files are on the
`tsperf` named volume. WAL, `synchronous=FULL`, two readers and one writer.

Baseline engine: `44ee471`, already including prepared reads. Candidate: `93a1755`. Both use the same `bench/perf`
harness and deterministic input, sequentially in one session.

`steady` writes 128 series × 7,681 samples = **983,168 samples**, at fixed
ten-second timestamps starting at 1789500000000. Four equally sized classes:
constant, changes every 80 samples, small counter-like integer steps, and
incompressible floating mantissas. This is a synthetic workload through the
public engine, not a new measurement of TSBS or real telegraf density.

Each round writes 240 samples per series (241 initially), then drains bounded
maintenance. It therefore exposes incremental grouping rather than bulk packing.
Both files are closed, reopened and checked sample by sample, including float
bits. File sizes include free pages; no VACUUM is used.

## File accounting

| object | before bytes | after bytes |
|---|---:|---:|
| groups | 364,544 | **217,088** |
| payloads | 2,101,248 | **2,101,248** |
| series_state | 86,016 | 86,016 |
| series | 16,384 | 16,384 |
| identity index | 12,288 | 12,288 |
| postings | 16,384 | 16,384 |
| clocks | 4,096 | 4,096 |
| clock digest index | 4,096 | 4,096 |
| label_values | 4,096 | 4,096 |
| label dictionary index | 4,096 | 4,096 |
| head | 4,096 | 4,096 |
| series_due | 4,096 | 4,096 |
| series_ready | 4,096 | 4,096 |
| store_state | 4,096 | 4,096 |
| migration history | 4,096 | 4,096 |
| schema | 4,096 | 4,096 |
| free pages | 0 | 4,096 |
| **whole file** | **2,637,824** | **2,494,464** |
| **whole-file B/sample** | **2.682984** | **2.537170** |

Groups fall from 4,096 to 128 and clocks from 32 to one. The file shrinks
**5.43%**, entirely through metadata, with unchanged payload-table space.

An intermediate implementation copied payloads into new consecutive ids to
retain directory version 2. It reached only 2.653821 B/sample: the smaller
directories were offset by payload-page slack and 17 free pages. It was rejected.
Version 3 stores explicit payload ids; merging now replaces directories and
clock ownership without moving payload rows. Old versions remain readable.

## Read costs, including the trade

Warm, one reader, three seconds per shape, no profiling during measurement:

| query | qps before → after | p50 before → after | p99 before → after |
|---|---:|---:|---:|
| point, one sample | 17,811.7 → 15,161.4 | 41.1 → 44.9 µs | 341.1 → 388.1 µs |
| hour, 360 samples | 12,602.1 → 12,656.5 | 53.2 → 50.0 µs | 419.0 → 431.8 µs |
| full scan, 983,168 samples | **13.7 → 20.6** | **72.3 → 48.2 ms** | 77.2 → 57.4 ms |

Full-scan allocations fall from 328,761 to 133,451 per query, about **59%**;
peak heap is 39.2 versus 36.9 MiB. The point query pays for inspecting a larger
directory, and its throughput falls about **15%**. This is a measured trade,
not a universal speedup. Hour throughput is effectively unchanged.

The first merge attempt also expanded every regular clock merely to validate
it, even for blocks outside the requested range. Validation now checks regular
clock arithmetic without allocating timestamp arrays; irregular clocks still
receive their existing full validation. This removed most, but not all, of the
short-read regression.

No write-throughput improvement is established. The publication-only comparison,
before the final clock-validation change, was 64,162 versus 63,477 samples/s;
maintenance p50 was 215.8 versus 222.8 ms and peak WAL 4,157,112 versus 4,152,992
bytes. Final write throughput, long-running retention and irregular-clock point
reads were not remeasured. This round does not establish a new real-corpus floor.

## Bounds and correctness

Only preceding groups with no more live blocks than the current group are
absorbed. The combined group remains at most 32 blocks and within its clock-byte
limit; each absorbed group at least doubles its live-block count. No global scan
or raw re-encoding is needed. Previously expired slots are omitted. Version
checks precede the merge; directory replacement, clock references, new payloads,
head removal and frontier advancement commit together. Work counters count only
new blocks. Quiet historical groups are not rewritten without new sealing work.

Tests cover exact bits across 33 incremental rounds and reopen, shared clocks,
unchanged payload allocation, partial and complete expiry, publication rollback,
late-sample rejection and golden directory versions 1, 2 and 3.

Windows `task check` passed with Git's `usr/bin` on PATH for `wc`; the Linux/amd64
probe is 8,068 KiB, import increment 6,596 KiB. `go -C bench/perf test ./...`
passed. Linux container checks: `CGO_ENABLED=0 go build ./...`,
`go test -race -shuffle=on ./...`, and a five-second `FuzzNewFormats` run
(62,357 executions) passed. macOS was not tested locally.

## Reproduce

Use a new directory for each population. In a `golang:1.27` container with
`<repo>` at `/src`, `tsperf` at `/perf`, `GOWORK=off`, and the shared Go cache:

```sh
mkdir -p /perf/merge-reference
git -C /src -c safe.directory=/src archive 44ee471 | tar -x -C /perf/merge-reference
cp /src/bench/perf/main.go /src/bench/perf/steady.go /perf/merge-reference/bench/perf/
cd /perf/merge-reference/bench/perf
go build -o /perf/merge-before .
cd /src/bench/perf
go build -o /perf/merge-after .
round=$(mktemp -d /perf/group-round-XXXXXX)
for v in before after; do
  /perf/merge-$v -dir "$round/$v" -stage steady -label "$v" -series 128 -samples 7681
done
for shape in point hour scan_all; do
  for v in before after; do
    /perf/merge-$v -dir "$round/$v" -stage read -label "$v" \
      -series 128 -shape "$shape" -readers 1 -seconds 3
  done
done
```

The fixed fixture must remain inside the harness's configured retention window.
