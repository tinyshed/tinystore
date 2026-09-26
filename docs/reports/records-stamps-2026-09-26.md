# The times a line spells, kept behind its record's time — 2026-09-26

Follow-up: [the merge round](records-merge-2026-09-26.md) merges a quiet
stream's small segments and takes the corpus sealed hourly from 18.9278 to
16.7724 bytes a record.

Most lines a program writes begin with a time of their own, and a store that
receives them keeps its own time beside it. Under zstd, a block of such lines
pays for every digit of the fraction that changes from one line to the next.
The engine now takes a time a text spells out of it and keeps it as its
distance behind its record's time, in the unit its fraction counts, with the
layout that spells it back; the text keeps the rest, byte for byte. Integers
that count time, as a JSON line's own `time` does, are kept the same way.

```text
record    2026-09-23 00:47:32.101187376
text      I20260923 00:47:32.100929   141 raft_server.h:60] Peer refresh succeeded!
stamp     1 byte in, "YMD h:m:s.6", 258 µs behind
rest      I   141 raft_server.h:60] Peer refresh succeeded!
```

The example is `TestAStampIsKeptBehindItsRecordsTime`. A layout is found among
eight patterns, each with or without a fraction of one to nine digits: ISO and
RFC 3339, Go's log, glog with and without its year, Redis, syslog and access
logs; up to four times a value, each within 64 bytes of the last. What a
calendar or a clock does not show, a 30 February or a leap second, stays text,
and so does a column where fewer than one value in eight spells a time.
`FuzzStamps` checks that every time found is spelled back as it was, and
`TestStampsWithoutTheirTimesAreRefused` that a column kept against record
times cannot be read without them.

## Environment and reproduction

- The baseline at `f31bf64`, the candidate at `ac2da04`, which changes
  `records/` alone; the runner is `bench/records` at each.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The research's fixtures as the engine round used them, and the private
  production snapshot `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`,
  read as that round read it. Aggregates only.

Both revisions ran in one container, one after the other:

```sh
git -C <repo> archive f31bf64 | tar -x -C <base>
git -C <repo> archive ac2da04 | tar -x -C <cand>
docker run --rm -v <base>:/src-base:ro -v <cand>:/src-cand:ro -v <module cache>:/go/pkg/mod:ro \
  -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' \
  -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand; do
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage late -batch 16384 -dir /data/$rev-late
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
    rm -rf /data/$rev-density /data/$rev-late /data/$rev-docker
  done
  for age in 1h 6h 24h; do for rev in base cand; do
    /tmp/records-$rev -stage replay -seal-age $age -corpus /corpus -dir /data/$rev-replay-$age
    rm -rf /data/$rev-replay-$age
  done; done
  for rev in base cand; do (cd /src-$rev && go test ./records -run "^$" -bench "^BenchmarkSegment$" \
    -benchtime=20x -benchmem -cpu=1 -count=6); done'
```

## Bytes a record in the file

| | `f31bf64` | `ac2da04` |
|---|---:|---:|
| one million frontend records | 7.8899 | 7.8899 |
| the same, one in a hundred up to ten minutes late | 7.9217 | 7.9217 |
| the production corpus, full segments | 20.7738 | 16.5527 |
| its text containers' payload | 20.0204 | 15.4765 |
| its JSON containers' payload | 21.8412 | 20.7027 |
| its `blocks` b-tree, by `dbstat` | 20.5587 | 16.3376 |
| sealed an hour old, on the corpus's own clock | 22.8333 | 18.9278 |
| sealed six hours old | 21.2133 | 17.0440 |
| sealed a day old | 20.8736 | 16.6695 |

Every other object of each file takes the same pages: in full segments the
change took 4.22 bytes a record from the blocks' columns and gave them to
nothing else, and sealed hourly 3.91. The text containers pay 4.54 bytes a
record less and the JSON containers 1.14. The frontend fixture has no text
and no integer that counts time, and does not move.

The prototype kept a 64 KiB sample of each text-heavy segment's bodies, worth
0.80 bytes a record here, and paid 20.09 for the corpus; the engine now pays
3.53 less without it. What the sample would add beside the stamps is not
measured.

## Queries

The engine round's queries on the corpus, single runs with a warm cache:

| Query | Blocks | Bytes at `f31bf64` | Bytes at `ac2da04` |
|---|---:|---:|---:|
| the busiest minute, every container | 36 | 455,804 | 392,104 |
| one `requestId`, all time | 11 | 42,483 | 39,866 |
| `level = 50` inside JSON, all time | 186 | 2,872,024 | 2,726,342 |

One-second reads of the frontend fixture fetch 1.92 blocks, and 4.63 with its
late records, in both.

## Speed

`BenchmarkSegment`, one CPU, six runs of twenty segments of 16,384 records,
records a second, the median of six and their range, and bytes allocated a
record encoded:

| | `f31bf64` | `ac2da04` |
|---|---:|---:|
| frontend encode | 538,540 (491,015 to 560,125), 663 B | 551,788 (495,450 to 592,767), 533 B |
| frontend decode | 1,289,258 (1,164,752 to 1,315,695) | 1,332,641 (1,252,571 to 1,418,010) |
| backend encode | 745,509 (712,003 to 780,833), 1,046 B | 798,626 (786,194 to 808,176), 909 B |
| backend decode | 1,298,066 (1,238,134 to 1,341,893) | 1,334,896 (1,247,336 to 1,410,921) |
| text encode | — | 844,528 (800,103 to 863,998), 700 B |
| text decode | — | 1,806,328 (1,733,858 to 1,886,753) |

The encoders allocate less because an attribute column is now allocated once
at the size its rows give it, where it used to grow; a block's time column is
decoded once, where a read decoded it twice. The `text` fixture, new at
`ac2da04`, is lines in each of the layouts and a JSON service's lines every
ninth record. On the corpus, appending and sealing every container a full
segment at a time took 12.115 s against 10.726, 109,247 records a second
against 123,394: looking for stamps costs about 1.2 µs a text record of these
lines.

## What this settles, and what it leaves

- A line's own time is kept apart, and the production corpus pays 16.55 bytes
  a record in full segments and 18.93 sealed hourly. The one-second reads,
  the request id and the frontend fixture are where they were.
- Text's next lever is the rest of a template: numbers typed where they stand,
  which a time is only the first of. Follow-up: [the text round](records-text-2026-09-26.md)
  measured them, and they cost more than zstd. A column where fewer than one value in
  eight spells a time stays text, as does a time in a layout none of the
  patterns names.
