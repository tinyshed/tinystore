# A follower far behind, and a merged block fetched once — 2026-09-26

[The merge round](records-merge-2026-09-26.md) left a follower far behind
paying for merged places: the places of a quiet stream lie hours apart in the
order segments were sealed, each batch that reaches one fetched the holder's
block again, and following the whole corpus sealed hourly fetched 81 MB where
the unmerged file had fetched 35. Three changes take that back.

- **A block's id is never given twice.** The next id came from the largest
  id left, so a merge that deleted the newest blocks gave their ids to the
  blocks it wrote. `blocks` is now `autoincrement` and the next id comes from
  its sequence (`TestABlockIDIsNeverGivenTwice`), so a block's bytes are known
  by its id, and a holder's row, which changes only with its first block, by
  its id and that block's.
- **Follow keeps the last 4 MiB of the blocks and segment rows it fetched**
  for the batches after it, the recent half filling first and a full recent
  half becoming the older one, whose entries move back when used. Every Follow
  reserves those 4 MiB beside its own weight in the store's memory. A byte the
  cache had is still spent against the batch's budget, so a batch ends where
  it did; `Stats.ReadBlocks` and `ReadBytes` count what was read from the file
  (`TestAFollowerFetchesAMergedBlockOnce`: a follower in batches of seven
  fetches each of two merged blocks once, where it fetched 38).
- **A place builds only its rows of the holder's block**, a few dozen of a
  thousand, rather than all of them.

Separately, a merged segment's places are found through an index on their
holder (`TestPlacesAreFoundThroughTheirHolder`, on the plan SQLite chooses),
where a merge, retention and `Drop` walked every segment sealed after the
holder.

## Environment and reproduction

- The baseline at `429d99a`, the engine of the load round with names made
  plain; the cache at `8d061f0`, which also carries the holder index of
  `b17b31a`; and the rows a place needs at `865d580`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. The host ran other tests during the last two rounds,
  which the spread of the first two rows shows. `modernc.org/sqlite` v1.59.0,
  `klauspost/compress` v1.19.0, 1 KiB pages.
- The private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, read as
  the engine round read it. Aggregates only.

The three revisions ran in one container, in turn, four rounds:

```sh
git -C <repo> archive 429d99a | tar -x -C <base>
git -C <repo> archive 8d061f0 | tar -x -C <cand>
git -C <repo> archive 865d580 | tar -x -C <rows>
docker run --rm -v <base>:/src-base:ro -v <cand>:/src-cand:ro -v <rows>:/src-rows:ro \
  -v <module cache>:/go/pkg/mod:ro -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local \
  -e GOFLAGS='-mod=readonly -buildvcs=false' -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand rows; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for round in 1 2 3 4; do for rev in base cand rows; do
    /tmp/records-$rev -stage replay -seal-age 1h -corpus /corpus -dir /data/$rev
    rm -rf /data/$rev
  done; done'
```

`replay` runs the corpus on its own clock, sealed hourly and merged as it
goes, then follows every sealed record from the first place, a thousand
records a batch.

## A follower behind by the whole corpus

| | Blocks | Bytes | Time, median of four | Range |
|---|---:|---:|---:|---:|
| `429d99a`, before | 7,376 | 81,156,908 | 2.95 s | 2.90 to 3.15 s |
| `8d061f0`, the cache | 1,882 | 21,360,814 | 3.03 s | 2.73 to 3.13 s |
| `865d580`, and a place's rows alone | 1,882 | 21,360,814 | 2.56 s | 2.52 to 2.60 s |
| unmerged, `abef94f` of the merge round | 6,454 | 34,834,897 | 2.03 s | one run |

The file holds 1,861 blocks, and a follower now fetches 1,882: 21 fetched
again once the cache had let them go. It fetches less than the unmerged file
cost, since merged blocks are fewer and each is fetched about once.

## The file

The holder index costs 41,984 bytes, 0.0317 a record, on the corpus sealed
hourly: its 4,206 places each name their holder. The file goes from 16.7794
to 16.8111 bytes a record; nothing else changes, block for block.

## What this settles, and what it leaves

- A follower far behind fetches each merged block about once: 21.4 MB where
  it fetched 81.2, and less than the 34.8 MB the unmerged file cost.
- The cache takes back the bytes and not the time: from a tmpfs a follower
  spends its time decoding, and a merged block was decoded whole for each of
  its places. Building only a place's rows takes the follow from 2.95 to
  2.56 s; it stays above the unmerged file's 2.03 s, since a block of a
  thousand records is still decompressed for each place of a few dozen.
- A follower that keeps up reads places before they merge, and neither pays
  nor gains.
