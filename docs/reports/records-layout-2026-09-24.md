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

## Not measured

A real corpus, write throughput with the non-blocking handler, read cost of a
time range and of a level or attribute filter inside compressed blocks, the
memory a block decode needs, and FTS5 over blocks (external content keyed by
record id). A durable head of recent rows that is sealed into blocks, as the
metrics engine does, is the shape these numbers point at; it is a hypothesis.
