# What blobs' mechanics cost — 2026-09-27

The round [docs/blobs.md](../blobs.md) asks for before a blobs engine
exists: where an object's bytes are kept, a row of `blobs.db` or a file of
their own, and what each place costs to write, read and copy; what a file's
sync, its directory's and an upload's first row cost; files against a pack; a
million files; checked reads; a folder's totals; uploads at once and a large
one; a backup's entries; and what a reader keeps through a delete. Each ran in
the Linux container and on Windows. Nothing here is the engine: it is the
prototype `spike/blobs_*`, and it repeats in the repository the figures the
design's probe took outside it.

| Question | Linux, the container | Windows 11, NTFS | Proposal |
|---|---|---|---|
| the inline size | inline writes and reads faster up to 16 KiB from 1 to 128 callers; at 64 KiB a file reads faster at every concurrency and writes faster at 128 | inline writes faster up to 64 KiB and reads faster up to 16 KiB; at 64 KiB a file reads faster alone | inline up to 16 KiB |
| a reader through a delete | every change allowed; the reader reads every byte | through an `os.Root` a remove and `Root.Rename` are allowed, the reader reads every byte and the name is free at once; `os.Rename` over it is refused; through `os.Open` all are refused | open through an `os.Root`, replace with `Root.Rename` |
| a directory's sync after its file's | nothing alone; 40 % fewer files from 16 writers on, shared or not | 0.5 ms alone, nothing from 16 writers on | keep it |
| files against a pack | a pack that shares its syncs: 1.9, 1.4 and 1.3 times the files at 64 KiB, 256 KiB and 1 MiB from 128 writers; equal alone | files 1.2 to 1.5 times the pack from 8 writers on; the pack 1.6 to 1.8 times alone | files; no pack in the first version |
| a million files | 2,960 created a second throughout; linked in 12.8 s against 20.5 s to copy their bytes; zipped in 183 s; a restore holds 198 MiB of headers | 100,000 at about 500 a second; linked in 8.1 s against 5.1 s to copy 8 KiB each | files; a backup of a million small files takes minutes |
| an upload's first row | 3.8 ms more alone at 64 KiB, a quarter fewer at 16 callers, nothing at 4 MiB | 1.5 ms more alone | ids reserved in blocks instead |
| checked reads | SHA-256 at 2.38 GB/s and CRC-32C at 10.6 of one core; a whole read checked at 2.07 GB/s against 15.3 to 15.8 unchecked; a range unchecked as fast as the file | 2.42 and 11.9; a whole read checked at 1.7 GB/s against 6.5 to 7.0 unchecked; a range unchecked as fast as the file | whole reads checked, ranges not |
| copying `blobs.db` | `VACUUM INTO` at 0.69 GB/s, the online backup API at 0.86 | 0.42 and 0.37 | `VACUUM INTO`, as every engine copies |
| a folder's totals | kept for every segment of a path: 6, 14 and 47 % fewer writes at 128 callers for paths of 1, 4 and 16 segments; a scan counts 10⁵ objects in 8 ms | 37 and 47 % fewer at 4 and 16 segments; a scan the same | the user's: totals for the folders `Of` names, or a scan |
| uploads at once | 1 MiB each: 906 to 3,911 a second from 16 to 1024 at once, the heap at most 15 MiB | 335 to 613 a second, at most 27 MiB | 1024 slots; memory follows neither size nor count |
| a large upload's syncs | 4 GiB with one sync at its end: 2.9 s and a last sync of 0.54 s, 1.4 GiB dirty; synced every 256 MiB: 5.0 s, 0.5 ms and 257 MiB | one sync at its end: 24.4 s; every 256 MiB: 6.6 s | a sync every 256 MiB |
| a backup's entries | stored or deflated, the zip the same size and the times within the order the runs came in | stored 2.7 s and restored in 3.2; deflated 3.1 and 12.1 | stored |

## Where an object's bytes are kept

Objects of one size written from 1, 16 and 128 callers for three seconds, then
read back at random for three; inline, a row of `bodies` beside the object's
row, written in a grouped commit, read by one statement and hashed before its
first byte; a file, written, hashed as it goes, synced with its directory's
sync shared, then its row committed, read by a lookup and an open through an
`os.Root`, hashed whole. Objects a second.

Linux, the container:

| size | callers | inline `Put` | file `Put` | inline `Open` | file `Open` |
|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1 | 163 | 85 | 90,154 | 35,087 |
| 4 KiB | 16 | 1,531 | 654 | 109,907 | 47,944 |
| 4 KiB | 128 | 6,994 | 3,339 | 83,912 | 53,360 |
| 16 KiB | 1 | 160 | 150 | 35,146 | 26,287 |
| 16 KiB | 16 | 2,299 | 1,033 | 66,891 | 51,420 |
| 16 KiB | 128 | 6,515 | 5,226 | 52,598 | 51,358 |
| 64 KiB | 1 | 336 | 150 | 10,022 | 15,110 |
| 64 KiB | 16 | 1,467 | 1,134 | 25,313 | 43,680 |
| 64 KiB | 128 | 2,446 | 5,363 | 25,722 | 37,022 |
| 256 KiB | 1 | 238 | 143 | 3,113 | 5,419 |
| 256 KiB | 16 | 545 | 1,015 | 9,418 | 31,671 |
| 256 KiB | 128 | 708 | 4,408 | 7,712 | 24,471 |

Windows 11:

| size | callers | inline `Put` | file `Put` | inline `Open` | file `Open` |
|---:|---:|---:|---:|---:|---:|
| 4 KiB | 1 | 664 | 267 | 76,395 | 12,535 |
| 4 KiB | 16 | 5,633 | 1,210 | 108,985 | 19,272 |
| 4 KiB | 128 | 13,090 | 1,592 | 84,479 | 18,532 |
| 16 KiB | 1 | 660 | 252 | 22,647 | 10,579 |
| 16 KiB | 16 | 4,113 | 1,217 | 58,025 | 18,130 |
| 16 KiB | 128 | 6,760 | 1,108 | 54,484 | 20,711 |
| 64 KiB | 1 | 606 | 289 | 6,567 | 9,277 |
| 64 KiB | 16 | 2,138 | 1,052 | 19,874 | 17,577 |
| 64 KiB | 128 | 2,220 | 1,600 | 22,206 | 15,371 |
| 256 KiB | 1 | 399 | 230 | 2,909 | 4,446 |
| 256 KiB | 16 | 574 | 920 | 7,444 | 12,832 |
| 256 KiB | 128 | 634 | 1,005 | 6,580 | 15,544 |

- **Up to 16 KiB inline wins everything** on both systems: its write shares a
  commit with the writes beside it, where a file pays its sync and its
  directory's, and its read is one statement, where a file is a lookup and an
  open.
- **At 64 KiB a read of a body is a chain of sixteen overflow pages** read
  whole: 10,022 a second alone on Linux against a file's 15,110, and 25,313
  against 43,680 from 16 callers. From 128 callers the inline writes also
  lose on Linux, 2,446 against 5,363, where a commit carries 8 MiB of bodies
  through the WAL. On Windows inline still writes faster at 64 KiB.
- **What inline costs `blobs.db`**: the bodies' pages hold 88 % payload at 4
  KiB, 97 % at 16 and 99 % at 64; the write-ahead log peaked at 5 to 10 MiB;
  `VACUUM INTO` copied the file at 0.42 to 0.58 GB/s on Linux and 0.37 to
  0.48 on Windows.

The draft proposed 64 KiB; the round says 16, the last size measured where
inline wins on both systems at every concurrency. 32 KiB was not measured.

## A reader through a delete

A 1 MiB file read halfway, its name then removed, replaced or linked, and the
reader reading on.

| reader opened by, then | Linux | Windows |
|---|---|---|
| `os.Open`, `os.Remove` or `Root.Remove` | allowed; the reader reads every byte; the name is free | refused: the file is in use; the name keeps the old bytes |
| `os.Open`, `Root.Rename` or `os.Rename` over it | allowed; the name holds the new bytes | refused: access is denied |
| `Root.Open`, `os.Remove` or `Root.Remove` | allowed; every byte read; the name free | the same as Linux |
| `Root.Open`, `Root.Rename` over it | allowed; every byte read; the name holds the new bytes | the same as Linux |
| `Root.Open`, `os.Rename` over it | allowed | refused: access is denied |
| `Root.Open`, `Root.Link`, then `Root.Remove` | allowed; the link reads every byte | the same as Linux |

The draft's table stands: on Windows only a file opened through an `os.Root`
shares its deletion, and only `Root.Rename` replaces it.

## Syncs

**A directory's sync after its file's**, 64 KiB files into one directory: the
file synced alone, its directory synced after it, and a directory sync shared
by the files that finish together. Files a second.

| writers | Linux: the file's | and its directory's | shared | Windows: the file's | and its directory's | shared |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 310 · 296 | 303 · 303 | 302 · 300 | 694 · 684 | 499 · 502 | 501 · 497 |
| 16 | 2,566 · 2,571 | 1,532 · 1,566 | 1,381 · 1,363 | 1,453 · 1,516 | 1,482 · 1,458 | 1,662 · 1,723 |
| 128 | 16,341 · 14,729 | 9,174 · 8,799 | 9,141 · 9,192 | 1,326 · 1,335 | 1,316 · 1,234 | 1,256 · 1,263 |

- **On Linux a directory's sync is free alone and costs 40 % of the files from
  16 writers on**, shared among them or not. That each is one more commit of
  ext4's journal is the explanation this round does not separate. ext4 in its
  ordered mode also makes a new file's name durable with the file's own sync,
  which no standard promises, so the engine keeps the directory's.
- **On Windows it costs half a millisecond alone and nothing under load**, and
  sharing it gains a little from 16 writers.
- The round's first run of this table, at the container's 6 ms syncs, gave
  the same ratios on Linux (7,673, 4,258 and 4,442 from 128 writers); its
  first Windows run measured the file's sync alone at 38 a second, 29 ms
  each, right after the writers' test had removed its files, and both reruns
  at 684 and 694.

**Files against a pack.** A file each, synced with its directory's sync
shared, against one pack all writers append to, which shares a sync among the
appends that wait for it. Objects a second:

| size | writers | Linux files | Linux pack | Windows files | Windows pack |
|---:|---:|---:|---:|---:|---:|
| 64 KiB | 1 | 308 | 316 | 422 | 772 |
| 64 KiB | 8 | 754 | 1,141 | 928 | 789 |
| 64 KiB | 128 | 7,151 | 13,832 | 1,133 | 755 |
| 256 KiB | 1 | 252 | 264 | 466 | 724 |
| 256 KiB | 8 | 682 | 868 | 830 | 738 |
| 256 KiB | 128 | 4,126 | 5,928 | 1,053 | 724 |
| 1 MiB | 1 | 172 | 192 | 358 | 604 |
| 1 MiB | 8 | 510 | 576 | 764 | 625 |
| 1 MiB | 128 | 1,204 | 1,513 | 948 | 615 |

One writer, the mean an object over 500:

| size | Linux: a file and its directory synced | a pack synced each | a pack synced every 32 | Windows: a file | synced each | every 32 |
|---:|---:|---:|---:|---:|---:|---:|
| 4 KiB | 5,707 µs | 5,906 | 181 | 2,039 | 1,363 | 42 |
| 64 KiB | 6,037 | 5,785 | 215 | 1,994 | 1,303 | 66 |
| 256 KiB | 6,437 | 5,967 | 431 | 2,120 | 1,456 | 155 |
| 1 MiB | 5,744 | 4,114 | 732 | 2,995 | 1,692 | 453 |

- **A pack gains what a shared sync gains**, and only with objects arriving
  together: on Linux 1.9 times the files at 64 KiB from 128 writers, 1.3 at 1
  MiB, less than the twice the design asked of it.
- **On Windows the shared pack loses from 8 writers on**, 615 to 789 objects
  a second whatever the writers, as the probe found: its appends wait for its
  flush.
- **The container's syncs were slower than the probe's**: a file with its
  directory took about 6 ms alone against the probe's 3; the first writers'
  test ran at 3.2 ms. The disk under WSL varies between runs; each table
  compares sides from one run.

**An upload's first row.** A row of `uploads` committed before a file's first
byte, so that the next open finds what a crash left, against none. `Put`s a
second:

| size | callers | Linux without | with | Windows without | with |
|---:|---:|---:|---:|---:|---:|
| 64 KiB | 1 | 151 | 93 | 282 | 144 |
| 64 KiB | 16 | 1,095 | 806 | 332 | 1,023 |
| 256 KiB | 1 | 142 | 95 | 279 | 194 |
| 256 KiB | 16 | 1,123 | 717 | 1,083 | 978 |
| 1 MiB | 1 | 121 | 88 | 222 | 176 |
| 1 MiB | 16 | 922 | 619 | 903 | 778 |
| 4 MiB | 1 | 78 | 72 | 143 | 115 |
| 4 MiB | 16 | 430 | 432 | 564 | 276 |

The row is a commit before the first byte: 3.8 ms more at the median alone on
Linux at 64 KiB, 1.5 ms on Windows, and nothing that stands out at 4 MiB on
Linux. Two Windows cells at 16 callers stand out, 64 KiB without the row and 4
MiB with it, each with a stall: a p99 of 202 and 98 ms. Ids reserved in blocks, as jobs reserves its own,
and files above the committed mark removed at open would save the commit.

**A large upload's syncs.** 4 GiB in writes of 64 KiB, hashed as it goes,
synced once at its end, every 256 MiB or every 64 MiB:

| synced | Linux: written | the last sync | dirty at most | Windows: written | the last sync |
|---|---:|---:|---:|---:|---:|
| at its end | 2.9 s | 541 ms | 1,421 MiB | 24.4 s | 723 ms |
| every 256 MiB | 5.0 s | 0.5 ms | 257 MiB | 6.6 s | 0.5 ms |
| every 64 MiB | 5.6 s | 0.6 ms | 66 MiB | 6.7 s | 3.3 ms |

Windows throttles a writer whose file holds gigabytes unwritten: one sync at
the end made the upload 3.7 times slower there, where Linux wrote it faster
and made its `Commit` wait half a second behind 1.4 GiB of dirty pages. A
sync every 256 MiB bounds both.

## A million files

A million files of 8 KiB from 64 writers, each synced with its directory's
sync shared, 4,096 a directory; then hard links of all of them, as a snapshot
makes, the same bytes copied into one file, a zip of them stored, and a
restore of the zip.

| | Linux, 10⁶ files | Windows, 10⁵ files |
|---|---:|---:|
| created a second, the first tenth | 2,937 | 686 |
| the last tenth | 2,964 | 497 |
| on the disk, for 7,813 MiB and 781 MiB of bytes | 7,836 MiB | 805 MiB |
| hard links, all of them | 12.8 s | 8.1 s |
| the same bytes copied into one file | 20.5 s | 5.1 s |
| a zip of them, stored | 183 s | 5.7 s |
| the zip's directory opened, and the heap it holds | 139 ms, 198 MiB | 20 ms, 19 MiB |
| the zip restored | 40.7 s | 15.6 s |

- **Directories that fill do not slow the files**: the last hundred thousand
  came as fast as the first, 4,096 names a directory.
- **A snapshot of links is cheaper than copying the bytes on Linux** even at
  8 KiB a file; on Windows links cost 81 µs each, which a copy of 8 KiB beats
  and one of the inline bound and more does not.
- **A backup's zip of a million files takes three minutes on Linux**, 183 µs
  an entry, and a restore holds 198 MiB of headers: the cost a pack would save,
  and the one question that could bring packs before a million files do.

## Checked reads and copies

**Hashing a byte**, 256 MiB of random bytes on one core:

| | Linux | Windows |
|---|---:|---:|
| SHA-256 | 2.38 GB/s | 2.42 GB/s |
| CRC-32C | 10.64 | 11.92 |
| deflate, fastest | 12.60 | 13.50 |
| deflate, default | 6.72 | 6.41 |

**Serving a 1 GiB object** through `http.ServeContent`, whole and in ranges of
4 MiB at random offsets, from the page cache: the file as it is; a reader that
hashes a read that runs from the first byte to the last and checks it at the
end, and leaves a range alone; and one that checks every 64 KiB block against
its CRC-32C.

| reader | Linux whole | ranges a second | Windows whole | ranges a second |
|---|---:|---:|---:|---:|
| the file as it is | 15.80 · 15.34 GB/s | 3,406 · 3,504 | 6.98 · 6.46 GB/s | 1,530 · 1,350 |
| a whole read checked by its SHA-256, a range not | 2.07 · 2.07 | 3,415 · 3,513 | 1.70 · 1.68 | 1,252 · 1,255 |
| each 64 KiB block checked by its CRC-32C | 6.44 · 6.62 | 986 · 988 | 3.29 · 3.65 | 674 · 718 |

- **A checked whole read runs at the hash's speed**, 2 GB/s of one core: a
  17 GB film served whole spends eight seconds of a core on it, and a 10 GbE
  link does not reach that rate.
- **A range left unchecked costs nothing**, and one checked by its blocks'
  CRC-32C costs 72 % of the ranges on Linux and half on Windows, from the page
  cache; a disk's latency would hide more of it.
- These are the second and third runs; the round's first measured a reader
  that went on hashing ranges and stopped checking after the server read the
  first 512 bytes to find the type, both corrected before them.

**Copying `blobs.db`**, a gigabyte of 64 KiB bodies: `VACUUM INTO`, which
rebuilds the file, at 0.69 GB/s on Linux and 0.42 on Windows; the driver's
online backup API, which copies pages, at 0.86 and 0.37. Neither is worth a
second path.

## A folder's totals

Rows of objects written with a folder's totals kept for every segment of the
path, in the same grouped transaction, against none; paths of 1, 4 and 16
segments. Writes a second at 128 callers (alone, a commit's sync hides both):

| segments | Linux without | with | Windows without | with |
|---:|---:|---:|---:|---:|
| 1 | 21,168 | 19,838 | 25,039 | 41,535 |
| 4 | 16,093 | 13,853 | 27,039 | 17,067 |
| 16 | 14,894 | 7,913 | 16,489 | 8,724 |

A `Usage` scanned from the objects' rows instead: 0.1 ms over a thousand, 0.8
ms over ten thousand, 8.0 over a hundred thousand and 88.7 over a million on
Linux; 0.1, 0.8 and 8.5 on Windows. The Windows cell of one segment without
totals had a p99 of 51 ms, a stall.

## Uploads at once

Uploads of 1 MiB, 4 GiB in all, and of 64 MiB, each streamed through a 64 KiB
buffer into a file of its own, synced with its directory, its row committed:

| uploads at once | size | Linux a second | heap at most | files open at most | Windows a second | heap at most |
|---:|---:|---:|---:|---:|---:|---:|
| 16 | 1 MiB | 906 | 4 MiB | 16 | 335 | 5 MiB |
| 64 | 1 MiB | 1,954 | 6 MiB | 63 | 568 | 9 MiB |
| 256 | 1 MiB | 3,332 | 9 MiB | 217 | 613 | 27 MiB |
| 1024 | 1 MiB | 3,911 | 15 MiB | 246 | 413 | 23 MiB |
| 16 | 64 MiB | 28 | 8 MiB | 16 | 20 | 13 MiB |
| 64 | 64 MiB | 21 | 9 MiB | 64 | 22 | 13 MiB |

Memory follows neither an object's size nor the uploads at once, and a file is
open only while it is written and synced: 246 at most of 1024 uploads.

## A backup's entries

A 4 GiB object and a thousand JPEGs of about 180 KB into one zip, entries
stored, then deflated, each zip restored:

| | Linux backup | restore | Windows backup | restore |
|---|---:|---:|---:|---:|
| stored | 3.7 s | 3.7 s | 2.7 s | 3.2 s |
| deflated | 2.5 s | 2.9 s | 3.1 s | 12.1 s |

Deflate saved no byte, 4,276.4 MiB against 4,276.0; Go 1.27 finds random
bytes and JPEGs incompressible and stores them at 6 to 13 GB/s, so the times
follow the order of the runs and what the page cache held, not the method.
Storing is the simpler promise.

## Environment and reproduction

- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Samsung 990 PRO NVMe.
- Linux: Docker Desktop 29.6.2 on WSL2, 16 CPUs and 15 GiB visible;
  `golang:1.27`, go1.27.1 linux/amd64; the files on a named ext4 volume.
- Windows: Windows 11 Pro 10.0.26200, NTFS on the same disk, Defender's
  real-time protection on; go1.27.1 windows/amd64.
- `modernc.org/sqlite` v1.59.0, `synchronous=FULL`, WAL, 4 KiB pages.
- The prototype at the commit that adds this report; one run of each
  measurement, the serving and directory syncs twice.
- Loads of three seconds; a million files on Linux, a hundred thousand on
  Windows; large objects of 4 GiB.

```sh
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_BLOBS_DIR=/perf -w /src golang:1.27 \
  go test ./spike -run '^TestBlobs' -v -count=1 -timeout 120m

cd <repo>
TINYSTORE_SPIKE=1 TINYSTORE_BLOBS_DIR=<a directory on the disk> TINYSTORE_BLOBS_FILES=100000 \
  GOWORK=off go test ./spike -run '^TestBlobs' -v -count=1 -timeout 120m
```

## What this settles and leaves

Settled for the first version, unless the user decides otherwise: two places,
inline up to 16 KiB and a file above it; files opened through an `os.Root` and
replaced with `Root.Rename`; a directory synced after its file; whole reads
checked by the SHA-256 and ranges not; `blobs.db` copied with `VACUUM INTO`; a
large upload synced every 256 MiB; backup entries stored.

For the user, with the round's numbers beside the draft's questions:

- **The inline size**: 16 KiB, measured; 64 KiB, the draft's, loses reads on
  both systems.
- **A folder's totals**: kept for every folder a path has, they cost up to
  half the writes of deep paths under load; kept only for the folders `Of`
  names, they cost what 1 to 4 segments cost, 6 to 37 %; a scan answers ten
  thousand objects in a millisecond.
- **An upload's first row, or ids in blocks**: the row is a commit before the
  first byte.
- **Backups of a million small files**: three minutes and 200 MiB of headers,
  or a pack.

Not measured: the scrub's pace beside readers, 32 KiB inline, a cold page
cache, and the production host.
