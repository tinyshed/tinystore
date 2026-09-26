# A quiet stream's small segments, merged — 2026-09-26

Sealing a head once its oldest record is an hour old keeps a follower an hour
behind at most, and on the production corpus it left 4,620 segments of a
median of 38 records, which cost 2.38 bytes a record more than full segments
after [the stamps round](records-stamps-2026-09-26.md): each paid its own row,
keys, index entries and small blocks. The engine now merges them.

After a pass seals a stream, its segments holding fewer than a quarter of a
segment's records and input merge four at a time, four whose records share a
power of four, taken in time order so that one after another their records
stay in time order; the merged segment is one of the next size, and a record
is written again a few times rather than once a seal:

```text
places 1 4 7 9 of a stream, 30 40 35 38 records   → 1 holds 143; 1 4 7 9 start at 0 30 70 105
four such, 143 150 160 140 records                → one holds 593
```

A segment's id is its place in the order segments were sealed, which a
`Follow` cursor names, so a merge keeps every member's row: the lowest id
holds the merged records, and every other member keeps only which segment
holds its records and where they start among them. A cursor at a place, or
inside it, goes on where it was and finds each record once
(`TestAFollowerKeepsItsPlaceAcrossMerges`); a merge is one transaction
(`TestAFailedMergeLeavesTheSegmentsAsTheyWere`,
`TestReadersSeeEveryRecordOnceWhileMerging`), and retention and `Drop` remove
a holder with its places.

## Environment and reproduction

- The baseline at `abef94f`, the stamps engine with a runner that also asks
  the operator's queries of the replayed corpus and follows all of it; the
  candidate at `a7a6e4a`, which adds merging (`c71ea48`) and the runner's
  count of what merges write again.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The research's frontend fixture, and the private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, read as
  the engine round read it. Aggregates only.

Both revisions ran in one container, one after the other:

```sh
git -C <repo> archive abef94f | tar -x -C <base>
git -C <repo> archive a7a6e4a | tar -x -C <cand>
docker run --rm -v <base>:/src-base:ro -v <cand>:/src-cand:ro -v <module cache>:/go/pkg/mod:ro \
  -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' \
  -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand; do
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
    rm -rf /data/$rev-density /data/$rev-docker
  done
  for age in 1h 6h 24h; do for rev in base cand; do
    /tmp/records-$rev -stage replay -seal-age $age -corpus /corpus -dir /data/$rev-replay-$age
    rm -rf /data/$rev-replay-$age
  done; done'
```

`replay` runs the corpus on its own clock, each minute's records appended
together and `Maintain` every minute, so a head seals when full or `SealAge`
old and, in the candidate, a stream's small segments merge as it goes.

## Bytes a record in the file

| | `abef94f` | `a7a6e4a` | Written again |
|---|---:|---:|---:|
| sealed an hour old, the default | 18.9278 | 16.7724 | 1.53 times a record |
| sealed six hours old | 17.0440 | 16.6943 | 0.39 |
| sealed a day old | 16.6695 | 16.6215 | 0.04 |
| full segments | 16.5527 | 16.5558 | — |
| one million frontend records | 7.8899 | 7.8909 | — |

Sealed hourly, where merging has most to take, the file divides by `dbstat`:

| Object | `abef94f` | `a7a6e4a` |
|---|---:|---:|
| blocks | 17.9244 | 16.4188 |
| segments | 0.3559 | 0.1215 |
| block_filters | 0.2886 | 0.1369 |
| blocks_by_time | 0.1470 | 0.0549 |
| segment_keys | 0.1393 | 0.0155 |
| segments_by_end | 0.0588 | 0.0062 |
| segments_by_size, new | — | 0.0046 |

The 4,620 segments became 414 that hold records and 4,206 places, rows that
keep only where their records went. Merging takes back 2.16 of the 2.38 bytes
a record hourly sealing cost, and the default stays an hour. Where nothing
merges the file pays for the new columns and index: a page of the segments
table and one of `segments_by_size`, 0.0031 bytes a record on the corpus and
0.0010 on the frontend fixture.

## Time

Replaying the corpus sealed hourly took 20.9 s against 16.5, the merges
writing 2.03 million records again; six-hourly 13.9 against 12.1, daily 11.9
against 11.6.

## Reads and a follower

The operator's queries of the engine round, on the corpus sealed hourly,
single runs with a warm cache:

| Query | Blocks | Bytes | Time |
|---|---:|---:|---:|
| the busiest minute, `abef94f` | 24 | 259,705 | 7.1 ms |
| the same, `a7a6e4a` | 31 | 318,250 | 6.4 ms |
| one `requestId`, all time, `abef94f` | 3 | 23,662 | 38.6 ms |
| the same, `a7a6e4a` | 13 | 41,688 | 6.6 ms |
| `level = 50` inside JSON, all time, `abef94f` | 525 | 2,887,242 | 62.1 ms |
| the same, `a7a6e4a` | 210 | 2,709,270 | 20.6 ms |

A block of a merged quiet stream holds a thousand records over a day or more
where an hourly one held a few dozen, so a read fetches bigger blocks: the
request id reads 13 blocks where it read 3, in a sixth of the time, which
these runs do not divide. Full segments, the other end, fetch 36, 11 and 186
blocks.

A consumer following the whole corpus from its first place, a thousand
records a batch:

| | Blocks | Bytes | Time |
|---|---:|---:|---:|
| sealed hourly, `abef94f` | 6,454 | 34,834,897 | 2.03 s |
| sealed hourly, `a7a6e4a` | 7,376 | 81,156,908 | 3.04 s |
| sealed six-hourly, `abef94f` | 3,662 | 36,270,112 | 1.88 s |
| sealed six-hourly, `a7a6e4a` | 3,765 | 44,060,303 | 1.68 s |

A follower reads places in the order they were sealed, and the places of a
merged quiet stream lie hours apart in that order: each batch that reaches one
fetches the block holding it again, a block holds many of them, and a
follower that has fallen behind by the whole corpus fetches 2.3 times the
bytes. One that keeps up reads places before they merge.

## What this settles, and what it leaves

- Sealing hourly now costs the production corpus 16.77 bytes a record, 0.22
  above full segments, each record written again one and a half times.
- A follower far behind pays for merged places in bytes fetched; keeping a
  batch's blocks past the batch would take most of it back, and is not built.
  Follow-up: [the follow round](records-follow-2026-09-26.md) keeps them, and
  a follower of the whole corpus fetches 21.4 MB.
- Merged quiet streams make wide blocks: the widest spans about 337 hours,
  where the hourly file's spanned an hour. The time index scans every block ending after
  a read's start, which wide blocks make worth bounding.
