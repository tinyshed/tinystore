# kv after its review — 2026-09-27

Another agent reviewed kv on 27 September and found seven defects, KV-01 to
KV-07, and four leads on performance, KV-P1 to KV-P4. This round fixes the
seven, follows three of the leads, and measures what the fixes cost against the
kv the review read, on identical input in one session, alternating the versions.

## For a reader in a hurry

| | what the review found | what holds now | commit |
|---|---|---|---|
| KV-01 | a Clear of `LoseAtMost` counters that failed had already dropped their memory: an acknowledged `Add` was lost | memory lets go only after the Clear commits; a failed commit is `ErrOutcomeUnknown` and lets go as a crash would | `5f568ad` |
| KV-02 | a `Scan` inside `Tx` waited for memory that a `Set` held while it waited for the transaction's writer: a deadlock | a call inside `Tx` or `View` takes only the memory that is free, and is `ErrLimit` past it | `823521a`, `dd19793` |
| KV-03 | 24 `Set`s waiting for memory had encoded their values, 25 MB the budget never saw | a write takes its turn and its memory before it encodes; `Get` and `Take` reserve before they read | `5f9af0f`, `dd19793` |
| KV-04 | a `Scan` page held 4.5 MB of values against its 4 MiB | a page ends before the value that would pass it | `c840611` |
| KV-05 | a `Take` whose value no longer decoded had deleted it | it decodes inside its savepoint: `ErrCorrupt`, and the value stays | `72e952e` |
| KV-06 | counters read for a change that failed were held outside the bound | the bound counts every counter held, each place taken before the file is read | `bc927c8` |
| KV-07 | a named `float32` quieted a NaN that signals | a float's bits are read and written where they lie | `df342bb` |

Each fix came with a test that fails on the code the review read, and the
review's own seven probes, run against the fixed code, pass. The same memory
rules reached jobs (`113b06e`), which encoded before it reserved and did not
count reads inside `Tx`.

What the fixes cost, on Windows, the median of three alternated runs:

| | reviewed `c557304` | fixed `0f65fcb` |
|---|---:|---:|
| an `Add` to a counter memory holds, 1 goroutine | 199 ns, 6 allocations | 130 ns, 2 allocations |
| the same, 8 processors | 166 ns | 59 ns |
| the same, 16 processors | 167 ns | 64 ns |
| an `Add` to a counter memory never held, 1 goroutine | 13.7 µs | 12.7 µs |
| a `Set` overwriting one of 20,000 keys at random, 512 goroutines | 42.6 µs | 49.2 µs |

The hot counter went 1.5 to 2.8 times faster because a gate is now one atomic
add rather than a lock every call of every engine queued on (`c785a32`), and a
call without options or error allocates nothing (`c8d15d7`). The random
`Set`s spread by up to 29 % between runs of one version, so the two medians
say only that the write path did not change beyond that; see
[the runs](#the-runs).

## The leads

**KV-P1, Clear marks.** Every statement that finds live rows tested every mark
of the bucket by substring, so a point read beside a thousand marks took
147 µs. The test now asks whether the bucket has a mark at all, one seek, and
then looks the root and each branch above the row up by its prefix, the
prefixes' lengths packed ten bits a level into one integer; a row deeper than
six owners is tested by substring as before (`fe29b62`). Binding the lengths
one to a parameter cost a read 1.7 µs where the substring cost 0.8: nine
parameters bound through `database/sql` cost more than the test they feed.
Packed into one, the test costs 0.6 µs:

| a counter's read, one statement on one reader, no marks | per read |
|---|---:|
| no test of hidden rows | 5.20 µs |
| the substring, as the review found it | 5.98 µs |
| a length a parameter, nine parameters | 6.95 µs |
| the lengths packed in one parameter | 5.77 µs |

A `Get` through kv, the context one that cannot end, and an `Add` that
reads its counter's row first, the median of three alternated runs:

| a point read beside the marks of other branches | reviewed `c557304` | fixed `0f65fcb` | lookups `fe29b62` |
|---|---:|---:|---:|
| 0 marks | 6.2 µs | 6.0 µs | 6.9 µs |
| 10 marks | 8.3 µs | 9.6 µs | 8.0 µs |
| 100 marks | 23.1 µs | 24.9 µs | 9.3 µs |
| 1000 marks | 178.2 µs | 146.9 µs | 10.8 µs |
| an `Add` to a counter memory never held, which reads the file | 15.4 µs | 14.5 µs | 15.5 µs |

A thousand marks cost a read 10.8 µs rather than 146.9. With no marks the
three medians lie inside one spread, every run of all three between 5.8 and
7.3 µs, so the engine cannot tell them apart; the statement alone, above, puts
the packed test 0.2 µs under the substring. The lookups allocate twice more a
read, 1,824 bytes against 1,696, where a statement's arguments grow by one.

**KV-P2, the writer's page cache.** Nothing changed: the choice is the
user's, since the cache is memory the writer holds for good. Random
overwrites through `Set`, 512 goroutines, with the lookups of KV-P1:

| writer cache | 20,000 keys: Sets a second, misses a Set | 200,000 keys |
|---:|---:|---:|
| 1 MiB, today's | 17,985, 1.18 | 8,182, 2.96 |
| 4 MiB | 21,781, 0.003 | 9,483, 1.73 |
| 8 MiB | 20,239, 0.003 | 10,272, 1.19 |
| 16 MiB | 20,604, 0.003 | 10,906, 0.23 |

A file whose pages the writer touches fit in 4 MiB at 20,000 keys and in
16 MiB at 200,000; past that the cache helps less and costs the same.

**KV-P3, hot counters**, is the gate and the allocations above.

**KV-P4**, writing the revision once a group, the review itself measured at
1 % and was not followed.

## Found on the way

- A point read with a context that can end costs about twice one without:
  `database/sql` starts a goroutine for every statement to watch the context,
  11.2 µs a `Get` of the fixed kv against 6.0, beside no marks. A lookup that cannot wait on anything but a
  reader might run its statement without the caller's context.
- `Maintain` deletes up to ten batches for each mark in one run, so a
  thousand marks of 10,000 rows each make one run of thousands of
  transactions; the writer is let go between them, but the run is long.
- Expiry deletes at most 100,000 rows a minute; a program creating keys with
  a TTL faster than that grows a backlog, as the review said.

## The runs

Every run, in ns an operation, in the order they ran; the medians above are
the middle of each three.

```text
# reviewed, fixed and the lookups bound a parameter each, all at 16 processors but the counters' -cpu
BenchmarkChangesInMemory/held                                fixes      134 130 128
BenchmarkChangesInMemory/held                                p1         174 153 176
BenchmarkChangesInMemory/held                                reviewed   213 195 199
BenchmarkChangesInMemory/held-16                             fixes      65 64 64
BenchmarkChangesInMemory/held-16                             p1         51 59 49
BenchmarkChangesInMemory/held-16                             reviewed   171 167 164
BenchmarkChangesInMemory/held-8                              fixes      59 53 61
BenchmarkChangesInMemory/held-8                              p1         52 55 59
BenchmarkChangesInMemory/held-8                              reviewed   115 179 166
BenchmarkChangesInMemory/new                                 fixes      12468 12659 13300
BenchmarkChangesInMemory/new                                 p1         19386 15641 18199
BenchmarkChangesInMemory/new                                 reviewed   13681 13499 13815
BenchmarkChangesInMemory/new-16                              fixes      7778 7242 7918
BenchmarkChangesInMemory/new-16                              p1         5589 8695 8175
BenchmarkChangesInMemory/new-16                              reviewed   9975 9875 9896
BenchmarkChangesInMemory/new-8                               fixes      8067 7805 8023
BenchmarkChangesInMemory/new-8                               p1         8698 7982 7048
BenchmarkChangesInMemory/new-8                               reviewed   8310 7997 8085
BenchmarkGetBesideClearMarks/marks=0-16                      fixes      10988 11260 11205
BenchmarkGetBesideClearMarks/marks=0-16                      p1         8483 9381 12435
BenchmarkGetBesideClearMarks/marks=0-16                      reviewed   12051 11947 8653
BenchmarkGetBesideClearMarks/marks=10-16                     fixes      15092 15022 15775
BenchmarkGetBesideClearMarks/marks=10-16                     p1         15974 13829 15162
BenchmarkGetBesideClearMarks/marks=10-16                     reviewed   13499 13171 13595
BenchmarkGetBesideClearMarks/marks=100-16                    fixes      28828 27587 29465
BenchmarkGetBesideClearMarks/marks=100-16                    p1         17491 18679 10718
BenchmarkGetBesideClearMarks/marks=100-16                    reviewed   30112 30769 31919
BenchmarkGetBesideClearMarks/marks=1000-16                   fixes      162650 158534 164274
BenchmarkGetBesideClearMarks/marks=1000-16                   p1         15792 18295 14018
BenchmarkGetBesideClearMarks/marks=1000-16                   reviewed   207582 222308 165258
BenchmarkRandomSetsByWriterCache/keys=20000/cache=16MiB-16   fixes      40231 40629 44591
BenchmarkRandomSetsByWriterCache/keys=20000/cache=16MiB-16   p1         48754 52498 46239 48831 48314 46128
BenchmarkRandomSetsByWriterCache/keys=20000/cache=16MiB-16   reviewed   48889 53063 58231
BenchmarkRandomSetsByWriterCache/keys=20000/cache=1MiB-16    fixes      49232 53294 45145
BenchmarkRandomSetsByWriterCache/keys=20000/cache=1MiB-16    p1         63465 45772 48463 54228 58826 57047
BenchmarkRandomSetsByWriterCache/keys=20000/cache=1MiB-16    reviewed   42242 42602 54626
BenchmarkRandomSetsByWriterCache/keys=20000/cache=4MiB-16    p1         45911 53917 45040
BenchmarkRandomSetsByWriterCache/keys=20000/cache=8MiB-16    p1         59454 48305 49409
BenchmarkRandomSetsByWriterCache/keys=200000/cache=16MiB-16  p1         94912 90980 91692
BenchmarkRandomSetsByWriterCache/keys=200000/cache=1MiB-16   p1         130831 122219 116982
BenchmarkRandomSetsByWriterCache/keys=200000/cache=4MiB-16   p1         177416 105448 104038
BenchmarkRandomSetsByWriterCache/keys=200000/cache=8MiB-16   p1         104463 97352 91724
# reviewed, fixed and the lookups packed
BenchmarkChangesInMemory/new                                 fixes2     14905 13401 14475
BenchmarkChangesInMemory/new                                 p2         14497 15780 15536
BenchmarkChangesInMemory/new                                 reviewed2  12179 16343 15354
BenchmarkGetBesideClearMarks/marks=0-16                      fixes2     7056 5983 5799
BenchmarkGetBesideClearMarks/marks=0-16                      p2         6310 7298 6856
BenchmarkGetBesideClearMarks/marks=0-16                      reviewed2  6037 6195 7179
BenchmarkGetBesideClearMarks/marks=10-16                     fixes2     10368 7262 9563
BenchmarkGetBesideClearMarks/marks=10-16                     p2         7520 10051 8003
BenchmarkGetBesideClearMarks/marks=10-16                     reviewed2  7241 9748 8350
BenchmarkGetBesideClearMarks/marks=100-16                    fixes2     23486 25626 24852
BenchmarkGetBesideClearMarks/marks=100-16                    p2         9106 9336 9650
BenchmarkGetBesideClearMarks/marks=100-16                    reviewed2  24142 21507 23070
BenchmarkGetBesideClearMarks/marks=1000-16                   fixes2     146901 188625 144223
BenchmarkGetBesideClearMarks/marks=1000-16                   p2         10816 8564 11015
BenchmarkGetBesideClearMarks/marks=1000-16                   reviewed2  184091 138771 178151
```

## Environment

AMD Ryzen 7 7700, 8 cores and 16 threads, Windows 11 Pro 10.0.26200, Go
1.27.1 windows/amd64 with `CGO_ENABLED=0`, modernc.org/sqlite v1.59.0 built
with `ENABLE_STAT4`, WAL and `synchronous=FULL`, 4 KiB pages, every file in the
temporary directory of the system drive. The host was otherwise idle; a build
running on it earlier was waited out, since it tripled every timing.

## Commands

The benchmarks are `BenchmarkChangesInMemory` in `kv/memory_test.go`,
`BenchmarkRandomSetsByWriterCache` in `kv/write_test.go` and
`BenchmarkGetBesideClearMarks` in `kv/read_test.go`, at `d345af0`. They open
their store themselves, so that the same code runs against an earlier kv: for
the reviewed and the fixed version, the three and the helpers they call
(`benchmarkCounters`, `openBenchmarkStore`, `overwriteWithWriterCache`,
`overwrittenKeysOf`, `reportWriterCache`, `writersPerCPU`) go into one file of
package kv given to `go test -overlay`.

```sh
git -C <repo> worktree add <reviewed> c557304
git -C <repo> worktree add <fixed> 0f65fcb
go test -c -o reviewed.test.exe -overlay <overlay adding the benchmarks> ./kv    # in <reviewed>
go test -c -o fixed.test.exe -overlay <overlay replacing kv/read_test.go> ./kv  # in <fixed>
go test -c -o lookups.test.exe ./kv                                           # in <repo> at d345af0
<each>.test.exe -test.run '^$' -test.bench '^BenchmarkChangesInMemory$' -test.cpu 1,8,16 -test.benchtime 2s
<each>.test.exe -test.run '^$' -test.bench '^BenchmarkRandomSetsByWriterCache$' -test.benchtime 50000x
<each>.test.exe -test.run '^$' -test.bench '^BenchmarkGetBesideClearMarks$' -test.benchtime 2s
TINYSTORE_SPIKE=1 TINYSTORE_KV_SECONDS=9 go test ./spike -run 'TestKVClearMarks' -v -count=1
```

The single-statement timings of KV-P1 come from a test of one read on one
reader, the statement written four ways, a second each, three times over.
