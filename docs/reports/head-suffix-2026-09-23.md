# Long-head append without decoding the completed prefix

The existing packed-head encoder already copied completed 240-sample chunks,
but an ordered append still decoded the entire head before the merge. A
2,000-sample seed made this visible: on a one-sample append per series the
original branch `bfa7232` delivered 6,532–6,572 samples/s. Its CPU profile
placed 32.4% of sampled CPU under `decodeHead` and 18.0% under
`encodeHeadPrefix` (cumulative stacks, not additive cost shares).

The working-tree change keeps the same head format and one-row UPDATE. For a
packed head with at least 240 points, zero configured lateness, a strictly
new incoming timestamp and a retention cutoff before the head start, it checks
the whole-head CRC and all chunk metadata, reuses completed prefix bytes and
decodes only the last incomplete chunk. It merges that suffix with the incoming
points, encodes the suffix and renews the whole-head checksum. Other cases,
including late replacement, lateness and retention overlap, retain the full
decode path. `TestLongPackedHeadAppendKeepsExactBitsAndFrontier` reads all
2,001 values bit for bit before and after sealing and checks head metadata and
the unchanged prefix bytes.

## Measurement

Ryzen 7 7700, Windows 11, Docker Desktop Linux amd64, 16 visible CPUs,
Go 1.27.1, modernc SQLite 1.59.0, databases on named volume `tsperf`.
Source before: saved `bfa7232`. Source after: saved `4589144`, with the
benchmark harness recorded in `dd4a6e5`. `bench/perf/write_paths.go` is
byte-identical between the two saved revisions. Its append stage wrote
100,000 deterministic samples
across 1,000 series in batches of 100, with no maintenance during timing.
Each run used a fresh file. Order was before, after, after, before, then a
short-head control on both binaries.

| Seed points/series | Before samples/s | After samples/s | Before / after allocations per sample | Closed file |
|---:|---:|---:|---:|---:|
| 2,000 | 6,575; 6,500 | 10,461; 10,584 | 257.25–257.26 / 231.41–231.42 | 1,130,496 B on both sides |
| 1 | 13,116 | 13,170 | 213.71 / 213.71 | 413,696 B on both sides |

The long-head gain is 59–63% in this batch shape. The sampled WAL file-growth
counter was 18.13 B/sample on both long-head binaries and 26.41 B/sample in
the short-head control; it is file growth, not physical bytes written. The
new long-head CPU profile has no `decodeHead` path; `encodeHeadSuffix` remains
31.5% cumulative and `resolveSeries` 29.2%, while `writeHead` is 56.2%.
These profiles are one input shape, and cumulative percentages overlap.

## Decision

Do not add an append-only head table now. A suffix-only decode removes the
largest long-head cost without a format migration, another index, changed
payload addresses or a new failure boundary. Encoding the changing chunk
still costs CPU, so an append-only prototype remains a valid future experiment
if sustained long-head workloads dominate. It must beat this new baseline on
the same inputs and report WAL frames, CacheWrite, per-object `dbstat`, read
RSS and retention churn before it can justify a new schema.

## Reproduce

Build `bench/perf` from the saved revision and from the proposed tree using
the same Go 1.27.1 Linux environment. Then run sequentially:

```sh
<before-perf> -dir <volume>/before-a -stage append -series 1000 -samples 100 -batch 100 -seed-samples 2000
<after-perf>  -dir <volume>/after-a  -stage append -series 1000 -samples 100 -batch 100 -seed-samples 2000
<after-perf>  -dir <volume>/after-b  -stage append -series 1000 -samples 100 -batch 100 -seed-samples 2000
<before-perf> -dir <volume>/before-b -stage append -series 1000 -samples 100 -batch 100 -seed-samples 2000
```

Repeat once per binary with `-seed-samples 1` for the control. `-cpu-profile`
on either binary records the named stacks. Run `go test ./metrics -run
'^TestLongPackedHeadAppendKeepsExactBitsAndFrontier$' -count=1` for the exact
bit and sealed-frontier gate.
