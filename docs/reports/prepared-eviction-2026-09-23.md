# Retain recently used prepared SQL programs

WP3 noted that the bounded per-connection statement cache evicted in insertion
order: a hot program could be removed after many other SQL shapes even if it
had just run. The shared prepared-connection cache now moves a hit to the back
of its 32-entry order. It remains bounded, connection-owned and independent of
read snapshots. `TestPreparedReadCacheKeepsRecentlyUsedProgram` creates 32
shapes, reuses the first, then adds a 33rd and checks that the reused program
retains its original `*sql.Stmt`. The same cache code serves the pinned writer.

## Environment and result

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database lived on Docker
volume `tsperf`; every read stage copied the 10,000-series × 500-sample
`arch-s10k/read.db`, SHA-256
`cdf4a070790d6bd98865b68dc5f3ee46f8adae3f5b45a5b2b11835980301405d`.
The old variant was revision `3dbf9ff` and the new one `23eb592`, with the
same `bench/perf` source at `20f5c93`. Only the cache hit order changed.

One-hour, 360-sample, fixed-series reads ran old, new, new, old sequentially,
four seconds per stage:

| readers | old QPS | new QPS | allocs/query |
|---:|---|---|---:|
| 1 | 11,762; 12,024 | 11,710; 11,867 | ~467.8 in both |
| 8 | 11,061; 10,968 | 11,035; 10,975 | ~468.9 in both |

The ranges overlap. There is **no demonstrated throughput gain or regression**
for this stable SQL shape. The benefit is keeping recently used programs when
many shapes compete; the functional cache test establishes that behavior, but
this round does not assign it a production QPS saving. A hit now searches up
to 32 retained texts, so a different workload could rank that cost differently.

## Reproduce and checks

```sh
go test ./internal/sqlite -run TestPreparedReadCacheKeepsRecentlyUsedProgram -count=1 -v
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-lru-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-lru-new -dir /perf/arch-s10k -stage read \
  -series 10000 -shape hour -readers 8 -seconds 4
```

The paired baseline was built from `3dbf9ff` through a Go overlay of only
`internal/sqlite/reader.go`; the candidate binary used `23eb592`. `task check`
passed on Windows and `go test -race ./internal/sqlite ./metrics` passed in
the Go 1.27.1 Linux container. Other SQL shapes still need normalization
before increasing the cache cap.
