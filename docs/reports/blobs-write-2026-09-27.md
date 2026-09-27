# Windows blob writes: fewer directory walks at once — 2026-09-27

The remaining streaming, mixed-load, memory, scale and maintenance questions
are covered by [the completion round](blobs-completion-2026-09-27.md).
The fixed-corpus measurements here remain the record of the Windows lock
change alone.

The [engine round](blobs-engine-2026-09-27.md) found a large gap between
Windows file writes through the engine and its prototype. This round profiles
the engine and changes one thing: a store on Windows admits one creation or
rename of an upload's file at a time. Writing bytes, hashing, file syncs,
directory syncs, reads and SQLite publication remain outside that lock.
Other operating systems take no lock.

## The result

Two runs of each binary, in order **base, candidate, candidate, base**, on
fresh stores. Each side writes exactly the same number of objects, paths and
seeded bytes. The table gives objects a second; both runs are shown.

| Windows fixture | objects | base | candidate |
|---|---:|---:|---:|
| 4 KiB, 128 callers, inline | 16,384 | 16,674 · 16,829 | 6,964 · 16,923 |
| 64 KiB, 1 caller | 128 | 220 · 217 | 218 · 216 |
| 64 KiB, 128 callers | 2,048 | 173 · 163 | 1,182 · 698 |
| 1 MiB, 64 callers | 1,024 | 153 · 153 | 410 · 893 |

The concurrent 64 KiB case improves by **4.3 to 6.8 times** when pairing
each candidate with its adjacent baseline. The 1 MiB case improves by
**2.7 to 5.8 times**. The single writer changes little. The first candidate's
inline run stalls, although inline writes do not enter the new lock; its
second run is beside both baselines. That unexplained outlier remains in
the result rather than being discarded.

Tail latency, milliseconds, the same Windows runs:

| fixture | base p99 | candidate p99 |
|---|---:|---:|
| 4 KiB, 128 callers | 20.1 · 18.7 | 227.7 · 18.7 |
| 64 KiB, 1 caller | 6.3 · 5.8 | 5.7 · 7.7 |
| 64 KiB, 128 callers | 884.4 · 1,255.7 | 158.3 · 449.3 |
| 1 MiB, 64 callers | 616.4 · 594.0 | 519.7 · 122.6 |

The engine still varies with the host and disk state. These are short fixed
workloads, not a sustained rate guarantee. No prototype number is used as
the baseline here.

Linux, the same binary sources and fixtures in a container on ext4:

| fixture | base | candidate |
|---|---:|---:|
| 4 KiB, 128 callers | 10,585 · 10,958 | 10,983 · 10,632 |
| 64 KiB, 1 caller | 101 · 102 | 104 · 101 |
| 64 KiB, 128 callers | 3,995 · 4,472 | 3,935 · 4,218 |
| 1 MiB, 64 callers | 1,652 · 1,791 | 1,701 · 1,753 |

The Linux change compiles to calls that do nothing. This run shows no gain
to attribute to it; it checks that the Windows choice does not impose its
serialization on Linux.

## What the profile found

The focused fixture is 2,048 new 64 KiB files from 128 callers. The base
trace took 11.70 seconds including setup, verification and cleanup; the
timed writes ran at 177 objects/s. The trace's syscall profile totals
1,421.17 **summed goroutine seconds**, which overlap and are not wall time.

| base syscall stack | summed delay | share |
|---|---:|---:|
| `os.Root.OpenFile` | 627.42 s | 44.15% |
| `os.Root.Rename` | 757.35 s | 53.29% |
| `os.File.Sync` | 20.34 s | 1.43% |

Together the rooted create and rename paths account for 97.44% of this
syscall delay. Opening intermediate directory handles and closing them
appear prominently below those calls. A separate block profile shows much
less waiting in the grouped SQLite writer. This points at concurrent
filesystem namespace operations, rather than at copying payload bytes.

The initial one-at-a-time candidate reached 1,110 objects/s. Allowing two
such calls at once instead reached 388/s. These were exploratory single
runs, not the paired table above. The candidate keeps one; it does not
replace `os.Root` with an unrooted pathname operation.

A second trace of the final candidate reached 1,257 objects/s, 1.76 seconds
including setup and cleanup. Total summed syscall delay fell to 16.01 s;
`createUpload` accounted for 0.69 s and `renameFile` for 0.87 s. The callers
now wait for their turn in Go instead of issuing all directory walks at
once. The evidence establishes a useful admission bound on this Windows
host; it does not isolate an NTFS, Defender or Go implementation cause.

## What changed and what still holds

[files.go](../../blobs/files.go) puts the upload's `Root.OpenFile` and each
attempt at `Root.Rename` behind a store-owned `nameCalls` lock. The Windows
implementation is a mutex; the other implementation has no state or wait.
A failed operation releases the lock, and rename retry backoff sleeps
outside it. No background goroutine or per-file retained state was added.

The file is still synced before its rename, the destination directory is
still synced before SQLite publication, and the grouped commit still
returns only after its transaction finishes. Unfinished bytes still live
in `uploads/`; the file layout, 16 KiB inline threshold, hashes, memory
reservations, recovery marks and public API do not change. A reader still
opens through `os.Root` and keeps what it opened through replacement.

Validation passed: `task check` on Windows (formatting, lint, all shuffled
tests, module checks, vulnerability scan and import-size check), plus
`go test -race -shuffle=on -count=1 ./blobs ./backup` in Linux. Those suites
include abrupt exit, failure during publication, readers through replacement,
memory limits, recovery and backup. Both platforms' paired measurements
also passed. macOS was not run locally.

The existing abrupt-exit tests cover process death before and after file
publication. They do not simulate loss of power. Other Windows filesystems,
many stores sharing a disk, mixed replacements and reads under sustained
load, and a production host remain unmeasured.

## Environment and reproduction

- Base production code: `15bdb3bf0465dac657953a6d8d32ac49ff440817`.
  Candidate: the implementation delivered with this report. Both builds
  include the same [put_compare_test.go](../../blobs/put_compare_test.go)
  and [load_test.go](../../blobs/load_test.go) measurement helpers.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Samsung 990 PRO NVMe.
  Windows 11 Pro 10.0.26200, NTFS, Defender real-time protection enabled.
- Go 1.27.1, `modernc.org/sqlite` v1.59.0, WAL, `synchronous=FULL`, 4 KiB
  pages, `CGO_ENABLED=0` for measured binaries. Stores use manual maintenance
  and no configured memory budget. Payloads come from ChaCha8 seeded by
  their size; keys are `item/1` through `item/<count>`.
- Linux binaries were cross-compiled with the same Go toolchain and executed
  in `golang:1.27`, Docker Desktop 29.6.2 on WSL2, 16 CPUs and about 15 GiB
  visible, files on a named ext4 volume. Windows and Linux timing sessions
  did not overlap. An unrelated Redis container was running.
- Each subtest creates its store before timing, writes the fixed corpus,
  then verifies three objects byte for byte and closes and removes the store
  outside the timed interval. There is no page-cache eviction or independent
  repetition beyond the two runs shown.

Prepare a base checkout at the hash above and a candidate checkout. Copy
the two measurement files into the base as well, without changing production
files. Build both before beginning the measurements. Windows PowerShell:

```powershell
$env:GOWORK = 'off'
$env:CGO_ENABLED = '0'
Set-Location <base>
go test -c -o <binaries>/base.exe ./blobs
Set-Location <candidate>
go test -c -o <binaries>/names.exe ./blobs
$env:TINYSTORE_SPIKE = '1'
$env:TINYSTORE_BLOBS_DIR = '<measurement directory>'
$binaries = '<binaries>'
foreach ($variant in @('base', 'names', 'names', 'base')) {
  Write-Output "== $variant"
  & (Join-Path $binaries "$variant.exe") '-test.run=^TestPutCompared$' '-test.v'
  if ($LASTEXITCODE -ne 0) { throw "$variant failed" }
}
```

For Linux, build each checkout with `GOOS=linux CGO_ENABLED=0`, naming its
binary `base-linux.test` or `names-linux.test`, then run sequentially:

```sh
docker run --rm -v <binaries>:/binfiles -v <volume>:/perf \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_BLOBS_DIR=/perf golang:1.27 sh -c '
  set -e
  for variant in base names names base; do
    echo == "$variant"
    /binfiles/$variant-linux.test -test.run=^TestPutCompared$ -test.v
  done'
```

Profile the focused Windows case separately from throughput comparisons:

```powershell
& '<binaries>/base.exe' '-test.run=^TestPutCompared/64KiB/128$' '-test.v' `
  '-test.trace=<profiles>/base.trace' `
  '-test.blockprofile=<profiles>/base.block' '-test.mutexprofile=<profiles>/base.mutex'
go tool trace -pprof=syscall <profiles>/base.trace > <profiles>/base.syscall
go tool pprof -top <binaries>/base.exe <profiles>/base.syscall
```

Raw outputs: [Windows paired runs](data/blobs-write-2026-09-27-windows.txt),
[Linux paired runs](data/blobs-write-2026-09-27-linux.txt),
[base syscall profile](data/blobs-write-2026-09-27-base-syscalls.txt),
[candidate syscall profile](data/blobs-write-2026-09-27-candidate-syscalls.txt).

The next independent candidate is the large `Put` transfer path: the earlier
round measured a material gap between its 16 KiB transfers and direct 64 KiB
`Create` writes. Any larger buffer must preserve progress with a 16 KiB store
budget. This round does not change that contract or claim to have measured
a buffer-only optimization.
