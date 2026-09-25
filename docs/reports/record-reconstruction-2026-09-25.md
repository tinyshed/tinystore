# Exact record reconstruction and the ten-byte target — 2026-09-25

The acceptance target is met on the unchanged randomized frontend fixture:
**98,304 bytes for 10,000 records, or 9.8304 bytes per record in the complete
closed SQLite file.** Encoded payload is 89,919 bytes. This is a sealed-storage
research result, not the density of the public `records/` engine.

This follows [the first normalized-record round](record-events-2026-09-25.md).
Its eight-microblock results remain historical baselines. The new result uses
a bounded sixteen-microblock dictionary scope and a final compression pass;
those resource and decode-granularity changes are part of the result.

The [subsequent speed probe](record-speed-2026-09-25.md) measures the current
encoder at about 965 records/s on one Go CPU, with high cumulative allocation.
The density result is not a claim of practical ingestion performance.

Follow-up: [the v2 round](record-v2-2026-09-25.md) shows this file was at the
fixture's arrival-order floor, stores segments in event-time order instead, and
reaches 7.8520 bytes per record at one million records with searchable blocks.

## Environment, fixed input and verification

Spike sources of commit `1bedb5d`, measured on the identical tree before it was
committed on base `07341dec0b743c11b0557245aa345e3e309bd420`. Windows 11 Pro 10.0.26200, Ryzen 7 7700, Go 1.27.1 windows/amd64,
`modernc.org/sqlite` v1.59.0 and `klauspost/compress` v1.19.0. Codec workers are
single-threaded. Docker's Linux daemon was unavailable; no Linux runtime or
throughput result is claimed.

The original `recordFixture("frontend", 10000)` is unchanged: PCG(17,23), 500
interleaved sessions, random coordinates, random URL/element choices, optional
experiment metadata, and nanosecond time jitter. Normalized input is 2,105,558
bytes. The acceptance test asserts its SHA-256 before encoding:

```text
15c447619be8671d3b4559ac29528eadf723dad0e1e8709a52ed07ec9c2e3d02
```

No fixture seed or generator is used by the codec. There is no dictionary of
expected test answers, timestamp truncation, field omission or order change.
Model coefficients come from the bounded input; exact residuals or exceptions
preserve every difference. A fresh decoder needs only the stored bytes.

`TestStructuredRecordsBelowTenBytes` writes the segment with actual minimum and
maximum event times and record count, vacuums and closes the SQLite file,
compares `os.Stat` to page accounting, then reopens it. A fresh codec decodes
the persisted body, and every normalized record is compared in arrival order,
including all metadata, nanoseconds, optional-field presence and raw field
JSON. SQLite `integrity_check` must also pass. The test fails at 100,000 bytes
or above; the target is not rounded to an integer number of bytes per record.

The file contains the `blocks` table, its time index, and SQLite's schema page.
It does not contain a production durable head, application tables, WAL,
retention state or search indexes. The same schema and transaction/VACUUM
procedure are used for both zstd references.

## Same-input comparison

The zstd references each compress all 10,000 original normalized rows in one
frame, with a 4 MiB window. Their decompressed bytes are checked against the
same complete input. The candidate's column codecs use `SpeedDefault` with
a 256 KiB window; its final envelope uses `SpeedBestCompression` with that same
256 KiB window. Level names are the Go library's names, not asserted CLI levels.

| Representation | Payload bytes | Complete file bytes | File B/record |
|---|---:|---:|---:|
| Row zstd, `SpeedDefault`, one frame | 230,453 | 241,664 | 24.1664 |
| Row zstd, `SpeedBestCompression`, one frame | 207,555 | 217,088 | 21.7088 |
| Previous adaptive prototype, eight-microblock scope | 105,124 | 114,688 | 11.4688 |
| New adaptive prototype, sixteen-microblock scope | 89,919 | 98,304 | **9.8304** |

The candidate file is about 2.21 times smaller than the strongest row-zstd
reference and 14.3% smaller than the previous adaptive prototype. It is not a
ClickHouse comparison: the proposed six-byte ClickHouse figure was a target,
not a verified result on this input.

`dbstat` divides the final file rather than attributing a total to the codec:

| Object | Allocated bytes | Payload bytes | Unused bytes |
|---|---:|---:|---:|
| blocks | 90,112 | 89,945 | 65 |
| blocks_at | 4,096 | 11 | 4,074 |
| sqlite_schema | 4,096 | 244 | 3,735 |
| total allocated | **98,304** | | |

The strongest row-zstd file assigns 208,896 / 207,581 / 1,097 bytes to
`blocks`, with the same two 4,096-byte metadata/index pages. No index was
deleted to cross the target.

## Where the improvement came from

The earlier frontend payload divided into 4.0014 bytes of time, 2.7820 of
coordinates, 1.8024 of context dictionaries, 1.1330 of context references, and
0.7936 of other fields and framing. That accounting adds to 10.5124.

The new sixteen-block representation before the final envelope costs 9.2053
bytes per record: time 3.7812, coordinates 2.6895, context dictionary 0.8551,
context references 1.1324, and everything else 0.7471. These components include
their encoding descriptors and frames.

The tested changes were:

- Integer trend prediction and exact residuals. A floating-point fit only
  proposes integer base/step parameters; the stored residual is computed in
  integer arithmetic and the decoder uses no floating-point prediction.
- Removal of a common integer factor, sparse exceptions from a modal value,
  and radix packing for alphabets whose size is not a power of two. These
  compete with the existing four numeric representations.
- Fixed-width and byte-plane string candidates, plus a scalar variant that
  does not repeat a known UUID/hex byte width on every value.
- Per-key previous-value and first-value numeric predictors, and affine
  cross-column predictions. Literal exceptions retain type/spelling changes.
- Exact JSON structure/leaf separation, with original punctuation, whitespace,
  key order and scalar fragments available for reconstruction. A losing or
  over-limit candidate leaves the ordinary column intact.
- Stable grouping by context, with the original context-ID sequence acting
  as the restore order. It avoids storing a second full permutation.
- A sixteen-microblock scope, still capped by events, input bytes, dictionary
  cardinality and dictionary bytes. It avoids writing the frontend's mostly
  repeated session dictionary into a short second segment.
- Optional final compression of the already encoded segment. It removes
  repetition across column headers/frames. The decoder accepts only one such
  envelope and caps its expansion; recursive envelopes are refused.

Not every idea helped. Per-key/affine state and context grouping did not improve
this randomized frontend over the numeric candidates. The JSON candidate had
no frontend nested objects to exploit. The first new numeric pass reduced
payload to 10.2389 but increased the file to 11.8784: it crossed an unfavorable
SQLite overflow-page boundary. That regression was measured, not hidden.

The final segment is 92,053 bytes before the outer pass. On identical bytes,
the outer pass produced 90,286 (`SpeedDefault`), 90,476
(`SpeedBetterCompression`) and 89,919 (`SpeedBestCompression`) bytes including
the new envelope. Only the last fit the SQLite page boundary needed here:
the first two still occupied 102,400-byte files. This result has a narrow
margin and a clear page-allocation effect; it is not a smooth guaranteed ratio.

## Scope controls

Previous and final codecs were run together on every input below. Synthetic
cases each have 10,000 records. The original frontend generator was not made
more predictable. `stateful` is an additional, explicitly favorable scenario:
64 interleaved sessions with changing numeric state, related coordinates and
nested metadata. It does not replace the original acceptance fixture.

| Input | Previous payload | Final payload | Previous file | Final file |
|---|---:|---:|---:|---:|
| frontend | 10.5124 | 8.9919 | 11.4688 | **9.8304** |
| backend, independent 128-bit trace per event | 20.4161 | 18.9493 | 21.2992 | 20.0704 |
| mixed frontend/backend/text | 14.9402 | 10.5944 | 16.3840 | 11.4688 |
| a new context per event | 24.8318 | 23.0754 | 26.2144 | 24.1664 |
| repeated ID in derived fields | 2.7643 | 2.5451 | 3.6864 | 3.6864 |
| stateful sessions | 6.7923 | 2.4702 | 7.7824 | 3.6864 |
| GH Archive, 4096 real events | 374.0000 | 373.9934 | 379.0000 | 378.0000 |

All entries are bytes per record. Independent random trace bytes are still
stored. The frontend result must not become a universal promise for structured
data: a JSON envelope does not make free text or unique identifiers redundant.

The real control is the first 4096 complete JSON lines of
[GH Archive](https://www.gharchive.org/)'s
[2025-01-01 00:00 archive](https://data.gharchive.org/2025-01-01-0.json.gz).
The 11,090,290-byte prefix has SHA-256
`45f67b6eded607232de80c1a94a12e8106f74b823d40daa4f3d8863851e2bb24`,
pinned in `bench/record-events-sha256.txt`. It contains 15 event types, including
2966 pushes, 357 creates, 206 pull requests and 201 issue comments. No records
were excluded; the largest source line is 141,714 bytes.

Normalization maps `type` to event name and `created_at` to its exact instant;
every other top-level field retains its original JSON value fragment. The
JSON/tree candidates did not improve the previous result on this sample.
The one-page final difference is not evidence of successful semantic
compression of GitHub payloads. This negative control is part of the result.

## Bounds, verification and limits

Microblocks retain the 1024-event / 256 KiB / 65,536-cell limits. The new
optional scope permits at most 16 microblocks: 16,384 events / 4 MiB of original
rows. Contexts remain limited to 2048 entries / 256 KiB. JSON decomposition is
bounded by 128 shapes, 1024 columns, 128 leaves per value, 16,384 leaf cells,
and four column nesting levels. Numeric state holds at most 2048 keys. Numeric
models cannot recursively embed other models. All sizes and references are
checked on decode.

The final envelope's window is at most 256 KiB and its expanded encoded segment
at most 4 MiB. It requires the whole segment before decoding its inner objects;
the prototype offers no selective-read or peak-RSS claim. Compression effort,
buffering scope, query latency and retention granularity require measurement
before engine integration. The public `records/` implementation and root
dependency list are unchanged.

`task check` passed: formatting, lint, module checks, all tests, vulnerability
scan and the cgo-free Linux/amd64 size build. The size build is not a Linux
execution test. Focused gates cover numeric extremes, modulo arithmetic,
resets, changed types, exact JSON and byte-plane round trips, bucket order,
compressed-envelope recursion, truncation, fresh-decoder reopen and SQLite
integrity. Final 20-second fuzz runs completed 513,140 column and 577,309 segment
executions without failure, using two workers. The segment target repairs the
outer checksum before testing mutations.

## Reproduce

```sh
python bench/fetch-record-events.py <corpus>
TINYSTORE_SPIKE=1 go test ./spike -run '^TestStructuredRecordsBelowTenBytes$' -v -count=1
TINYSTORE_SPIKE=1 TINYSTORE_RECORD_MODE=previous,final \
  TINYSTORE_RECORD_EVENTS=<corpus>/2025-01-01-0.first4096.jsonl \
  go test ./spike -run '^TestDeepRecordDensity$' -v -count=1
TINYSTORE_SPIKE=1 go test ./spike -run '^TestRecordByteCensus$' -v -count=1
go test ./spike -run '^$' -fuzz '^FuzzDeepRecordColumn$' -fuzztime=20s -parallel=2
go test ./spike -run '^$' -fuzz '^FuzzDeepRecordSegment$' -fuzztime=20s -parallel=2
task check
```

On Windows set the same variables with `$env:TINYSTORE_SPIKE = '1'`,
`$env:TINYSTORE_RECORD_MODE = 'previous,final'` and
`$env:TINYSTORE_RECORD_EVENTS = '<corpus>/2025-01-01-0.first4096.jsonl'`.
`TINYSTORE_RECORD_CASE=frontend` restricts the density harness to the target;
omitting the mode filter runs individual candidate-family comparisons.
`--pin` on the fetcher deliberately updates the prefix manifest; ordinary
reproduction must omit it and verify the existing hash.

Related work: [C3's authors](https://github.com/cwida/C3) and
[Thomas Glas's thesis](https://homepages.cwi.nl/~boncz/msc/2023-ThomasGlas.pdf)
describe correlated-column compression, including numerical and dictionary
relationships. Those ideas are prior art; this round adds independent Go
experiments and makes no novelty or comparative-performance claim about C3.
