# The rice parameter chosen among all 64 — 2026-09-26

An integer column written with a rice code used to price four parameters
around the logarithm of its residuals' mean and keep the cheapest. A few wide
gaps raise the mean for every value, so a column of small residuals with two
large ones was written as if all of them were large. The encoder now prices
every parameter from 0 to 63 exactly, from one pass over a histogram of the
residuals' bit lengths, and skips those past the longest residual, which only
add bits. The format and the decoder are unchanged, and the golden vectors are
the same bytes.

```text
14 zeros and 1000000 twice   k 0: 14×1 + 2×96 = 206 bits
                             k 16, near log2 of the mean: 14×17 + 2×(15+17) = 302 bits
```

The example is `TestTheRiceParameterIsTheCheapestOfAll`, which also checks
the choice against every parameter on 200 random columns.

## Environment and reproduction

- The baseline at `b98d703`, the candidate at `68aee01`, which changes
  `records/ints.go` alone; the runner is `bench/records` at each.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The research's fixtures as the engine round used them, and the private
  production snapshot `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`,
  read as that round read it. Aggregates only.

Both revisions ran in one container, one after the other, and the replays in
a second with the same mounts:

```sh
git -C <repo> archive b98d703 | tar -x -C <base>
docker run --rm -v <repo>:/src-cand:ro -v <base>:/src-base:ro -v <module cache>:/go/pkg/mod:ro   -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false'   -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand; do
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage late -batch 16384 -dir /data/$rev-late
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
  done
  for rev in base cand; do (cd /src-$rev && go test ./records -run "^$" -bench "^BenchmarkSegment$"     -benchtime=20x -benchmem -cpu=1 -count=6); done'
docker run … golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for age in 1h 6h 24h; do for rev in base cand; do
    /tmp/records-$rev -stage replay -seal-age $age -corpus /corpus -dir /data/$rev-replay-$age
  done; done'
```

## Bytes a record in the file

| | `b98d703` | `68aee01` |
|---|---:|---:|
| one million frontend records | 7.8899 | 7.8899 |
| the same, one in a hundred up to ten minutes late | 7.9657 | 7.9217 |
| the production corpus, full segments | 20.8984 | 20.7738 |
| its text containers' payload | 20.1584 | 20.0204 |
| its JSON containers' payload | 21.8602 | 21.8412 |
| its `blocks` b-tree, by `dbstat` | 20.6833 | 20.5587 |
| sealed an hour old, on the corpus's own clock | 22.9463 | 22.8333 |
| sealed six hours old | 21.3309 | 21.2133 |
| sealed a day old | 20.9951 | 20.8736 |

Every other object of the corpus's file takes the same pages: the change took
its 0.12 bytes a record from the blocks' columns and gave them to nothing
else. The frontend fixture does not move: its times are random nanoseconds
within a second of their turn, whose best parameter is the one the mean
points at. Its late variant sends one record in a hundred up to ten minutes
back to the late head, whose blocks have the wide gaps that misled the mean.

The queries of the engine round read the same blocks on the corpus: the
busiest minute 36 blocks and 455,804 bytes against 460,867, one request id 11
blocks and 42,483 bytes, `level = 50` 186 blocks and 2,872,024 bytes against
2,874,452. One-second reads of the late fixture fetch 4.63 blocks in both.

## Speed

`BenchmarkSegment`, one CPU, six runs of twenty segments of 16,384 records,
records a second, the median of six and their range:

| | `b98d703` | `68aee01` |
|---|---:|---:|
| frontend encode | 564,114 (544,258 to 582,965) | 550,136 (526,774 to 578,665) |
| frontend decode | 1,387,872 (1,281,305 to 1,456,908) | 1,373,384 (1,289,326 to 1,466,771) |
| backend encode | 724,407 (687,112 to 742,297) | 791,939 (705,720 to 807,940) |
| backend decode | 1,393,439 (1,302,766 to 1,510,001) | 1,339,579 (1,238,917 to 1,414,528) |

Allocation is the same to the byte: 10,860,840 a frontend segment encoded,
663 bytes a record. The decoder did not change and its medians moved by up to
4 %, which is this machine's spread from one run to the next; the encoders'
medians moved by less than that one way and more the other. The hand-off's
gates hold in both: at least 400,000 records a second encoded and 1.3 million
decoded at the median.

## What this settles, and what it leaves

- The rice parameter is now the cheapest there is, and the production corpus
  pays 20.77 bytes a record in full segments. The gap to the prototype's
  20.09, which kept the text sample and chose its parameter as the engine
  used to, narrows from 0.81 to 0.69; what the sample would be worth beside
  the new parameter is not measured.
- A column of one step with a few exceptions still pays about a bit a value
  in a rice code; a list of the exceptions, as metrics writes its clocks,
  would pay a few bytes a column. Nothing measured here asks for it.
