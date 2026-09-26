# Reads beside a writer that appends and seals — 2026-09-26

The engine round tested that a reader finds each record once while appends
and sealing run beside it, and did not measure what reads cost then, nor the
process's memory and write-ahead log. The `load` stage of `bench/records`
builds a store of a million frontend records, then runs three phases of 20
seconds: readers alone, a writer alone, and both. A reader reads one second
at a random place, again and again; the writer appends flushes of 1024 records
as fast as it can, or 20,000 records a second, and seals each segment that
fills. Nothing of the fixture is held beyond the flush being written, and a
watcher takes the process's resident memory, its Go heap and the size of
`records.db-wal` every tenth of a second.

## Environment and reproduction

- `b74c667`, the engine of `1653b07` and the runner's `load` stage.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2, the `golang:1.27` container (go1.27.1
  linux/amd64), the store on a tmpfs, no memory budget: two reads and two
  appends at once, the engine's slots.

```sh
git -C <repo> archive b74c667 | tar -x -C <cand>
docker run --rm -v <cand>:/src:ro -v <module cache>:/go/pkg/mod:ro -e GOWORK=off -e GOTOOLCHAIN=local \
  -e GOFLAGS='-mod=readonly -buildvcs=false' -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  cd /src/bench/records && go build -o /tmp/records . &&
  for r in 2 8; do for rate in 0 20000; do
    /tmp/records -stage load -records 1000000 -readers $r -rate $rate -phase 20s -dir /data/l-$r-$rate
    rm -rf /data/l-$r-$rate
  done; done'
```

## Results

| Readers | Writer | Reads | p50 | p99 | Max | Appended a second | Peak RSS | Peak heap | Peak WAL |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 2 | none | 23,666 | 1.67 ms | 2.69 ms | 4.8 ms | — | 43.4 MiB | 27.2 MiB | 1.1 MiB |
| 0 | as fast as it can | — | — | — | — | 177,715 | 55.4 MiB | 35.1 MiB | 1.1 MiB |
| 2 | as fast as it can | 20,772 | 1.86 ms | 3.73 ms | 12.7 ms | 121,702 | 66.6 MiB | 42.9 MiB | 1.2 MiB |
| 2 | 20,000 a second | 23,519 | 1.67 ms | 3.04 ms | 7.3 ms | 20,019 | 65.7 MiB | 38.1 MiB | 1.1 MiB |
| 8 | none | 23,672 | 6.70 ms | 8.70 ms | 12.5 ms | — | 43.5 MiB | 17.5 MiB | 1.1 MiB |
| 8 | as fast as it can | 20,323 | 7.75 ms | 11.70 ms | 19.9 ms | 122,010 | 66.5 MiB | 43.7 MiB | 1.3 MiB |
| 8 | 20,000 a second | 22,017 | 7.14 ms | 10.30 ms | 19.6 ms | 20,019 | 65.5 MiB | 43.0 MiB | 1.1 MiB |

- A writer at full speed, sealing a segment every 0.13 s, raises a
  one-second read's median by a tenth and its p99 by two fifths; a writer at
  20,000 records a second leaves the median where it was.
- Eight readers read no more than two: the engine lets two reads in at once,
  and the other six wait for a slot, so a read's latency grows fourfold and
  the reads a second stay near 1,180.
- The write-ahead log stays near a megabyte, the reads holding their
  snapshots for a millisecond or two; the process stays under 67 MiB of
  resident memory with the writer, readers and the SQLite pages together.

## What this settles, and what it leaves

- Reads beside appends and sealing cost what the table shows, with the
  memory and the log bounded; the race-detector test still guards that each
  record is read once.
- Not measured: macOS, Linux outside a container, a store on a disk, and a
  memory budget narrower than what these phases use.
