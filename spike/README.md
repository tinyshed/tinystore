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
