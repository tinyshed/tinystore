# What sqldb's mechanics cost — 2026-09-28

The round [docs/sqldb.md](../sqldb.md) asks for before sqldb's first full
version is built. Its surface is frozen; this measures the mechanics under it
through `internal/sqlite` and `database/sql`, on the notes table the design
prints: the path a point read takes, a bound `limit` and the planner's
statistics, a connection's cache of statements, a row decoded into a struct and
a struct encoded into arguments, uuid keys as text and as bytes, a parent table
rebuilt under its children, and the text SQLite keeps of checks and defaults.
Nothing here is the engine: it is the prototype `spike/sqldb_*`, on Windows and
then in a Linux container the same night; the table gives Windows, and
[the container](#the-container) section its figures.

| Question | Answer on Windows | Decision |
|---|---|---|
| a point read by id | a prepared statement without a transaction: 59,700 to 63,700 a second from one goroutine and 115,800 to 125,700 from eight; sqldb's path today, a transaction compiling its statement each call: 34,600 to 36,700 and 84,000 to 89,300 | a read is one prepared statement without a transaction |
| four readers or eight | eight served between 3.6 % less and 2.8 % more on Windows, and 15 to 26 % more from eight and sixty-four goroutines in the container | eight |
| a bound `limit ?` | compiled again at every binding: 100.3 to 107.9 µs a page against 88.4 to 93.9 with `limit 20` and 87.9 to 93.3 with `limit cast(? as integer)` | the cost is documented; sqldb rewrites no SQL, and modernc offers no switch for QPSG |
| a cache of statements | one covering the texts in use serves 69,100 to 74,100 reads a second against 46,600 to 49,100 compiling each; one smaller than them 40,700 to 46,600, less than none; a statement holds 5.4 to 8.8 KiB and compiles and closes in 4 to 22 µs | 128 a connection, about a megabyte |
| decoding a row into a struct | 1.19 to 1.30 µs a row with a plan made once, as by hand; reflection every row 1.47 to 1.61 µs; the scan alone 0.56 to 0.61 | a plan a type and column list |
| encoding an insert's arguments | reflection over the fields 0.64 to 0.74 µs a row against 0.51 to 0.61 by hand | reflection, planned once as the decoding is |
| uuid keys | 500,000 notes and a comment on each: 105.4 to 105.8 MiB as text, 64.4 to 64.7 as bytes, reads within 3 to 12 %; version 7 writes the notes in 1.3 to 1.6 s against 6.8 to 9.9 for version 4 | text stays the default; the documentation recommends version 7 |
| a migration rebuilding a parent | foreign keys on, their checks deferred or not: all 100,000 children deleted; off, with `foreign_key_check` before the commit: all kept, in 8 to 10 ms | foreign keys off while migrations run |
| the text of checks and defaults | kept as written; `ADD COLUMN` appends, `RENAME COLUMN` rewrites the name inside checks; pragmas report types, keys, references, indexes and defaults' text | `Open` compares structure, and defaults that are values by what SQLite computes from their text |

## Environment and reproduction

- The prototype `spike/sqldb_*` at the commit that adds this report, over the
  design in `docs/sqldb.md`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1 GiB, one Samsung SSD
  990 PRO 1 TB (NVMe), Windows 11 Pro 10.0.26200; go1.27.1 windows/amd64; the
  files in the temporary directory, NTFS on that NVMe.
- **Windows first.** Go's clock on Windows advances in steps of about half a
  millisecond here, so no latency below a millisecond is reported from it;
  throughput is, since it divides operations by a whole run.
- **Then the container**, on the same machine and the same working tree, from
  one test binary built before either pass: Docker Desktop 29.6.2 on WSL2,
  kernel 6.18.33.2-microsoft-standard-WSL2, 16 CPUs and 15.2 GiB visible; the
  `golang:1.27` image, go1.27.1 linux/amd64; the files on a Docker volume, ext4
  on the WSL2 virtual disk on the same NVMe. The two passes ran from 22:58 to
  23:05 UTC on 27 September; their output is
  [data/sqldb-mechanics-2026-09-28-linux.txt](data/sqldb-mechanics-2026-09-28-linux.txt).
- `modernc.org/sqlite` v1.59.0, SQLite 3.53.4; `synchronous=FULL`, WAL, a
  1 MiB page cache a connection, as `internal/sqlite` opens a file.
- Each load runs three seconds. Everything ran twice, the second pass right
  after the first; a range is the two runs, and a figure only one run gave is
  called so.

```sh
# on Windows, from <repo>
TINYSTORE_SPIKE=1 TINYSTORE_SQLDB_DIR=<dir> go test ./spike -run '^TestSQLDB' -v -count=1 -timeout 60m

# in the container: one binary, then two passes of it
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off -e CGO_ENABLED=0 \
  -w /src golang:1.27 go test -c -o /perf/sqldb-spike.test ./spike
docker run --rm -v <repo>:/src -v <volume>:/perf -e TINYSTORE_SPIKE=1 -e TINYSTORE_SQLDB_DIR=/perf/sqldb \
  -w /src/spike golang:1.27 /perf/sqldb-spike.test -test.run '^TestSQLDB' -test.v -test.count=1 -test.timeout 60m
```

## Point reads

A million notes of the design's table, their ids version 4 uuids as text; each
goroutine reads a random note by its id, `select` of all seven columns.
**Compiled each call** is what sqldb does today: `File.View` and the query
through its `*sql.Tx`, which begins a transaction and compiles the statement.
**Transaction, prepared** is `File.ViewPrepared`, a transaction around the
connection's prepared statement. **Statement** is `File.Lookup`, the prepared
statement alone, a statement being its own snapshot.

| readers, goroutines | compiled each call | transaction, prepared | statement |
|---|---:|---:|---:|
| 4, 1 | 34,616; 36,749 | 51,682; 56,018 | 59,670; 63,724 |
| 4, 8 | 83,962; 89,258 | 105,307; 113,665 | 115,779; 125,656 |
| 4, 64 | 81,412; 86,632 | 105,102; 111,720 | 115,866; 124,416 |
| 8, 1 | 32,738; 34,705 | 53,304; 54,797 | 59,452; 63,336 |
| 8, 8 | 87,932; 96,512 | 105,963; 110,006 | 118,882; 122,583 |
| 8, 64 | 85,431; 96,831 | 105,059; 110,875 | 119,067; 119,925 |

Reads a second, first run; second run. The statement alone serves 72 to 73 %
more than today's path from one goroutine and 38 to 44 % more from eight or
sixty-four; the transaction around it costs 10 to 15 %. Eight readers against
four moved the statement's reads by −3.6 to +2.8 %: on sixteen logical
processors four readers are not what holds a point read back.

## A bound limit

One reader pages through an author's hundred notes, twenty the newest first,
the statement prepared once on its connection. `computeLimitRegisters` reads a
bound limit through `sqlite3ExprIsInteger`, which marks the parameter with
`sqlite3VdbeSetVarmask`, and `vdbeUnbind` expires the statement whenever that
parameter is bound again; a cast is an expression, and is not marked.

| query | before `analyze` | after `analyze` |
|---|---:|---:|
| `limit 20` | 90.3; 88.4 µs | 93.9; 88.9 µs |
| `limit ?` | 102.8; 100.7 µs | 107.9; 100.3 µs |
| `limit cast(? as integer)` | 93.1; 88.4 µs | 93.3; 87.9 µs |
| `author_id = ? and created_at > ?`, `limit 20` | 91.6; 86.0 µs | 96.1; 91.3 µs |

A page's time from one goroutine, first run; second run. `limit ?` costs 11 to
14 µs a page, the compile, on a page of about 90 µs; the cast costs nothing.
`analyze` wrote 48 samples to `sqlite_stat4`. After it the plain page moved by
0.5 and 3.6 µs and the cast one by −0.5 and 0.2; the range read was 4.5 and
5.3 µs slower in the two runs, which this round does not separate into a
recompile or another plan.

QPSG, `SQLITE_DBCONFIG_ENABLE_QPSG`, would stop the marking as well, and the
planner's reading of other bound values with it. modernc v1.59 applies
`sqlite3_db_config` only for `_dqs` and `_defensive` and exposes no call for
it, so it is out of reach without changing the driver.

## A cache of statements

One reader connection runs point reads spread uniformly over 16, 128 or 1024
texts, which differ only in a comment so that each compiles as much as the
others; a least-recently-used cache keeps 32, 128 or 512 of them, or none
compiles each call as `database/sql` does without one.

| texts | none | 32 | 128 | 512 |
|---:|---:|---:|---:|---:|
| 16 | 47,181; 47,942 | 69,348; 73,972 | 70,538; 74,097 | 69,098; 74,002 |
| 128 | 46,567; 49,143 | 44,220; 46,638 (75 % compiled) | 67,544; 72,202 | 68,439; 72,330 |
| 1024 | 46,878; 48,342 | 40,710; 42,108 (97 %) | 40,971; 42,418 (88 %) | 46,785; 48,442 (50 %) |

Reads a second, the second and third runs of the table. Its first run gave
1024 texts 26,629 at 128 and 31,179 at 512; two runs after it, one of them the
full second pass, did not reproduce that, and it is not used.

| a statement | holds | compiled and closed in |
|---|---:|---:|
| a point read | 8.4 KiB | 5.6; 5.8 µs |
| an author's page | 5.4 KiB | 4.9; 5.5 µs |
| a report by day | 7.7 KiB | 3.7; 8.5 µs |
| a join of four | 8.8 KiB | 22.4; 22.3 µs |

- **A cache that holds the texts in use is worth half again.** Compiled from
  the cache, a point read is 14 µs; compiled each call, 21.
- **A cache smaller than them is worse than none**, 5 % at 75 % misses and 13 %
  at 88 to 97 %:
  a miss closes a statement, compiles one and then runs it through a `Stmt`,
  where a call without a cache compiles, runs and finalizes in one step.
- **What a cache keeps costs nothing to the next compile.** With 0 to 1024
  statements kept on the connection, a compile and close took 4.0 to 6.3 µs,
  and a close 1.4 to 2.1 µs whether the statement had run or not.
- 128 statements of these sizes hold 0.7 to 1.1 MiB a connection.

## Values

200,000 notes of the design's table read into the model's struct, whose id is a
`[16]byte` standing for a known uuid, its tags JSON, its due date text and its
time unix milliseconds, each way twice a run. **A plan made once** finds each
column's field and conversion before the first row and scans into holders it
keeps; **reflection every row** asks the rows for their columns, makes a holder
for each and finds each field by name, as a mapper without a plan does.

| decoding | ns a row | allocations a row |
|---|---:|---:|
| by hand | 1,328, 1,158; 1,229, 1,200 | 18.8 |
| a plan made once | 1,218, 1,304; 1,191, 1,200 | 12.7 |
| reflection every row | 1,590, 1,609; 1,477, 1,470 | 20.7 |
| the scan alone, nothing converted | 576, 613; 557, 566 | 8.5 |

| encoding an insert's arguments | ns a row | allocations a row |
|---|---:|---:|
| by hand | 570, 605; 511, 529 | 13.0 |
| reflection over the fields | 738, 709; 643, 645 | 18.0 |

A plan costs what writing the decoding by hand does, with fewer allocations,
since its holders live as long as the query; reflection every row costs a
fifth to a third more than a plan. The conversions together cost about as
much as the scan.

## uuid keys

500,000 notes keyed by a uuid, then a comment on each note in a scattered
order, whose `note_id` references its note through an index, in transactions of
10,000; four files: the uuid as text and as 16 bytes, of version 4, random, and
version 7, ordered by its milliseconds.

| key | notes written | comments written | the four objects | a note by id | a note's comments |
|---|---:|---:|---:|---:|---:|
| text, v4 | 9.87; 9.91 s | 13.07; 12.90 s | 105.4 MiB | 67,639; 64,616/s | 67,811; 66,126/s |
| text, v7 | 1.52; 1.56 s | 12.95; 13.09 s | 105.8 MiB | 69,099; 70,668/s | 67,079; 65,637/s |
| bytes, v4 | 7.08; 6.84 s | 9.74; 9.50 s | 64.4 MiB | 69,897; 72,621/s | 71,235; 74,111/s |
| bytes, v7 | 1.38; 1.33 s | 9.76; 9.42 s | 64.7 MiB | 73,609; 77,707/s | 71,076; 74,526/s |

| object, bytes a row | text | bytes |
|---|---:|---:|
| `notes` | 69.3 | 49.2 |
| its key's index | 50.6 to 51.8 | 27.6 to 28.6 |
| `comments` | 50.7 | 30.2 |
| `comments_note_id` | 49.9 to 50.4 | 27.7 to 27.8 |

A uuid as text costs 20 to 23 bytes more wherever it lies: the row, its key's
index, every row that references it and that reference's index, 1.64 times
these four objects. Reads by key are 3 to 12 % slower. The version decides more
than the storage: a random key lands on a random page of the key's index, and
the notes went in 5.1 to 6.5 times faster when their ids arrived in order. The
comments, which reference notes in a scattered order, took as long with either
version.

## A migration rebuilding a parent

1,000 authors and 100,000 notes that reference them with `on delete cascade`;
one transaction runs SQLite's procedure for a change `ALTER TABLE` cannot
make: a new table, the rows copied, the old one dropped, the new one renamed.

| foreign keys | children left | time |
|---|---:|---:|
| on, as a migration runs today | 0 of 100,000 | 118; 116 ms |
| on, `pragma defer_foreign_keys = on` inside the transaction | 0 of 100,000 | 112; 106 ms |
| off on the connection before `BEGIN`, `foreign_key_check` before `COMMIT` | 100,000 of 100,000 | 10; 8 ms |

`DROP TABLE` runs an implicit `DELETE`, which fires the children's actions; a
deferred check defers the checking, not the actions. `foreign_keys` cannot
change inside a transaction, so the migrator turns it off on the writer before
`BEGIN` and on after `COMMIT`. The children's `references authors (id)` text
still names the table, now the new one.

## The text of checks and defaults

A table made with a check on a column, a table check, a literal default and an
expression default; then a column added, a column renamed, the table renamed,
and a second table spelling its check otherwise.

```text
made           done   integer not null default 0 check (done in (0, 1)),
add column     code   text not null default (lower(hex(randomblob(4)))), tags text not null default '[]' check (json_valid(tags)),
rename column  finished   integer not null default 0 check (finished in (0, 1)),
rename table   CREATE TABLE "u" (
spelled        finished integer not null default 0 CHECK ( finished IN(0,1) )
```

- **`sqlite_schema` keeps what was written**, spacing and case included. `ADD
  COLUMN` appends the column's text after the last column, on its line;
  `RENAME COLUMN` rewrites the name in the definition and inside the check and
  leaves the spacing; `RENAME TABLE` quotes the new name.
- **The pragmas describe the structure.** `table_xinfo` gives each column's
  type in capitals, `not null`, its place in the primary key, and its default
  as text without `DEFAULT` and without an expression's outer parentheses:
  `0`, `'draft'`, `'[]'`, `lower(hex(randomblob(4)))`. `index_list` gives an
  index's name, uniqueness, origin and whether it is partial;
  `foreign_key_list` its table, columns and actions.
- **A default that is a value can compare by value.** SQLite computes `0` and
  `'draft'` from their text, so `Open` can compare what `select <default>`
  gives with the declared value, and a default spelled `00` in a hand-written
  migration is then not a difference.
- **A check is text only**, and folding space and case outside quotes makes
  `check (done in (0, 1))` and `CHECK ( done IN(0,1) )` one string, which is
  all the test needs for the migrations the tool writes.

## The container

The same prototype and loads, two passes; a pair is the first pass and the
second. The container's clock resolves a microsecond, so latencies are given.

| readers, goroutines | compiled each call | transaction, prepared | statement |
|---|---:|---:|---:|
| 4, 1 | 55,871; 56,418 | 78,716; 76,700 | 93,238; 96,228 |
| 4, 8 | 115,287; 113,633 | 155,956; 143,376 | 181,166; 186,251 |
| 4, 64 | 116,316; 115,029 | 154,585; 157,303 | 187,534; 193,496 |
| 8, 1 | 53,757; 55,576 | 75,452; 75,926 | 96,020; 93,929 |
| 8, 8 | 149,527; 157,517 | 183,850; 181,218 | 228,489; 233,350 |
| 8, 64 | 157,856; 155,154 | 188,859; 180,930 | 226,068; 222,662 |

Point reads a second. The statement alone serves 57 to 71 % more than the
transaction compiling each call, as on Windows, at 8.9 to 9.0 µs a read at the
median from one goroutine against 14.7 to 15.3, and 32.0 to 34.7 µs at the
99th percentile against 55.4 to 60.7. **Eight readers serve 15 to 26 % more
statements than four from eight and sixty-four goroutines**, 222,662 to
233,350 a second against 181,166 to 193,496, and within 3 % from one; on
Windows they served no more. Servers run on Linux, so sqldb opens eight, as kv
does.

| query | before `analyze` | after `analyze` |
|---|---:|---:|
| `limit 20` | 19,643; 19,743 a second, 43.7; 43.4 µs | 19,041; 19,236, 45.0; 44.0 µs |
| `limit ?` | 16,441; 16,494, 52.2; 51.5 µs | 16,169; 16,508, 52.8; 51.6 µs |
| `limit cast(? as integer)` | 19,454; 19,530, 44.2; 44.0 µs | 19,103; 19,436, 44.9; 43.7 µs |
| a range, `limit 20` | 19,647; 20,058, 45.6; 44.6 µs | 18,896; 18,943, 48.7; 49.1 µs |

Pages a second and their median time. `limit ?` compiles again as on Windows:
8.1 to 8.5 µs a page at the median, 16 % fewer pages; the cast costs nothing.
The range read is 3.1 and 4.5 µs slower after `analyze` here too, which this
round still does not separate into a recompile or another plan.

| texts | none | 32 | 128 | 512 |
|---:|---:|---:|---:|---:|
| 16 | 72,584; 75,032 | 107,239; 111,018 | 106,583; 111,553 | 105,237; 110,579 |
| 128 | 71,364; 75,816 | 67,789; 70,779 (75 % compiled) | 103,150; 108,612 | 102,676; 109,320 |
| 1024 | 72,712; 75,388 | 62,437; 64,106 (97 %) | 61,089; 64,039 (88 %) | 70,556; 74,404 (50 %) |

Reads a second. A cache holding the texts in use serves 43 to 48 % more than
none; one smaller than them 5 to 16 % fewer, as on Windows. A compile and close
beside 0 to 1024 kept statements took 4.3 to 7.0 µs, a close 1.7 to 2.3; the
four statements held what they did on Windows, 5.4 to 8.8 KiB, and compiled and
closed in 4.7 to 21.6 µs.

| | Windows | the container |
|---|---:|---:|
| a row decoded: by hand; a plan; reflection every row; the scan alone | 1.16 to 1.33; 1.19 to 1.30; 1.47 to 1.61; 0.56 to 0.61 µs | 1.21 to 1.30; 1.21 to 1.26; 1.55 to 1.59; 0.59 to 0.62 µs |
| an insert's arguments: by hand; reflection | 0.51 to 0.61; 0.64 to 0.74 µs | 0.52 to 0.54; 0.67 µs |
| 500,000 notes written: text v4; text v7; bytes v4; bytes v7 | 9.87 to 9.91; 1.52 to 1.56; 6.84 to 7.08; 1.33 to 1.38 s | 5.20 to 5.28; 1.55 to 1.56; 3.97 to 4.02; 1.45 s |
| a note by id: text v4; bytes v4 | 64,616 to 67,639; 69,897 to 72,621 a second | 110,557 to 110,683; 111,156 to 114,341 |
| a parent rebuilt under 100,000 children: on; deferred; off and checked | 0 left, 116 to 118 ms; 0, 106 to 112; 100,000, 8 to 10 | 0 left, 109 to 118 ms; 0, 105 to 116; 100,000, 10 to 11 |

The container keeps the Windows answers: a plan decodes as by hand, a uuid as
text costs its bytes and 0 to 3 % of a read by key, and rebuilding a parent with
foreign keys on deletes every child. Version 7 is 2.7 to 3.4 times faster to
insert here against 5.1 to 6.5 on Windows: the random key costs less on this
file system, and still the most. The four objects' sizes are the same to the
row, 105.4 to 105.8 MiB as text and 64.4 to 64.7 as bytes.

## What this settles in docs/sqldb.md

- A point read is one prepared statement without a transaction: 38 to 73 %
  more reads than today on Windows, 57 to 71 % in the container.
- Eight readers: four served as many on Windows and 15 to 26 % fewer in the
  container.
- Each connection keeps 128 compiled statements.
- `limit ?` compiles every call, 11 to 14 µs a page; the documentation says
  so and shows the cast, and nothing rewrites the application's SQL.
- Rows decode through a plan made once for a type and its columns, and an
  insert's arguments through one made once for a type.
- A uuid key stays text by default; the documentation recommends version 7,
  and says what text costs a key.
- Migrations run with foreign keys off and `foreign_key_check` before the
  commit.
- `Open` compares structure, and defaults that are values by value.

## What it leaves

- **A cache that does not churn.** Keeping a text only from its second use in a
  window would spare a working set larger than the cache the 13 to 16 % a miss
  costs over no cache; the build measures it against the plain
  least-recently-used cache.
- **The range read after `analyze`**, 3 to 5 µs slower in all four runs: a
  recompile or another plan.
- **A long transaction failing the grouped writes behind it**, found before
  the round: a correctness fix in `internal/sqlite`, which the build gates.
