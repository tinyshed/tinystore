# The time index walked within each width of block — 2026-09-26

A read picks its candidate blocks from an index that led with where a block
ends: every block ending after the read's start is a candidate until its
beginning says otherwise, so a read near the start of a large store walked
nearly every entry of the index, and the wide blocks of late records and of
merged quiet streams gave no bound to cut the walk short. The index now leads
with a block's span, the power of two its width in nanoseconds stays under,
and a read asks it once for each span the file holds, for the blocks ending
between its start and its end moved on by that width:

```text
span 29, blocks under half a second: a read of [12:00, 12:01) walks last_at in [12:00, 12:01 + 0.54 s)
span 51, blocks under 26 days:       the same read walks last_at in [12:00, 12:01 + 26 days)
```

`TestTheTimeIndexIsWalkedWithinEachSpan` checks the plan SQLite chooses, and
`TestReadsFindRecordsInBlocksOfEveryWidth` a hundred reads of random ranges
over blocks of a busy stream, of a quiet stream merged over hours and of late
records, against the records themselves; with each span's end cut by half,
it fails. The spans the file holds are kept in memory, loaded on `Open`, and a
span enters them before the transaction writing its block commits, so a read
that loads them once its snapshot has begun knows every span it can meet.

## Environment and reproduction

- The baseline at `f11c1da`, whose runner adds the `reach` stage below; the
  candidate at `34be495`, which changes `records/` alone.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The research's frontend fixture, and the private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, read as
  the engine round read it. Aggregates only.

```sh
git -C <repo> archive f11c1da | tar -x -C <base>
git -C <repo> archive 34be495 | tar -x -C <cand>
docker run --rm -v <base>:/src-base:ro -v <cand>:/src-cand:ro -v <module cache>:/go/pkg/mod:ro \
  -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' \
  -e GOPROXY=off --tmpfs /data:rw,size=8g golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand; do
    /tmp/records-$rev -stage reach -records 10000000 -dir /data/$rev-reach
    rm -rf /data/$rev-reach
  done
  for rev in base cand; do
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage late -batch 1024 -dir /data/$rev-late
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
    /tmp/records-$rev -stage replay -seal-age 1h -corpus /corpus -dir /data/$rev-replay
    rm -rf /data/$rev-density /data/$rev-late /data/$rev-docker /data/$rev-replay
  done'
```

`reach` writes ten million frontend records a flush at a time, 9,766 blocks,
then reads one second a hundred times in the first hundredth of the store's
time and a hundred times in its last.

## Reads

| | Blocks a read | `f11c1da` | `34be495` |
|---|---:|---:|---:|
| one second in the first hundredth of ten million records | 1.85 | 2.163 ms | 1.309 ms |
| one second in the last hundredth | 1.86 | 1.345 ms | 1.312 ms |
| one second of one million records, the density stage | 1.92 | 1.239 ms | 1.222 ms |
| the same, one record in a hundred up to ten minutes late | 5.24 | 2.463 ms | 2.488 ms |

A read at the start of ten million records cost 0.82 ms more than one at the
end, walking the index entries of the blocks after it; now both cost the
same. The blocks a read fetches do not change, nor do the operator's queries
on the corpus, in full segments and sealed hourly with its merged blocks
spanning up to 337 hours: the same blocks and bytes, their times within a
single run's spread.

## The file

| | `f11c1da` | `34be495` |
|---|---:|---:|
| ten million frontend records | 7.8677 | 7.8704 |
| one million frontend records | 7.8909 | 7.8930 |
| the production corpus, full segments | 16.5558 | 16.5604 |
| the same, sealed hourly | 16.7724 | 16.7794 |

A block's span is a byte in its row and in its index entry: on the ten
million records `blocks_by_time` goes from 0.0357 to 0.0384 bytes a record and
the blocks take the same pages; on the corpus the index goes from 0.0472 to
0.0503 and the blocks from 16.3376 to 16.3391.

## What this settles, and what it leaves

- A read walks the index near its range, whatever the widths of the blocks
  and wherever in the store's time it falls; what the old index walked grew
  with the blocks after a read's start, 0.82 ms at ten million records.
- The late fixture's one-second reads still fetch 5.24 blocks: the span bounds
  what a read walks, not what it must fetch.
