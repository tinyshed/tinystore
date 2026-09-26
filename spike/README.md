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
