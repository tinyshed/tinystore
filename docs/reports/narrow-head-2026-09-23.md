# Decode only selected packed-head chunks

This implements D6 and the narrow-head part of WP5. A query still fetches and
charges the full compressed tail against `PayloadBytes`. Before decoding, it
verifies the whole-head CRC, version, chunk counts, ordering and endpoints.
Only chunks overlapping `[from,to)` enter the codec decoder and the
`DecodedSamples` budget. A selected chunk can contain up to 240 samples for
one returned point. Legacy row heads retain their full-head read and budget
behavior. Ingest and maintenance still decode the full mutable head.

`TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum` checks a
481-sample head: a request for its last point succeeds with
`DecodedSamples=1`, a point in the first chunk needs 240, and corruption in an
unselected prefix is rejected by the whole-head CRC with no partial answer.
`TestBatchedNarrowHeadsChargeSelectedChunks` checks the same accounting across
20 matched series. The compressed bodies of skipped chunks are protected by
the head CRC but are not individually passed through the codec decoder until
selected by a query.

## Environment and fixture

Ryzen 7 7700, Windows 11, Docker Desktop,
Linux 6.18.33.2-microsoft-standard-WSL2, 16 visible CPUs,
Go 1.27.1 linux/amd64, modernc SQLite 1.59.0. The database was on Docker
volume `tsperf`. The dedicated fixture has 1,000 series × 481 ordered samples
kept in packed heads without calling `Maintain`; it occupies 540,672 bytes.
SHA-256 of `/perf/arch-narrow/read.db`:
`0ea24e4b0a592c04cc2db36d203b4bd1940490f91385c551762a6d317a63ca70`.
Each timed stage copied that file before opening it.

The baseline engine was `0bf1669`; the candidate implementation is
`7bcc8ec`, with harness revision `64ce638`. The baseline binary used a Go
overlay of its `metrics/head.go`, `head_batch.go` and `query.go` with the same
current harness. Runs were sequential old, new, new, old, five seconds each.

| one-series read | old QPS | new QPS | old → new allocs/query |
|---|---|---|---:|
| one-point window, one reader | 16,418; 15,677 | 17,287; 17,195 | 325.9 → 320.9 |
| one-point window, eight readers | 16,088; 15,976 | 16,573; 16,671 | 326.5 → 321.6 |
| whole 481-point series, one reader | 14,028; 14,603 | 13,842; 14,417 | 335.0 → 335.9 |

The two-run means improved about **7%** for a point with one reader and
**4%** with eight. Whole-series ranges overlap, so a throughput change there
is not established. The new inspection adds work when all chunks are selected;
this round does not claim a full-range speedup. Query bytes fetched and the
on-disk format did not change.

## Reproduce and checks

```sh
docker run --rm -v <repo>:/src -v tsperf:/perf -v tinystore-gocache:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -w /src/bench/perf golang:1.27 go build -o /perf/perf-arch-narrow-new .
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-narrow-new -dir /perf/arch-narrow \
  -stage narrow_populate -series 1000 -samples 481
docker run --rm -v tsperf:/perf golang:1.27 \
  /perf/perf-arch-narrow-new -dir /perf/arch-narrow -stage read \
  -series 1000 -shape point -readers 1 -seconds 5
go test ./metrics -run 'TestNarrowPackedHeadChargesSelectedChunksAndChecksWholeChecksum|TestBatchedNarrowHeadsChargeSelectedChunks' -count=1 -v
```

`task check` passed on Windows; `go test -race ./metrics` passed in the
Go 1.27.1 Linux container; `go -C bench/perf test ./...` passed on Windows.
The redundant ordinary-value envelope in D7 remains unbuilt.
