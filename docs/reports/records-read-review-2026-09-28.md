# Bounded records page reads — 2026-09-28

The review changed `Read` to walk indexed candidates in bounded order rather
than materialize every candidate before applying a page's budget. This round
compares the previous commit `dd59ab2` with the reviewed tree, from two test
binaries run alternately in one Linux container. Full output is in
[data/records-read-review-2026-09-28-linux.txt](data/records-read-review-2026-09-28-linux.txt).

| Fixture and page | Previous `Read` p50 | Reviewed `Read` p50 | Allocated per call |
|---|---:|---:|---:|
| 5,000 head rows, broad oldest, four-block budget | 3.37–3.45 ms | 30–36 µs | 3.09 MB → 20–21 KB |
| 5,000 head rows, broad newest | 3.41–3.44 ms | 28–44 µs | 3.09 MB → 20 KB |
| 5,000 head rows, narrow ten-millisecond range | 224–228 µs | 53–56 µs | 22 KB → 20 KB |
| 1,000 sealed one-record blocks, broad oldest | 862–865 µs | 48–50 µs | 632 KB → 39 KB |
| 1,000 sealed blocks, broad newest | 805–832 µs | 55–59 µs | 632 KB → 39 KB |
| 1,000 sealed blocks, narrow ten-millisecond range | 49–51 µs | 47–48 µs | 40 KB → 39 KB |

All values above are ranges of the two passes. Each pass ran twenty warmed
calls per path, measuring the median with Go's clock and total allocations
with `runtime.MemStats`. The test also measured snapshot fetch alone; its
figures are in the raw output. These are small, bounded pages on one fixture,
not general throughput guarantees.

The new `heads_by_first` index occupied 198,656 bytes on the head fixture;
the logical SQLite file grew from 1,093,632 to 1,293,312 bytes. The new
`blocks_by_first` index occupied 45,056 bytes on the sealed fixture; that
file grew from 318,464 to 364,544 bytes. `dbstat` shows the table and old
index pages unchanged on each fixture. File bytes are `page_count × page_size`
(1,024-byte pages); the raw output lists every object.

Environment: Docker Desktop 29.6.2 on the Windows Ryzen 7 7700 host of the
earlier rounds, Linux kernel 6.18.33.2-microsoft-standard-WSL2,
`golang:1.27` (go1.27.1 linux/amd64), `CGO_ENABLED=0`, modernc SQLite
v1.59.0. Test files lived on a Docker volume. The head fixture appends one
record to each of 5,000 streams; the sealed fixture seals one record in each
of 1,000 streams. Both ask for 100 records with a four-block budget.

Reproduce from the repository root, with an unmodified `dd59ab2` checkout
at `<old>` and the same `records/read_round_test.go` in both trees:

```sh
docker volume create tinystore-records-read-20260928
docker run --rm -v <old>:/old -v <reviewed>:/new \
  -v tinystore-records-read-20260928:/perf \
  -e GOWORK=off -e CGO_ENABLED=0 -e GOCACHE=/perf/gocache \
  -e GOMODCACHE=/perf/mod -e TINYSTORE_SPIKE=1 \
  -e TINYSTORE_RECORDS_DIR=/perf golang:1.27 sh -c '
    cd /old && go test -c -o /perf/old.test ./records &&
    cd /new && go test -c -o /perf/new.test ./records &&
    for pass in 1 2; do
      /perf/old.test -test.run="^TestRead.*CandidatesMeasured$" -test.v -test.count=1
      /perf/new.test -test.run="^TestRead.*CandidatesMeasured$" -test.v -test.count=1
    done'
```
