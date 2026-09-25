# What the records research settles for the engine — 2026-09-25

This closes the records research of 24 and 25 September, merged into `main`
as `ece67a5`, and hands it to the implementation. The design is
[docs/records.md](../records.md). This report says which measurement stands
behind each decision, what the first engine version takes and leaves, what it
has to build that no round touched, the gates it must pass and the numbers it
must hold. The implementation's work plan is derived from it.

## The rounds

Every commit below is reachable from `main`; each report carries its
environment and command.

| Date | Round | Commit | Report | What it settled |
|---|---|---|---|---|
| 24 Sep | rows, dictionaries, zstd blocks, FTS5 | `2b9a720`, `d520447`, `83849a9` | [records-layout](records-layout-2026-09-24.md) | blocks are five times smaller than rows; FTS5 costs more than the data; level masks and token blooms instead |
| 24 Sep | Drain templates on Loghub samples | `db34377` | [loghub-templates](loghub-templates-2026-09-24.md) | templates with untyped variables beat zstd by 11 % |
| 24 Sep | typed template variables on full Loghub | `07341de` | none | no report was written; nothing here relies on it |
| 25 Sep | normalized events, shapes, shared contexts | `1bedb5d` | [record-events](record-events-2026-09-25.md) | one model for logs and events; per-segment context dictionaries |
| 25 Sep | exact reconstruction | `1bedb5d` | [record-reconstruction](record-reconstruction-2026-09-25.md) | 9.83 B/record, the fixture's arrival-order floor; a whole-segment envelope no query can use |
| 25 Sep | encoder cost | `1bedb5d` | [record-speed](record-speed-2026-09-25.md) | 965 records/s: exhaustive trial compression |
| 25 Sep | v2 | `0f7500a` | [record-v2](record-v2-2026-09-25.md) | event-time order, a row per block, choice by computed size, 1 KiB pages |
| 25 Sep | production container logs | `9d01766` | [record-docker-logs](record-docker-logs-2026-09-25.md) | real density is decided by what programs log; text needs templates |
| 25 Sep | sealing, head, late records, blooms, memory | `fed6ea9` | [record-sealing](record-sealing-2026-09-25.md) | seal after an hour, a head of zstd rows, a late head, id blooms, 24 MiB per segment |

## Decisions and their evidence

| Decision | Evidence |
|---|---|
| Records are ordered by event time within a segment; `Append` order is not observable; queries merge by time; consumers follow `(segment, row)` | arrival order cost 1.03 B/record on the frontend fixture, 8.7224 against 7.6906 at one million records |
| A segment row plus a row per block of at most 1024 records; nothing compressed across rows | SQL reads a blob whole; a one-second query reads 2 of 977 blocks |
| 1 KiB pages | 7.8520 B/record against 8.6016 with 4 KiB pages; narrow queries as fast |
| Columns chosen by computed size, zstd only once per text blob | 400,000 to 480,000 records/s against 965, at the same density |
| Contexts in a per-segment dictionary | 0.54 B/record at 16,384 records a segment |
| A head of zstd rows, one per stream and flush | 129.5 B/record at two records a flush, against 152.4 as a segment |
| Seal at 16,384 records, 4 MiB, or an hour of age | an hour costs 9 % of the file on a quiet fleet, a minute 230 % |
| A late head for records more than a minute behind their batch | one-second reads fell from 26.08 to 5.12 blocks with 1 % of records ten minutes late |
| A bloom per block for id-like strings and for trace ids | a request id lookup read 11 blocks instead of 160 |
| 24 MiB reserved per segment in flight | encoding allocates 3–5 times the input and decoding 4–6 |

## The first engine version

**Takes:** the order contract; segment, block, trace-filter and attribute-filter
rows; 1 KiB pages and the covering time index; the integer codec (transforms,
base and divisor, bit width, radix words, Rice, FSE, dictionary, dominant value);
typed values (integers with exceptions, quoted integers, UUID and hex bytes,
text); newline-separated text; per-segment contexts; shapes with the serialized
fallback; CRC-32 per row; the bounds of docs/records.md; the head, the seal
policy, the late head and the memory reservation.

**Leaves out:**

- Recipe and affine predictions: identical bytes on the production logs with
  and without them, measured at `fed6ea9` with a temporary switch, and they cost
  format rules and decoder dependencies.
- The per-segment text sample: 2.6 % on text records only, for a decoder state
  per segment; it returns, if at all, with text templates.
- Arrival-order storage, the first prototype's envelope and every earlier
  format: the engine's format starts at version one and reads nothing older.

## What no round built

Research measured encodings and reads over finished segments. The engine has
to build:

- **The public surface:** the record type, append, query with its filters and
  limits, a consumer cursor, options (retention, seal age, memory), `Stats`.
- **The head:** its table, flush from the non-blocking handler, visibility to
  queries before sealing, late-head routing, recovery through SQLite alone.
- **Sealing:** a job on `Store.Every` that writes the segment, blocks and
  filters and deletes the head rows in one transaction; a failed seal leaves
  the head as it was.
- **Queries:** candidates from the covering index, level masks, blooms and an
  index of attribute keys per segment; bytes fetched in a short read
  transaction and decoded outside it; a bound on blocks, bytes and decoded
  records before a query starts; results merged by event time; cancellation.
- **Retention:** whole segments whose newest record is past the cutoff; reads
  clipped by one captured cutoff, as metrics does.
- **Memory:** `Store.Reserve` around every encode and decode.
- **Replacement of the first version:** its row table goes; nothing released
  wrote a file anyone must read.

The metrics engine already solved the patterns: a durable head sealed in one
transaction, one snapshot per read, retention clipping, admission through the
store's memory budget, and the prepared-statement discipline of
`internal/sqlite`.

## Gates the implementation must add

Each is a test that fails loudly, in the manner of the gates table in
`AGENTS.md`:

- a record survives the format byte for byte, against golden vectors carried
  over from the spike's fixtures;
- a block decodes with its segment row and nothing else, and a changed or
  missing byte in either row is refused; the format is fuzzed;
- equal event times keep arrival order, and a query returns event-time order
  across overlapping segments and the head;
- a record in the head is visible before its segment is sealed;
- sealing is one transaction, and a failed one leaves the head intact;
- a record more than a minute behind its batch is sealed from the late head;
- a lookup by an id-like value skips blocks whose bloom excludes it, and a
  level filter skips blocks whose mask does;
- encode and decode hold the store's memory reservation;
- retention removes whole segments and clips what a read returns;
- the existing handler gates stay: it never blocks, and refuses the records
  engine's own lines.

## Numbers the engine must hold

Measured with the same fixtures and corpora, on the same kind of machine; the
spike's figure in brackets:

- one million frontend records: at most 7.9 B/record in the file (7.8531);
- the production container corpus: at most 20.1 B/record in the file (20.0860);
- encoding at least 400,000 frontend records/s on one Linux CPU (404,961),
  decoding at least 1.3 million (1,512,787), at most 1 KB allocated per encoded
  record (758 B);
- a one-second read of one million frontend records: at most 2 blocks (1.92);
- one request id among the production corpus: at most 11 blocks (11);
- one-second reads with 1 % of records ten minutes late: at most 5.2 blocks
  (5.12).

## Corpora

- Synthetic fixtures: `recordFixture` and `recordStatefulFixture` in `spike/`,
  seeded.
- GH Archive: `bench/fetch-record-events.py`, pinned by
  `bench/record-events-sha256.txt`.
- Production container logs: `bench/fetch-docker-logs.sh`, private, snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`; never
  committed, reported only in aggregate.
- Loghub: `bench/run-loghub.sh`, pinned by `bench/loghub-sha256.txt` and
  `bench/loghub-full-sha256.txt`.

## Left for later research

- Text templates with typed variables, a line's own timestamp first; the
  `07341de` spike is where to start, and its result needs a report.
- A continuation rule for multi-line records; adapters for pino, logfmt, glog
  and log4j lines.
- Merging a stream's small sealed segments; a bound on the time-index scan when
  late blocks are wide.
- A store-level context registry, per-context numeric state, nested JSON
  decomposition, and the predictions left out above, each on a real workload
  that asks for it.
