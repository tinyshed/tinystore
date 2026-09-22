# Packed head: the public engine approaches the research result

The mutable head is now a packed, checksummed tail in each series-state row.
Ingest merges sorted timestamps and rewrites once per touched series. The head
remains mutable, durable and subject to the same retention/frontier rules.
Its physical compression does not advance the sealed frontier.

## Implementation and compatibility

- `metrics/head.go` owns packed encode/decode, sorted replacement merge, legacy
  reads and removal of exactly the sealed samples. Existing-codec chunks have
  at most 240 points; the entire head has separate sample and byte budgets.
- `MaxHeadBytes` defaults to 256 KiB. The hard supported ceilings are 16 MiB of
  encoded head and 1048576 samples. Exceeding a configured capacity rejects the
  entire batch; unsafe data are not sealed to make space.
- Queries fetch encoded heads inside their snapshot and decode afterwards.
  A narrow query currently decodes the complete touched head and is charged
  for every decoded point. This is bounded, but can amplify narrow reads.
- Tail encoding is bounded work inside the single writer transaction. This
  reduces SQL mutations but can increase per-write CPU when appends are tiny.
  No claim of universally better write latency follows from the bulk result.
- Schema migration 0003 preserves old row heads and records their bounds;
  each converts atomically on its next mutation. Old payload/group readers stay.
- Canonical labels use JSON pairs rather than repeated Name/Value fields.
  Migration 0004 separates the short SHA256-derived lookup identity from the
  actual labels. Full canonical equality is checked on a hash match; an injected
  collision test verifies that different labels never resolve to the same series.
  Older text identities remain readable and convert when touched by ingestion.

No production path imports the research harness. There is no new dependency.

## Repeated TSBS measurement

The former temporary copies of Alibaba and TSBS were no longer present. TSBS
was regenerated at revision `8323e59c74027b108f4ad5ec5d3e498b0101a02e`, DevOps,
seed 123, scale 20, interval 10 seconds, 2026-01-01 00:00 to 07:00 UTC.
The normalized JSONL SHA256 is:

```text
e4f502af7b0b2ff2c4dba92057a8f2b95e636882f3cb9900986e013d189572cf
```

2020 series, 5090400 samples. The converter checked all text round trips.
Go 1.27.1 linux/amd64, Docker `golang:1.27` image digest
`sha256:3680233e3204827fbdc66088528ae6d4b3d034f51d03a99d454f6de034888244`,
modernc SQLite 1.59.0, klauspost/compress 1.19.0, same local host.

The public-engine test uses the prior batch size/options and still leaves
242400 samples in the mutable head. It performs Ingest, Maintain, Close,
measures the complete file, reopens and compares every returned timestamp and
float bit with the input. No unsafe samples are force-sealed or omitted.

| Public engine stage | Whole file bytes | B/sample |
|---|---:|---:|
| Prior row-head engine | 12775424 | 2.509709 |
| Packed head plus pair labels | 5918720 | 1.162722 |
| Packed head plus short identity index | **5488640** | **1.078234** |

The final file is **57.0% smaller** than the previous public engine. Every
sample passed full readback after reopening. Diagnostic ingest+maintain+parsing
time was about 20.7 s, reopened parsing/read/bit-check 2.4 s. These are not
isolated throughput comparisons; the prior 41 s came from an earlier run.

| Current b-tree | Bytes |
|---|---:|
| payloads | 3076096 |
| series labels and identity | 712704 |
| postings | 663552 |
| series_state including packed heads | 446464 |
| groups | 409600 |
| unique series identity index | 114688 |
| series_due | 36864 |
| other listed b-trees | 28672 |
| Sum of dbstat objects | 5488640 |

The obsolete head table is empty and occupies one 4096-byte root page, included
above. Registry and its index have not disappeared: original labels, postings
and digest collision verification remain part of the engine.

## Comparison boundary

The prior TSBS VictoriaMetrics directory was 5484522 bytes, 1.0774 B/sample,
including caches. The current engine is just **4118 bytes larger**, about 0.075%.
That is near parity with the previously reported footprint, not a new controlled
victory: VM was not rerun, its earlier merges varied, and its earlier HTTP
round trip changed many original float bits. TinyStore's current full readback
is bit-exact. The best research-only TinyStore layout remains smaller at 0.966387;
the public engine has roughly 11.6% more file bytes and carries a mutable head.

Alibaba was not rerun in this step because its temporary corpus was gone and
recreating it requires the 1.8 GB archive. No new Alibaba density is claimed;
the previous 0.733083 engine figure is a historical pre-packed-head measurement.

## Verification beyond bulk import

- Existing atomic ingest, snapshot, compaction, retention, reopen and abrupt-exit
  tests pass with packed heads instead of rows.
- New tests cover out-of-order replacement, NaN/signed-zero head bits, byte-budget
  rejection with rollback, inability to bypass lateness to fit capacity, legacy
  head conversion, fixed-format reader bytes and checksum binding to a series.
- Ten seconds of packed-head decoder fuzzing executed about 145278 inputs with
  no discovered failure. Counts and lengths are bounded before allocation.
- A 1000-round incremental workload writes new and late replacement points,
  runs maintenance and reads after each round. With 400 ms retention and 10 ms
  lateness, all returned points and counts matched. Autocheckpoint was disabled;
  the WAL was 9055792 bytes before close, elapsed about 4.3 s on that run.
  This has no old-row-head WAL baseline and does not prove a WAL improvement.
- Full Windows normal tests, Linux race tests and lint passed. Broader cold-read,
  steady-state RSS/p99, longer WAL/retention and power-loss testing remain work.

## Reproduce

```powershell
./bench/run-tsbs.ps1 -Corpus ./bench/corpus/tsbs-packed -SkipEngines
```

Both corpus runners now use the public metrics engine by default. Pass
`-Prototype` to reproduce their historical spike layout. Corpus data live under
the ignored bench/corpus directory rather than depending on temporary files.
The Alibaba runner has the same engine/prototype switch. No commit was made.
