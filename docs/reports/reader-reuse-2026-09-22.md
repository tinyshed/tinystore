# Reader reuse: first measured execution change

The first experiment from [the engine audit](engine-audit-2026-09-22.md)
and [the execution review](execution-review-2026-09-22.md) addresses D4 and
the idle-slot part of D5. A normal callback error now retains its reader and
prepared statements after a successful rollback. Idle readers are reused in
last-returned order, so sequential calls warm one connection. Cancelled,
broken and uncertain transactions still discard their connection. Snapshot
and statement-cache limits are unchanged.

This is not all of WP3 or D5: statement eviction remains FIFO, SQL shapes are
not normalized, and the proposed execution-layer changes were not made.

## Environment and input

Ryzen 7 7700, Windows 11, Docker Desktop with Linux
6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs, Go 1.27.1 linux/amd64,
modernc SQLite 1.59.0. The database lived on Docker volume `tsperf`.
The fixed read fixture was 10,000 series × 500 samples, SHA-256
`4d41358bd6c0854baffba05829406090559237803390bd8e12eac4c2da44c436`.
The old engine was `f4568cc` (the same production code as `f2974bc`); the
new engine was the working tree on `codex/architecture-measurements`. All reads
were sequential warm stages using the same database, not simultaneous runs.

## Results

The targeted benchmark reads one row and returns an ordinary application
error on each call. Baseline and candidate test binaries used the same
benchmark source and ran old, new, new, old in one container invocation,
three seconds each:

| version | ns/op | B/op | allocs/op |
|---|---:|---:|---:|
| old, run 1 | 10,373 | 2,339 | 46 |
| new, run 1 | 6,889 | 1,683 | 36 |
| new, run 2 | 6,930 | 1,683 | 36 |
| old, run 2 | 10,227 | 2,339 | 46 |

The new path is about 33% faster on this error-heavy workload, with ten fewer
allocations per call. A deterministic test also confirms that four sequential
reads with `MaxReaders=4` use one warm connection, rather than opening four.

The normal read workload selects one series and a one-hour window returning
360 samples. The first two five-second runs per version were made before and
after the edit; a further eight-reader comparison alternated new, old, old,
new in one container invocation:

| readers | old QPS | new QPS |
|---:|---|---|
| 1 | 8,174; 8,474 | 8,533; 8,405 |
| 8 | 9,133; 9,365; 9,159; 9,032 | 9,079; 9,057; 9,140; 9,197 |

These ranges overlap. There is no demonstrated throughput gain or loss for
ordinary successful reads. The improvement is specific to errors and avoiding
unnecessary cold connections; it should not be projected onto query QPS.

## Reproduce

Build test binaries for both revisions, keeping the benchmark source identical.
The old production `file.go` and `reader.go` were read from `f4568cc` through a
Go overlay; the current `reader_test.go` supplied the benchmark to both builds.
The overlay maps the two `/src/internal/sqlite/` paths to copies under the
ignored `/src/bench/corpus/` directory. Run the two test binaries alternately:

```sh
mkdir -p bench/corpus/reader-baseline
git show f4568cc:internal/sqlite/file.go > bench/corpus/reader-baseline/file.go.txt
git show f4568cc:internal/sqlite/reader.go > bench/corpus/reader-baseline/reader.go.txt
cat > bench/corpus/reader-baseline/overlay.json <<'EOF'
{"Replace":{"/src/internal/sqlite/file.go":"/src/bench/corpus/reader-baseline/file.go.txt","/src/internal/sqlite/reader.go":"/src/bench/corpus/reader-baseline/reader.go.txt"}}
EOF
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src golang:1.27 sh -c 'go test -overlay /src/bench/corpus/reader-baseline/overlay.json -c -o /perf/sqlite-old.test ./internal/sqlite && go test -c -o /perf/sqlite-new.test ./internal/sqlite'
docker run --rm -v tsperf:/perf golang:1.27 sh -c \
  'for v in old new new old; do /perf/sqlite-$v.test -test.run "^$" -test.bench "^BenchmarkPreparedReadAfterApplicationError$" -test.benchtime=3s -test.count=1; done'
```

Build `bench/perf` at each revision, giving its binary a distinct path on
`tsperf`, then alternate the binaries against the pinned read fixture:

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-new -dir /perf/s10k -stage read -series 10000 \
  -shape hour -readers 8 -seconds 5
```

The old performance-harness binary was built as `/perf/perf-arch-baseline` with
the same command as the new one. Both used the same read fixture.

## Checks

`go test ./internal/sqlite` and the full `go test -shuffle=on ./...` passed on
Windows. `task check` passed tidy, formatting, lint, tests and vulnerability
scan, then failed in `task size` because `wc` is absent from this Windows PATH.
The two Linux/amd64 binaries were built; their Windows-side file sizes were
8,261,792 bytes for the API probe and 1,507,488 bytes for the baseline, a
6,596 KiB import difference. `go test -race ./internal/sqlite ./metrics` also
passed in the Go 1.27.1 Linux container. This does not claim a successful
`task check` run.

Later on this branch, `task size` gained a host-native Go reporter while its
probe binaries remained linux/amd64. A subsequent full `task check` passed on
Windows; it reported an 8,165 KiB probe and a 6,553 KiB import difference for
the later engine state.
