# Blobs: streaming, mixed work and the remaining operational checks — 2026-09-27

This follows [the Windows publication fix](blobs-write-2026-09-27.md).
It tests the five remaining questions: large `Put`, mixed reads and writes,
scheduled scrub, Windows rename contention, and scale with bounded memory.
The production change is a larger optional transfer buffer for a long `Put`.
File layout, durability order, inline placement and the public API stay the
same.

## Large Put

After 256 KiB have passed into a file, `Put` tries once to reserve a 64 KiB
transfer buffer. It reserves that memory before allocating, while the old
16 KiB is still charged: the transition can hold 80 KiB. It then releases
the old reservation. If the new reservation cannot be taken immediately,
the upload continues with 16 KiB; it never waits while holding the buffer
another upload needs. `Create`/`Write` keeps its original allocation.

Each cell is seconds to stream **4 GiB**, two runs, with the binaries run in
order **before, after, after, before**. A checked whole read follows each
write outside its timing. Both binaries include the Windows publication fix.

| platform and memory budget | before | after |
|---|---:|---:|
| Windows, no configured budget | 5.50 · 5.52 | 4.23 · 4.24 |
| Windows, 16 KiB | 7.96 · 7.75 | 6.49 · 6.46 |
| Linux, no configured budget | 7.21 · 6.91 | 6.59 · 6.14 |
| Linux, 16 KiB | 6.29 · 6.79 | 6.09 · 5.85 |

The unbudgeted Windows writes take about **23% less time**, Linux about
**9–11% less** in the adjacent pairs. The 16 KiB rows establish continued
progress with that budget; their timing differences cannot be attributed
to a buffer that did not grow. Two runs do not isolate all host, layout and
cache effects or establish a universal speedup.

A final single run compares the two APIs on the corrected identical stream,
`Put` first and `Create` with 64 KiB writes second, both including commit:

| 4 GiB, current engine | Windows | Linux |
|---|---:|---:|
| Put | 4.38 s | 6.05 s |
| Create / Write | 3.84 s | 5.02 s |

The APIs still differ by about 14% and 21% in this follow-up. `Put` pulls
and copies an unknown-length stream, while this `Create` caller supplies
its size and reuses a chunk directly. This is a fixed-order diagnostic,
not an isolated attribution of the remaining cost or proof of API parity.

### A correction to the old measurement fixture

The earlier test helper restarted its seeded chunk at every `Read`. A
16 KiB read therefore repeated a different byte sequence from a 64 KiB read.
The production reader was not at fault. The corrected fixture retains its
offset within the chunk; a regression test compares its output at read
sizes 7, 16 KiB, 64 KiB and 128 KiB against the same expected bytes.

This round's two binaries use that corrected helper and identical bytes.
The earlier report's streaming engine/prototype and `Put`/`Create` results
remain observations of their individual paths, not strictly identical-byte
comparisons. Its placement table and the Windows publication fix's
fixed-corpus comparison used `bytes.Reader` and are unaffected.

The buffer gates cover 16, 64 and 80 KiB budgets, byte-for-byte reads after
growth or fallback, and a failing source after growth that releases every
reservation and leaves no upload file or published object.

## Mixed uploads, replacements, deletes and reads

Twenty seconds per run: eight writers each own a key, delete it every fifth
operation and otherwise replace it with one of three complete payloads
(4 KiB, 64 KiB and 1 MiB). Eight readers choose those keys at random. Every
successful whole read is checked against the complete known versions,
including its bytes, while writers continue. The engine has a **1 MiB**
memory budget.

| operation | Windows before | Windows after | Linux before | Linux after |
|---|---:|---:|---:|---:|
| Put/s | 234 · 222 | 229 · 231 | 363 · 209 | 370 · 366 |
| Open attempts/s | 27,039 · 32,901 | 31,239 · 29,062 | 27,996 · 27,559 | 27,430 · 25,831 |
| successful whole reads/s | 18,905 · 22,182 | 21,093 · 20,234 | 20,275 · 19,944 | 19,830 · 18,895 |
| Open p99, ms | 1.061 · 1.037 | 1.043 · 1.043 | 0.879 · 0.850 | 0.877 · 0.898 |
| whole-read p99, ms | 2.586 · 2.268 | 2.316 · 2.575 | 3.061 · 2.977 | 3.101 · 3.140 |

All runs completed without a partial or corrupt version and returned their
engine reservations. Absence during deletion is expected and is counted in
the raw logs; Open-attempt rates include those misses. An Open whose key
changes three times may return `ErrConflict`, also counted separately.
This is a time-bounded stress workload, not a replay of an identical request
sequence. It supports correctness and latency observations; it does not
isolate a throughput gain from the new buffer. `io.ReadAll` and comparison
buffers belong to the measurement's caller, not the engine memory budget.

## Scheduled scrub

A real non-manual store runs for 75 seconds, with the normal one-minute
`Store.Every` tick. There are 256 files of 64 KiB, 16 concurrent readers and
a 2 MiB store memory budget. The test does not accelerate the ticker or
call scrub itself. It checks the persisted scrub cursor after the run.

| p99 latency, ms | Windows before / during / after tick | Linux before / during / after tick |
|---|---:|---:|
| Open | 1.091 / 0.834 / 0.796 | 0.628 / 0.499 / 0.463 |
| whole checked read | 1.553 / 1.042 / 1.040 | 0.758 / 0.569 / 0.528 |

Neither run shows an increase in its tick window's p99. This is not evidence
that maintenance makes reads faster: the windows are ordered, not randomized.
Both stores peak at 786,432 reserved bytes and return to zero.

Window 0 precedes 55 seconds after Open; window 1 covers seconds 55–65,
including the first maintenance tick; window 2 covers seconds 65–75.
The first window includes fixture preparation in its rate denominator,
so this report uses the latency percentiles rather than comparing its rate.
One slice advances to content 17, offset zero: exactly 1 MiB was read.
For this 16 MiB fixture, the minimum 1 MiB slice determines the pace;
larger stores use the existing 43,200th-of-bytes rule targeting 30 days.

These windows do not establish a bound on requests overlapping one exact
maintenance syscall, or on maintenance with millions of expired rows.
They measure the actual configured schedule on this fixture. The earlier
continuous-scrub stress measurement remains useful separately.

## Windows files held by another program

Two new Windows gates open an upload through ordinary `os.Open`, which does
not share deletion, before its commit attempts the rename:

- Release the handle after observing a retry: commit succeeds and the exact
  original bytes read back.
- Keep the handle through all five attempts: four retries are counted, the
  failed upload publishes no object; release the handle and run maintenance,
  which removes the leftover upload, then successfully upload the same key.

Both gates passed once during development and then **25 consecutive runs
each** on Windows. They also run in the normal Windows test suite.

This exercises both retry success and exhaustion deliberately. It does not
claim a frequency for real Defender interference; the earlier unforced
placement run observed no retry with Defender enabled.

## Concurrent large uploads with finite memory

64 callers each upload one 64 MiB object: **4 GiB total** for each budget.
The source is a shared seeded 64 KiB chunk repeated without materializing
the objects. Rates include successful commits. The first and last objects
are read whole and checked after timing.

| store budget | Windows time / GB/s | Linux time / GB/s | peak engine reservation, both | Windows / Linux sampled heap |
|---|---:|---:|---:|---:|
| 16 KiB | 5.51 s / 0.78 | 10.23 s / 0.42 | 16,384 B | 3 / 3 MiB |
| 256 KiB | 3.31 s / 1.30 | 3.69 s / 1.16 | 262,144 B | 5 / 5 MiB |
| 4 MiB | 0.90 s / 4.79 | 3.45 s / 1.24 | 4,161,536 B | 8 / 7 MiB |

Every run returned to zero reserved bytes and stayed within its configured
budget. A 16 KiB budget admits only one upload's buffer at once, even when
64 callers wait. Heap is process-wide Go `HeapInuse` sampled every 20 ms;
it includes SQLite and runtime allocations, excludes the OS page cache,
and is not RSS or the store's reservation ledger. These are single runs of
each budget, not a promise that Windows always beats Linux at this workload.

## A million objects

One million one-byte objects are populated through the public `Put` API
by 64 writers under `users/42/`. Each of five `Usage` calls checks both
the count and the byte total. Caches are not evicted. A page Scan then asks
for the first 1,000 objects and checks that another page exists.

| operation | Windows | Linux |
|---|---:|---:|
| populate through Put | 71.49 s | 205.10 s |
| Usage, median of five | 151.84 ms | 125.92 ms |
| Usage, minimum–maximum | 150.88–166.46 ms | 122.57–126.15 ms |
| first Scan page, 1,000 objects | 4.93 ms | 1.05 ms |

These numbers support keeping `Usage` as a scan for occasional inspection.
They do not make a full-folder count suitable for every latency-sensitive
request; persisted folder totals remain a separate feature needing a
workload that repays their write cost.

## Reads without the usual data cache

Linux reads the public API normally, after `POSIX_FADV_DONTNEED` on this
test's own 512 MiB file. `mincore` checks **zero of 131,072 pages resident**
before each cold read. Each is followed by a warm checked whole read.

| round | Linux page-cache cold | Linux warm |
|---:|---:|---:|
| 1 | 1.15 GB/s | 1.89 GB/s |
| 2 | 1.42 GB/s | 1.87 GB/s |
| 3 | 1.77 GB/s | 1.94 GB/s |

This proves eviction from the Linux guest's file page cache, not from a
drive controller or every layer below the WSL virtual disk.

Windows has a separate diagnostic: the engine's normal `Reader` and hash
verification use a test-only handle opened with `FILE_FLAG_NO_BUFFERING`,
and an aligned 64 KiB read buffer. This bypasses Windows data caching under
the documented [file buffering rules](https://learn.microsoft.com/en-us/windows/win32/fileio/file-buffering).
It is **not a cold run of the default public Open path**, which continues
to use cached I/O. No global cache flush or production I/O flag is added.

| round | Windows unbuffered diagnostic | Windows normal cached handle, same read loop |
|---:|---:|---:|
| 1 | 0.45 GB/s | 0.85 GB/s |
| 2 | 0.47 GB/s | 0.86 GB/s |
| 3 | 0.48 GB/s | 0.83 GB/s |

Both use the engine's SHA-256-checking Reader on the same 512 MiB object.
These Windows rates exclude opening the handles and use a different I/O
method from the Linux cache-eviction test; they are not a platform ranking.

## Validation, environment and reproduction

All measurements above passed. `task check` passed on Windows: formatting,
lint, shuffled tests for the full module, module tidiness, vulnerability
scan and the cgo-free import-size check. The Windows rename gates also
passed 25 repeated runs each. Linux passed
`go test -race -shuffle=on -count=1 ./blobs ./backup`, including the memory,
failure, recovery and backup gates. macOS was not run locally.

The environment is unchanged: Ryzen 7 7700, 8 cores / 16 logical processors,
Samsung 990 PRO NVMe; Windows 11 Pro 10.0.26200 on NTFS with Defender enabled;
Docker Desktop 29.6.2 / WSL2, 16 CPUs and about 15 GiB visible, a named ext4
volume, `golang:1.27`. Go 1.27.1 on both, modernc SQLite v1.59.0, WAL,
`synchronous=FULL`, 4 KiB pages. Measured binaries use `CGO_ENABLED=0`.
Only the scheduled test enables automatic maintenance. Platform timing
sessions run sequentially; an unrelated Redis container was running.

The before implementation is production commit
`15bdb3bf0465dac657953a6d8d32ac49ff440817` plus
[the preserved Windows publication patch](data/blobs-completion-2026-09-27-before.patch).
The after implementation is delivered with this report. To reproduce,
prepare separate before and after checkouts. Apply that patch to the before
checkout and copy the corrected `blobs/upload_test.go`, `blobs/load_test.go`
and `blobs/remaining_load_test.go` from the after checkout into it. Both
then compile the same streaming source and measurement functions.

Build both before timing, with `GOWORK=off` and `CGO_ENABLED=0`:

```powershell
Set-Location <before>
go test -c -o <binaries>/before.exe ./blobs
Set-Location <after>
go test -c -o <binaries>/after.exe ./blobs
$env:TINYSTORE_SPIKE = '1'
$env:TINYSTORE_BLOBS_DIR = '<measurement directory>'
$binaries = '<binaries>'
foreach ($variant in @('before', 'after', 'after', 'before')) {
  Write-Output "== $variant"
  & (Join-Path $binaries "$variant.exe") '-test.run=^Test(StreamingPutMeasured|MixedMeasured)$' '-test.v' '-test.timeout=20m'
  if ($LASTEXITCODE -ne 0) { throw "$variant failed" }
}
& '<binaries>/after.exe' '-test.run=^Test(ConcurrentLargeBoundedMeasured|MillionUsageMeasured|ScheduledScrubMeasured|UncachedReadMeasured)$' '-test.v' '-test.timeout=20m'
```

Build Linux binaries from the same checkouts with `GOOS=linux`, named
`before-linux.test` and `after-linux.test`. After Windows has finished:

```sh
docker run --rm -v <binaries>:/bins -v <volume>:/perf \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_BLOBS_DIR=/perf golang:1.27 sh -c '
  set -e
  for variant in before after after before; do
    echo == "$variant"
    /bins/$variant-linux.test -test.run="^Test(StreamingPutMeasured|MixedMeasured)$" -test.v -test.timeout=20m
  done
  /bins/after-linux.test -test.run="^Test(ConcurrentLargeBoundedMeasured|MillionUsageMeasured|ScheduledScrubMeasured|ColdReadMeasured)$" -test.v -test.timeout=20m'
```

The named measurement directory must exist. Unset `TINYSTORE_SPIKE` before
normal tests. The Windows unbuffered diagnostic and Linux eviction fixture
are platform-specific; neither silently substitutes a warm read for cold.

Raw output: [Windows paired runs](data/blobs-completion-2026-09-27-windows-paired.txt),
[Windows scale, memory and scheduled scrub](data/blobs-completion-2026-09-27-windows.txt),
[Windows uncached diagnostic](data/blobs-completion-2026-09-27-windows-uncached.txt),
[Linux paired runs and operational checks](data/blobs-completion-2026-09-27-linux.txt),
[Windows API comparison](data/blobs-completion-2026-09-27-windows-api.txt),
[Linux API comparison](data/blobs-completion-2026-09-27-linux-api.txt).
