# Record layouts, first density round — 2026-09-24

How many bytes a log record costs in `records.db` today, and what dictionaries,
zstd blocks and FTS5 would change. Density only: no write speed, no read
latency, no memory. The corpus is synthetic, so these numbers bound the
question rather than answer it.

## Environment

- Commit `2b9a720` on `research/records`, Go 1.27.1 linux/amd64,
  `modernc.org/sqlite` v1.59.0, `klauspost/compress` zstd at `SpeedDefault`.
- Cloud container, Intel Xeon @ 2.80GHz, 4 vCPU. Not the reference linux
  container of `docs/measurements.md`; density does not depend on it.
- Corpus: `syntheticLogs(100000)`, seeded PCG: 16 message templates, 8 routes,
  5 000 users, a random 64-bit `trace_id` per record, 0–200 ms between records.
- One transaction, then `VACUUM`; file size is `page_count * page_size`, the
  division by object is `dbstat`, in KiB.

```sh
TINYSTORE_SPIKE=1 go test ./spike -run TestRecordLayoutDensity -v -count=1
```

## Result

| layout | payload B/record | file B/record | where the file went (KiB) |
|---|---:|---:|---|
| rows, JSON attrs (records today) | 113.4 | 141.1 | records 12 328, records_at 1 448 |
| rows + FTS5 on message and attrs | 113.4 | 239.1 | + fts data 8 476, docsize 1 068 |
| rows + FTS5 trigram | 113.4 | 631.6 | + fts data 46 760, docsize 1 072 |
| dictionary for messages and keys | 72.6 | 96.3 | records 7 948, records_at 1 448 |
| zstd blocks of 256 records | 28.0 | 32.2 | blocks 3 128, index 12 |
| zstd blocks of 1024 records | 27.6 | 28.1 | blocks 2 740, index 4 |

## Reading

- Blocks are five times denser than today's rows, and the time index almost
  disappears with them: one entry per block instead of per record. What is
  left is mostly the random `trace_id`, 8 bytes of entropy written as 16 hex
  characters; a real corpus with fewer unique values should compress further.
- A dictionary saves a third, and blocks make it unnecessary for density:
  zstd already finds the repetition.
- FTS5 over the row table costs 70 % more file than the rows; trigram, which
  answers substrings, costs 3.5 times the rows. Either index is larger than a
  compressed copy of the whole corpus, so full-text search is a feature to
  pay for on purpose, not a default.
- 256 records against 1024 changes the payload by 1.5 %; the file gains 13 %
  from pages, because a 12 KiB block overflows a 4 KiB page. Block size is a
  page question as much as a compression one.

## Second pass: FTS5 over whole blocks

Commit `d520447`, same environment, corpus and command. A block is one FTS5
document: contentless (`content=''`), `detail=none`, `unicode61` with
`tokenchars '.:_-/'`, and each distinct message and attribute value of the
block given once. A match names candidate blocks, which are then decoded and
filtered. `rows + fts5 detail=none` separates the effect of `detail` from the
effect of indexing blocks instead of records.

| layout | payload B/record | file B/record | where the file went (KiB) |
|---|---:|---:|---|
| rows + FTS5 `detail=none` | 113.4 | 187.5 | + fts data 3 444, docsize 1 068 |
| blocks of 256 + FTS5 per block | 28.0 | 54.0 | blocks 3 128, fts data 2 108 |
| blocks of 1024 + FTS5 per block | 27.6 | 48.6 | blocks 2 740, fts data 1 980 |

Candidate blocks per query, out of 391 blocks of 256 and 98 of 1024:

| term | blocks of 256 | blocks of 1024 |
|---|---:|---:|
| one `trace_id` | 1 | 1 |
| `refused` (in 1 record of 16) | 391 | 98 |
| `/api/export` (1 route of 8) | 391 | 98 |

- Searchable blocks cost 49–54 B a record, a third of today's unsearchable
  rows. Per record, `detail=none` alone saves half of the row FTS index;
  indexing blocks removes the per-document `docsize` rows as well.
- Almost all of the block index is unique values — a `trace_id` per record and
  users — and a block index does its job only for them: a rare term names one
  block, a common one names every block and the search degrades to decoding
  the whole range. A level, route or message filter needs the time range to
  narrow it, or a column of its own, not the full-text index.
- `detail=none` refuses phrase and `NEAR` queries, so `"connection refused"`
  is two terms whose blocks are intersected, then filtered exactly on decode.
- `tokenchars ':'` keeps `10.0.0.7:5432` whole, but the error text writes
  `10.0.0.7:5432:` and indexes that, trailing colon included. Which punctuation
  is part of a token is a tokenizer decision the corpus has to settle.

## Third pass: a level mask and a token bloom filter per block

Commit `83849a9`, same environment and corpus. Each block row carries a bitmask
of the levels it holds and a bloom filter over its tokens, split as `unicode61`
with `tokenchars '.:_-/'` would split them: 10 bits a distinct token, 7 hashes
from FNV-64a and FNV-64. `holding` is the number of blocks that really hold the
term, counted from the corpus.

```sh
TINYSTORE_SPIKE=1 go test ./spike -run TestRecordBlockFilters -v -count=1
```

| layout | payload B/record | file B/record | against plain blocks |
|---|---:|---:|---|
| blocks of 256 + levels + bloom | 31.6 | 33.0 | +3.6 payload, +0.8 file |
| blocks of 1024 + levels + bloom | 30.5 | 32.1 | +2.9 payload, +4.0 file |

| term | 256: candidates / holding of 391 | 1024: candidates / holding of 98 |
|---|---:|---:|
| one `trace_id` | 4 / 1 | 1 / 1 |
| one user of 5 000 | 29 / 24 | 24 / 22 |
| `refused` | 391 / 391 | 98 / 98 |
| `/api/export` | 391 / 391 | 98 / 98 |
| a token nowhere in the corpus | 4 / 0 | 0 / 0 |
| level error | 391 | 98 |

- For rare terms the filter prunes as the FTS5 block index does, at a sixth
  of its cost: 3–4 B a record against about 20, with false positives near the
  1 % that 10 bits a token promise (3 of 390 blocks of 256).
- On the file, 256-record blocks gained less than their payload: the filter
  filled slack in overflow pages the bodies had left. That is the page effect
  of the first pass again, not a property of the filter.
- The level mask and the common terms prune nothing here, and this is the
  corpus rather than the method: the generator spreads errors and routes
  uniformly, one error in sixteen records, so every block holds all of them.
  Real errors come in bursts, and whether a mask pays needs a real corpus.

## Not measured

A real corpus, write throughput with the non-blocking handler, read cost of a
time range and of a level or attribute filter inside compressed blocks, the
memory a block decode needs, and decode cost per candidate block. A durable head of recent rows that is sealed into blocks, as the
metrics engine does, is the shape these numbers point at; it is a hypothesis.
