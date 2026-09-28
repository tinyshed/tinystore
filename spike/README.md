# Research harnesses

These files reproduce design experiments. Production code must not import this
package. Results describe their particular corpus and prototype, not the
behavior or density of `metrics/`.

The durable engine's correctness tests now live beside it in `metrics/` and
`internal/sqlite/`. Historical prototype tests remain available until their
measurements have equivalent harnesses against the real engine. Moving or
deleting them prematurely would break the commands in the existing reports.

Synthetic measurements use `TINYSTORE_SPIKE=1`. Corpus probes use
`TINYSTORE_CORPUS` or `TINYSTORE_JSONL`; see the corresponding report in `docs/`.
Normal unit gates in this package still run without those variables.

The `record_*_test.go` round stores a common normalized event model, compares
bounded shapes and column codecs, predicts repeated fields with exact
exceptions, and shares context snapshots across at most eight microblocks.
`TestAdaptiveRecordDensity` measures sealed payloads and SQLite objects;
`bench/run-record-events.sh` adds pinned Loghub bodies. See `docs/records.md`
and `docs/reports/record-events-2026-09-25.md`. It does not change `records/`.

`TestStructuredRecordsBelowTenBytes` pins the original frontend input hash and
checks a closed, reopened SQLite file below the stated density target, with all
records restored. `TestDeepRecordDensity` compares deeper candidate families
and a pinned GH Archive prefix fetched by `bench/fetch-record-events.py`.
`docs/reports/record-reconstruction-2026-09-25.md` includes losing candidates,
the broader dictionary scope and the whole-segment decoding cost.

`spike/record_v2_*` is the second prototype: segments sorted by event time, a
segment row and a row per block of 1024 records in SQLite with 1 KiB pages,
integer and text columns chosen by computed size, and recipe or affine
predictions tried only when a row sample agrees. `TestRecordV2Density`,
`TestRecordV2Queries`, `TestRecordV2PageFit` and `TestRecordV2AgainstV1` need
`TINYSTORE_SPIKE=1`; see `docs/reports/record-v2-2026-09-25.md`.

`TestRecordV2DockerLogs` and `TestRecordV2DockerTextScope` read container logs
collected by `bench/fetch-docker-logs.sh` through `TINYSTORE_RECORD_DOCKER`.
Such a corpus is production data: it stays local and only aggregates leave it.

`spike/kv_*` is the round `docs/kv.md` asks for before the kv engine exists:
durable Sets a transaction each against a group that a waiting caller leads,
point Gets through `File.ViewPrepared` against a bare statement, a `LoseAtMost`
counter's memory and flush, the file divided by `dbstat`, `Clear`, and the
request rates of production services from their container logs. The
measurements need `TINYSTORE_SPIKE=1`; `TINYSTORE_KV_DIR`, `TINYSTORE_KV_SECONDS`
and `TINYSTORE_KV_LARGE=1` tune them, and `TINYSTORE_KV_DOCKER` points the rates
at a corpus `bench/fetch-docker-logs.sh` collected, of which only aggregates
are printed. See `docs/reports/kv-mechanics-2026-09-26.md`.

`spike/sqldb_*` is the round `docs/sqldb.md` asks for before sqldb's first full
version is built: a point read through a transaction that compiles its
statement, a transaction around a prepared one and the prepared statement
alone, over four readers and eight; a page's `limit` as a literal, a bound
parameter and a cast one, before and after `analyze`; a connection's cache of
statements against the texts an application runs, what a statement holds and
what compiling and closing one cost; a row decoded into a struct and a struct
encoded into arguments; uuid keys as text and as bytes, of version 4 and 7; a
parent table rebuilt under its children with foreign keys on, deferred and
off; and the text SQLite keeps of checks and defaults through `ALTER TABLE`.
The measurements need `TINYSTORE_SPIKE=1`; `TINYSTORE_SQLDB_DIR` and
`TINYSTORE_SQLDB_SECONDS` tune them. See `docs/reports/sqldb-mechanics-2026-09-28.md`.
`TestSQLDBCacheThatDoesNotChurn` measures, for the engine's round, a door that
keeps a text in a connection's cache only when it is read more often than the
text it would evict, against the plain cache and against none; see
`docs/reports/sqldb-engine-2026-09-28.md`.

`spike/jobs_*` is the round `docs/jobs.md` asks for before the jobs engine
exists: a million jobs due within one minute drained in batches from a table
in the order jobs arrived with an index by time, and from tables ordered by
time whose leases move the row, mark it or live in a table of their own; the
file divided by `dbstat`, values in the row and spilled, grouped Enqueues,
a Work loop with an alarm in memory and its wake-up, a handler's insert into
another database a transaction each and grouped, and Claim and Ack one at a
time. The measurements need `TINYSTORE_SPIKE=1`; `TINYSTORE_JOBS_DIR`,
`TINYSTORE_JOBS_SECONDS` and `TINYSTORE_JOBS_COUNT` tune them. See
`docs/reports/jobs-mechanics-2026-09-27.md`.
