# The blobs engine against its prototype — 2026-09-27

The Windows file-write findings below are followed by
[a profiled optimization round](blobs-write-2026-09-27.md), which supersedes
them for the changed engine. This report retains the original measurements.

[The completion round](blobs-completion-2026-09-27.md#a-correction-to-the-old-measurement-fixture)
also corrects a test-stream generator that restarted its chunk at each Read.
The streaming Put/Create and engine/prototype paths below therefore did not
receive strictly identical byte sequences at different transfer sizes.
Their numbers remain observations of each path. Placement measurements used
bytes.Reader and are unaffected; the completion round repeats streaming with
an input independent of read size.

The public engine of [blobs/README.md](../../blobs/README.md), measured beside
the prototype from [the mechanics round](blobs-mechanics-2026-09-27.md).
Both run sequentially in one session on each operating system: prototype
first, engine second. Windows and Linux run sequentially too, to avoid
making their writes compete for the same disk. This round changes no
production code.

## How to read the numbers

Large sequential objects are a bytes-per-second workload. Small independent
objects are an objects-per-second workload: each needs a durable file and/or
database publication, however few bytes it holds. Their tail latency matters
to a caller waiting for one upload; a high aggregate rate does not make that
wait short. The read tables cover cached whole reads, not a disk seek or a
network request. Throughput, p99 and sampled heap answer different questions.

The useful findings of this run:

- **Large streaming objects:** the engine writes 4 GiB through `Create` in
  4.63 s on Linux and 5.03 s on Windows. `Put` takes 7.83 and 7.39 s. Whole
  checked reads reach 1.81 and 1.28 GB/s respectively. The transfer-buffer
  difference is a candidate to investigate; this is not a controlled test
  of that difference alone.
- **Inline writes cost more than the prototype on both systems.** At 128
  callers the engine writes 4 KiB objects at 5,755/s against 10,275/s on
  Linux, and 7,691/s against 11,429/s on Windows.
- **Concurrent file writes are the Windows weakness.** At 128 callers,
  64 KiB files reach 165/s against the prototype's 1,086/s; engine p99 is
  897 ms. Linux reaches 4,221/s against 4,960/s, with engine p99 of 63 ms.
  These differences identify a profiling target, not its cause.
- **Large fan-out is different on Linux:** 1 MiB uploads at 1024 callers
  reach 3,978/s against the prototype's 1,378/s; Windows reaches 139/s
  against 415/s. One fixed-order run cannot establish the stability or
  mechanism of that Linux advantage. Sampled engine heap rises to 47 and
  51 MiB respectively, rather than staying flat with concurrency.
- **The stress scrub has a small read-tail effect in this fixture:** Linux
  p99 moves from 1.080 to 1.091 ms; Windows from 1.166 to 1.565 ms.

These results support cached reads and streaming large objects as useful
first-version paths. They do not justify carrying the prototype's write
rates into the engine's promises, especially on Windows.

## Placement through the public API

The same seeded bytes, sizes, key pattern and caller counts on both sides:
new objects written for three seconds, then whole objects read at random
among each caller's keys for three seconds. `Open` in these tables includes
opening, reading every byte and closing. The prototype hashes those bytes;
the engine also checks the stored SHA-256. Neither column is an open-only
latency. Reads follow writes with no page-cache eviction.

The prototype column uses inline bodies at 4 and 16 KiB and files at 64 and
256 KiB, matching the engine's placement. The raw logs also keep the
prototype's alternative placements. Rates are objects a second. This is a
time-bounded load, so the faster side creates and subsequently reads from
more objects; it is not an identical fixed-count read corpus. Random read
selection is not replayed between implementations.

Linux:

| size | callers | prototype Put | engine Put | prototype Open | engine Open |
|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1 | 338 | 135 | 82,101 | 62,403 |
| 4 KiB | 16 | 2,789 | 1,323 | 112,113 | 67,528 |
| 4 KiB | 128 | 10,275 | 5,755 | 109,547 | 82,017 |
| 16 KiB | 1 | 338 | 159 | 39,819 | 29,026 |
| 16 KiB | 16 | 2,329 | 1,253 | 68,026 | 40,313 |
| 16 KiB | 128 | 6,334 | 4,328 | 52,402 | 49,377 |
| 64 KiB | 1 | 146 | 101 | 15,610 | 16,688 |
| 64 KiB | 16 | 1,102 | 848 | 42,742 | 33,987 |
| 64 KiB | 128 | 4,960 | 4,221 | 38,271 | 70,202 |
| 256 KiB | 1 | 136 | 96 | 5,270 | 5,198 |
| 256 KiB | 16 | 993 | 761 | 29,150 | 52,511 |
| 256 KiB | 128 | 3,636 | 2,823 | 22,648 | 37,335 |

Windows:

| size | callers | prototype Put | engine Put | prototype Open | engine Open |
|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1 | 680 | 355 | 65,428 | 37,374 |
| 4 KiB | 16 | 5,536 | 2,823 | 91,930 | 60,256 |
| 4 KiB | 128 | 11,429 | 7,691 | 95,006 | 71,520 |
| 16 KiB | 1 | 636 | 290 | 20,335 | 18,173 |
| 16 KiB | 16 | 4,192 | 2,248 | 47,987 | 34,528 |
| 16 KiB | 128 | 7,436 | 4,735 | 51,454 | 33,013 |
| 64 KiB | 1 | 278 | 90 | 8,053 | 7,524 |
| 64 KiB | 16 | 879 | 145 | 16,548 | 13,157 |
| 64 KiB | 128 | 1,086 | 165 | 17,543 | 28,790 |
| 256 KiB | 1 | 235 | 115 | 4,349 | 3,503 |
| 256 KiB | 16 | 799 | 131 | 14,403 | 21,996 |
| 256 KiB | 128 | 1,047 | 163 | 15,177 | 24,714 |

The engine adds its public contract: admission, path and option checks,
content identity, publication, metadata and integrity verification. This
round measures their combined cost; it does not isolate any one of them.

## Large objects

Each write streams 4 GiB made by repeating the same seeded 64 KiB chunk.
The comparable prototype row syncs every 256 MiB. Its time covers writing
and periodic syncs; its final sync is logged separately. The engine's
`Create` time also includes `Commit`, which publishes the object. Engine
`Put` carries the stream through its own 16 KiB buffer, whereas `Create`
receives 64 KiB writes directly. The input bytes are identical; the API and
commit work differ.

| operation | Linux | Windows |
|---|---:|---:|
| prototype, 64 KiB writes, sync every 256 MiB | 3.324 s | 4.846 s |
| prototype, final sync | 0.6 ms | 0.5 ms |
| engine Put, 16 KiB transfer buffer, including commit | 7.83 s / 0.55 GB/s | 7.39 s / 0.58 GB/s |
| engine Create, 64 KiB writes, including commit | 4.63 s / 0.93 GB/s | 5.03 s / 0.85 GB/s |
| engine Create's Commit, included above | 9.3 ms | 5.7 ms |
| engine whole read, SHA-256 checked | 1.81 GB/s | 1.28 GB/s |
| engine unchecked ranges, 4 MiB each | 3,123/s / 13.10 GB/s | 2,288/s / 9.60 GB/s |

The engine's whole read checks all 4 GiB. Its range measurement is 1,000
unchecked `ReadAt` calls of 4 MiB at seeded random offsets in that object.
These are standalone engine results, not a rerun of the mechanics round's
1 GiB `http.ServeContent` benchmark; those rates must not be divided to
claim an improvement or regression.

## Uploads at once

4,096 objects of 1 MiB, the same repeated seeded chunk and key pattern,
at each concurrency. Prototype uploads use a 64 KiB transfer buffer;
engine `Put` uses 16 KiB and includes the full publication path. Each cell
is uploads a second followed by sampled Go `HeapInuse` in MiB.

| callers | Linux prototype | Linux engine | Windows prototype | Windows engine |
|---:|---:|---:|---:|---:|
| 16 | 835 / 7 MiB | 580 / 5 MiB | 305 / 7 MiB | 119 / 6 MiB |
| 64 | 1,679 / 8 MiB | 1,545 / 7 MiB | 482 / 9 MiB | 150 / 8 MiB |
| 256 | 1,453 / 12 MiB | 3,008 / 16 MiB | 475 / 17 MiB | 150 / 17 MiB |
| 1024 | 1,378 / 23 MiB | 3,978 / 47 MiB | 415 / 23 MiB | 139 / 51 MiB |

Heap is sampled every 20 ms after a GC before each case. It includes Go
allocations throughout the process, not just the upload buffers; it excludes
the OS page cache and is neither RSS nor a proof of the configured store
memory bound. No finite store memory budget is set in this round. The
prototype's additional 64 MiB cases remain in the raw logs; the engine did
not repeat them.

## Readers beside scrub, usage and Windows renames

The reader test creates 4,096 files of 64 KiB, then measures 16 callers for
three seconds alone and three seconds beside a loop calling the real scrub
as fast as it returns. This deliberately removes its normal one-minute
spacing. It measures that stress case, not the latency of scheduled
maintenance, and is not an upper bound on every workload.

| engine measurement | Linux | Windows |
|---|---:|---:|
| whole 64 KiB reads alone | 79,975/s | 32,863/s |
| alone, p50 / p99 | 155 / 1,080 µs | 519 / 1,166 µs |
| whole 64 KiB reads beside scrub | 78,018/s | 31,096/s |
| beside scrub, p50 / p99 | 158 / 1,091 µs | 519 / 1,565 µs |
| scrub rate during that load | 0.14 GB/s | 0.11 GB/s |
| Usage over 100,000 objects, median of five | 14.8 ms | 14.8 ms |
| rename retries in all four placement sizes | 0 | 0 |

`Usage` counts a folder populated through the public API with 100,000
one-byte objects by 64 writers. The reported time is the median of five
calls after population. The count is checked on every call. This is an
engine measurement; the prototype's usage scan was not rerun in this
session and its older timing is not a paired comparison.

Rename retries are observed during the placement test with Defender
real-time protection enabled on Windows. There is no injected scanner
contention, so zero retries would mean only that this run did not observe
one, not that the retry path was exercised or proved unnecessary.

## Environment and reproduction

- Production engine and prototype: `15bdb3bf0465dac657953a6d8d32ac49ff440817`.
  The added harness is [blobs/load_test.go](../../blobs/load_test.go), delivered
  with this report, including cleanup of measurement helper goroutines on
  failure paths.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors; Samsung 990 PRO NVMe.
- Windows 11 Pro 10.0.26200, NTFS, Go 1.27.1 windows/amd64;
  Defender real-time protection enabled.
- Docker Desktop 29.6.2 on WSL2, 16 CPUs, about 15 GiB visible;
  `golang:1.27`, Go 1.27.1 linux/amd64, files on a named ext4 volume.
- `modernc.org/sqlite` v1.59.0, WAL, `synchronous=FULL`, 4 KiB pages.
  Engine stores use `Manual: true`; only the explicit scrub test runs scrub.
- One run per system; the scrub test is baseline then stressed, not
  alternated. No confidence interval, cold-cache result or production-host
  guarantee follows from these short runs. An unrelated Redis container
  was running; the host was not reserved exclusively for benchmarks.

Windows PowerShell, with an existing empty directory on the chosen disk:

```powershell
Set-Location <repo>
$env:GOWORK = 'off'
$env:TINYSTORE_SPIKE = '1'
$env:TINYSTORE_BLOBS_DIR = '<measurement directory>'
go test ./spike -run '^(TestBlobsPlaces|TestBlobsLargeUpload|TestBlobsUploadsAtOnce)$' -v -count=1 -timeout 30m
go test ./blobs -run 'Measured$' -v -count=1 -timeout 30m
```

Linux, after the Windows process finishes:

```sh
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go-cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -e CGO_ENABLED=0 -e TINYSTORE_SPIKE=1 -e TINYSTORE_BLOBS_DIR=/perf \
  -w /src golang:1.27 sh -c '
  set -e
  go test ./spike -run "^(TestBlobsPlaces|TestBlobsLargeUpload|TestBlobsUploadsAtOnce)$" -v -count=1 -timeout 30m
  go test ./blobs -run "Measured$" -v -count=1 -timeout 30m'
```

Both use the default three-second load and 4 GiB large object. Do not set
`TINYSTORE_BLOBS_LARGE` when reproducing this comparison: only the prototype
honours that override. Unset `TINYSTORE_SPIKE` before normal tests.

Raw output: [Windows](data/blobs-engine-2026-09-27-windows.txt),
[Linux](data/blobs-engine-2026-09-27-linux.txt). All three prototype tests and
all five engine measurements passed on each platform. The Windows session
ran from 16:08 to 16:16 UTC; the Linux session from 16:16 to 16:22 UTC.

## What remains

Profile Windows file publication before changing it: distinguish the time
spent writing bytes, syncing the file and directories, renaming and waiting
for SQLite publication. Repeat candidate changes against the current engine
on identical input in alternating order. The smaller transfer buffer and
the additional durable publication work are hypotheses, not explanations
established by this round.

Still unmeasured here: cold-cache reads, mixed uploads and reads, read-only
open latency, a finite memory budget under these loads, forced Windows
rename contention, normal scheduled scrub, engine uploads of 64 MiB at once,
usage beyond 100,000 objects, and the production host. No format, placement
threshold or durability setting changes on the strength of this run.
