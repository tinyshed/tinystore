# What the built sqldb costs — 2026-09-28

The engine's round, after [the mechanics round](sqldb-mechanics-2026-09-28.md)
measured the prototype: the built `sqldb` on the paths the prototype took, in
the same run as the prototype's own paths, and against `database/sql` and
modernc as a program that opens the file itself uses them.

| Question | Windows | The container | Decision |
|---|---|---|---|
| a point read by id, a million notes, 1/8/64 goroutines | `One[Note]` 58,688 to 60,234 / 123,961 to 126,540 / 111,726 to 118,809; the statement scanned by hand 67,930 to 68,343 / 132,929 to 134,778 / 124,876 to 126,658 | 95,281 / 286,430 / 301,711; by hand 106,045 / 309,693 / 329,937 | `One` keeps 86 to 95 % of the floor, typed |
| against the transaction compiling each call, sqldb's path before | 28,597 to 41,019 / 93,040 to 95,906 / 91,166 to 92,555 | 62,385 / 163,175 / 179,221 | `One` serves 23 to 111 % more |
| against `database/sql` as a program opens it, 100,000 notes | `One` 80,329 to 80,591 / 206,160 to 207,521 / 189,461 to 190,389; the pool 64,589 to 64,747 / 111,387 to 112,094 / 73,913 to 74,012 | 101,468 / 300,583 / 313,010; the pool 78,517 / 153,322 / 102,174 | 24 to 206 % more reads |
| writes from 1/8/64/512 goroutines, against `database/sql` | grouped 670 to 684 / 3,953 to 4,288 / 12,815 to 13,162 / 18,054 to 18,538; the pool 665 to 671 / 637 to 646 / 389 to 404 / 388 to 393, 217 and 292 `SQLITE_BUSY` at 512 | 366 / 1,200 / 6,435 / 23,908; the pool 352 / 178 / 188 / 205, 23 and 140 `SQLITE_BUSY` | grouped commits, no lock errors |
| a row decoded, 200,000 | `All` 1.28 to 1.34 µs, `Each` 1.18 to 1.25, by hand 1.25 to 1.35; 12.7 allocations a row against 17.7 | 1.35 to 1.39, 1.19 to 1.20, 1.19 to 1.21; 12.6 against 17.6 | a plan decodes as by hand |
| `Insert` against the same insert returning nothing, 512 goroutines | 31,293 to 31,989 against 32,981 to 33,602 | 29,931 against 31,344 | `Insert` returns only what the database generated |
| `Open` checking the schema | 5.16 to 5.42 ms against 3.68 to 4.62 without | 2.76 against 1.95 | 0.8 to 1.5 ms an `Open` |
| a cache door against the plain cache, 1024 texts | compiled 87.4 to 87.5 % both; 2.4 % fewer to 6.2 % more reads | 87.5 % both; 3.8 % more | not built |
| what linking sqldb costs | 256 KiB of the probe's 8,676 over an empty program | | |

## Environment and reproduction

- The tree of the commit that adds this report; the machine of the
  mechanics round, AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1
  GiB, Samsung 990 PRO NVMe, Windows 11 Pro 10.0.26200, go1.27.1; the container
  as there: Docker Desktop 29.6.2 on WSL2, kernel
  6.18.33.2-microsoft-standard-WSL2, the `golang:1.27` image, the files on a
  Docker volume. `modernc.org/sqlite` v1.59.0, SQLite 3.53.4.
- **The desktop was in use**: other programs held 14 to 19 % of the processors
  while Windows ran, so its absolute figures sit below the mechanics round's;
  every comparison here is within one run. Windows' clock steps half a
  millisecond, so its latencies are not given.
- Each load runs three seconds; each platform ran twice, one test binary for
  both passes. Windows ran from 00:16 to 00:22 UTC, its comparison with
  `database/sql` after it; the container from 00:25 UTC. The container column
  gives the first pass; the second pass is below. Both passes' full output is
  [data/sqldb-engine-2026-09-28-windows.txt](data/sqldb-engine-2026-09-28-windows.txt) and
  [data/sqldb-engine-2026-09-28-linux.txt](data/sqldb-engine-2026-09-28-linux.txt).
- The notes are the mechanics round's: version 4 uuids as text, a hundred to an
  author, a third done, one in four due, written ten thousand a transaction.
  `database/sql` opens the file as `path?_pragma=busy_timeout(5000)`, one pool
  for reads and writes, each query compiled for itself and each `Exec` a
  transaction of its own.

```sh
# on Windows, from <repo>/sqldb
TINYSTORE_SPIKE=1 go test -run 'Measured$' -v -count=1 -timeout 60m
# from <repo>/spike
TINYSTORE_SPIKE=1 go test -run '^TestSQLDBCacheThatDoesNotChurn$' -v -count=1

# in the container
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off -e CGO_ENABLED=0 \
  -w /src golang:1.27 sh -c 'go test -c -o /perf/sqldb.test ./sqldb && go test -c -o /perf/spike.test ./spike'
docker run --rm -v <repo>:/src -v <volume>:/perf -e TINYSTORE_SPIKE=1 -e TINYSTORE_SQL_DIR=/perf/run \
  -e TINYSTORE_SQLDB_DIR=/perf/run golang:1.27 sh -c 'cd /src/sqldb && /perf/sqldb.test -test.run "Measured$" -test.v &&
  cd /src/spike && /perf/spike.test -test.run "^TestSQLDBCacheThatDoesNotChurn$" -test.v'
```

## Reads

The second container pass gave `One[Note]` 87,874 / 259,163 / 278,870 reads/s
at 1/8/64 goroutines, against 99,713 / 281,161 / 302,850 by hand and
57,012 / 152,660 / 158,336 through the old transaction path. Against a
program's `database/sql` pool it gave 99,805 / 273,146 / 284,475 reads/s,
against 76,852 / 136,784 / 94,065. These are the same comparisons as the
first pass in the table above.

`One[Note]` decodes every column of the design's model: a uuid from its text,
JSON into a slice, a date, a time and a bool. The statement scanned by hand
does the same conversions with no plan, no admission and no memory held: the
floor. In the container a point read took 9.3 µs at the median from one
goroutine against 8.4 by hand and 13.9 through a transaction compiling its
statement, and 29.7 µs at the 99th percentile against 26.2 and 48.4.

Against `database/sql` as a program opens it, the pool compiles every query
and, past a few goroutines, opens a connection for each: 24 to 29 % fewer reads
from one goroutine, and a third of `One`'s from sixty-four, with a 99th
percentile of 4.6 ms against 1.0 in the container.

## Writes

In the second container pass grouped `Exec` gave 341 / 1,588 / 8,054 /
22,209 inserts/s at 1/8/64/512 goroutines, against 357 / 298 / 201 / 204
from the program's `database/sql` pool. The latter had 21 lock errors at 64
goroutines and 211 at 512; grouped `Exec` had none. `Insert` gave 29,606
inserts/s at 512 goroutines against 31,099 for the insert returning nothing.
`Open` with a checked schema took 2.55 ms median against 1.81 ms without it.

Every `Exec` from `database/sql` is a transaction and an fsync of its own, and
its connections take the write lock in turn: from eight goroutines on it wrote
fewer inserts a second than from one, waited up to the five seconds of its busy
timeout, and failed 23 to 292 inserts a run with `database is locked`. sqldb's
grouped `Exec` committed 32 to 116 times as many from sixty-four and 512
goroutines, with no errors. On the simple notes table the grouped `Exec` reached
113,888 to 114,140 inserts a second from 512 goroutines on Windows and 60,510
in the container, against about 700 and 300 for a transaction each.

`Insert` returned the row with `RETURNING *` in its first build: at 512
goroutines on Windows returning the row cost 7 % and decoding it on the writer,
where every write of a group waits for it, 11 % more, 26,083 inserts a second
against 32,758. It now returns the row it wrote, a time to the millisecond in
UTC, with the rowid the insert's result carries and the generated columns a
default fills, which the insert returns: 5 % under the insert returning nothing.

## A cache that does not churn

`spike/sqldb_cache_spike_test.go` puts a door before the plain cache of 128: a
text that misses is kept only when it has been read more often, lately, than
the one it would evict, and otherwise compiles, runs and finalizes in one step.
Over 1024 texts read uniformly it compiled as often as the plain cache, 87.4 to
87.5 % of reads, and served 2.4 % fewer to 6.2 % more; with 64 of the 1024
texts taking nine reads in ten both compiled 9.3 %. The second container pass
gave the door 61,541 reads/s against 59,989 for the plain cache with uniform
texts, and 93,280 against 94,070 for the hot set. Not built.

## What it leaves

- The range read that is 3 to 5 µs slower after `analyze`, from the mechanics
  round.
- `Open`'s check on a schema of a hundred tables, not measured.
