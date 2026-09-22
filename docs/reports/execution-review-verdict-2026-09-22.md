# Execution review verdict and required corrections

This reviews [execution-review-2026-09-22.md](execution-review-2026-09-22.md).
It records the optimization verdict from the conversation. No new measurements,
source inspection or implementation work were performed for this document.
Evidence identifiers below refer to the execution review; its inspected engine
revision is `f2974bc`. Its measurements retain their original environments and
limitations. This document changes neither the engine contract nor the status
of proposals to implemented behavior.

## Verdict

Keep SQLite and modernc. The strongest next direction is to reduce the amount
of work performed around them: statements, allocations, redundant state reads,
index maintenance, head re-encoding and commits.

E1 and E2 support that direction. They do not yet establish that TinyStore needs
a replacement execution layer, a writer actor or direct SQLite bindings. Treat
the review as a sequence of experiments, not a requirement to implement every
runtime proposal before continuing the metrics engine.

## Work accepted as the next implementation direction

- **A1 + D3:** bounded writer statement reuse, one state read and one merged
  UPDATE. Exclude unchanged indexed columns from the SET list. Assigning them
  through CASE still names those columns and does not accomplish that exclusion.
- **A11:** reuse untouched encoded head chunks and add a sorted-input fast path,
  preserving duplicate-last-wins, ownership, budgets and the sealed frontier.
- **A10:** bounded batched reads of descriptors, heads and selected payloads,
  preserving one snapshot and checking byte budgets before materialization.
- **D4/D5:** keep healthy connections after ordinary application errors and
  reuse warm idle connections. Normalize SQL shapes before enlarging caches.
- **D1/D2:** isolate per-series maintenance failures and make selection fair,
  with an explicit recovery path for suspended work.
- **A5:** publish small, byte-bounded maintenance batches with savepoints and
  correct accounting after the outer transaction commits.

These changes address measured or directly observed costs while retaining the
current storage foundation. Their individual effects still need measurement.

## Corrections and decision gates

### 1. A2 L1a: WithoutCancel is not an ordinary production optimization

A primary-key lookup with a small result does not guarantee a small latency:
I/O and waiting remain. Checking a deadline between statements cannot interrupt
a statement already executing. Do not silently weaken SnapshotTimeout to remove
per-statement cancellation overhead.

Removing repeated cancellation machinery is acceptable only with effective
connection- or transaction-level cancellation. An interrupt belonging to a
completed operation must never reach the next operation using that connection.
Cleanup, callback synchronization and connection reuse need tests.

Until that mechanism exists, L1a belongs in a benchmark experiment. Compare L1b
while retaining the driver's cancellation first. Acceptance must cover deadlines,
cancellation and cleanup in addition to throughput and the lock profile.

### 2. A2 L1b/L2: experiment before choosing a new execution layer

E1 justifies comparing sql.Conn.Raw with the existing path. It does not yet
justify adopting modernc.org/sqlite/lib directly. Reduce statement count through
normal batching first, then profile again: some of the reason for bypassing the
wrapper may disappear.

The number of SQLite entry points understates the maintenance obligation.
Direct bindings must own memory lifetimes, reset/finalize, cancellation, errors,
connection shutdown and platform differences. A benchmark must exercise those
semantics, not just the successful lookup loop.

**Correction to E4:** a dependency requiring modernc 1.37.1 does not by itself
downgrade a root module requiring 1.59.0. Under normal Go minimal version
selection, the higher requirement wins. Compatibility with that selected version
still needs testing. The root dependency gate remains an independent reason not
to add zombiezen. Align dependency versions and SQLite settings in a proxy
comparison before attributing differences to the execution wrapper.

### 3. A4: separate group commit from checkpoint policy

First batch maintenance publications. Then evaluate grouping independent user
requests, with explicit rules for:

- cancellation before execution and after execution begins;
- errors that invalidate the whole transaction rather than one savepoint;
- failure of the outer COMMIT and notification of every waiter;
- when statistics and cached state become committed;
- admission, batch bytes, batching delay and writer hold time.

Preserving FULL requires acknowledgement after the common commit; it does not
alone settle the other API semantics.

Do not make RESTART at a WAL threshold the initial checkpoint policy. Waiting
for readers there can delay the entire writer queue. Start with observation and
PASSIVE/automatic checkpointing as the backstop. Accept more aggressive modes
through a separate comparison, distinguishing active WAL frames from physical
file length. A large retained WAL file does not by itself mean equivalent
checkpoint debt.

### 4. D1: quarantine needs a recovery lifecycle

ErrCorrupt and ErrLimit after lowering configured capacities are different
conditions. Store a useful reason, provide a way to retry or revalidate, and
specify recovery when limits change. A failed_at marker that permanently removes
a series from maintenance is not a complete fix.

Distinguish a damaged series payload from a file-level SQLite or I/O failure.
The latter must not be hidden as an isolated bad series. Quarantined work must
remain visible across reopen, with clearly defined counters and ingestion rules.

### 5. A10 and D6: optimize fetching without weakening budgets

One extra materialized row is not a negligible allowance when that row contains
a packed head. Prefer a bounded size/descriptor phase that reserves the bytes
before fetching selected bodies. Alternatively, any permitted overfetch must
have its own explicit accounted bound; it must not silently weaken the existing
payload budget.

Skipping head chunks outside the range must still validate the enclosing CRC,
chunk structure, counts, ordering and endpoints. Decide how selected chunks are
charged to DecodedSamples; merely skipping decode while charging the entire head
does not provide the same resource behavior. Preserve the legacy path.

### 6. A13: introduce streaming as a separate contract

Replacing Read with an iterator changes the existing promise of an error without
a partial result. Prefer a separate streaming API while retaining Read's
materializing behavior.

Lazy decoding after the snapshot still retains the compressed bytes fetched by
that snapshot. It reduces output materialization, not all query memory. Define
early termination, context cancellation, admission lifetime and cleanup. Bounded
parallel decoding must share the work/memory budget rather than just the reader
connection count.

### 7. A14: choose numerical semantics before a representation

For the audit's example the exact mathematical sum is 2. That is the appropriate
target if the new API promises a correctly rounded mathematical sum. Independence
from block boundaries alone does not establish numerical exactness.

A floating-point expansion requires validation over the full float64 domain:
intermediate overflow followed by cancellation, subnormals, expansion length,
NaN and infinities. One or two components are not sufficient in the general case.
Compare expansions with a fixed superaccumulator and measure their persisted
size on the corpora before selecting the summary format.

Keep the transition between adjacent counter blocks in the contract. The
existing raw API is not incorrect because its future aggregate shortcut needs
these decisions.

### 8. D7/D8: keep the optimization narrowly scoped

An already-checksummed value-stream entry point may remove a redundant envelope
and copy. It must still validate stream structure and decode bounds; it must not
turn into a general unchecked public decoder.

The lock evidence ranks decoder concurrency below statement execution. Do not
introduce decoder pools or replace a Mutex with an RLock until shared scratch,
close behavior and memory bounds are accounted for. Revisit after the higher-cost
work has been removed or a wide/mixed profile demonstrates a different ranking.

### 9. A6/A9: ownership and caches are separate design decisions

A single owning process changes opening and interoperability behavior. Adopt
that policy only when its benefit and contract are explicit; compiled-statement
reuse does not require authoritative cached database contents.

One owner also does not automatically make mutable caches snapshot-consistent.
Keep content-addressed immutable caches distinct from identity, cardinality and
allocation state. Series reclamation and id reuse require explicit invalidation
or generations. Do not make the first optimization round depend on moving
authoritative counters out of SQLite.

## How to interpret the expected-effects table

The X rows are hypotheses, not delivery targets or evidence of achievable gains.

- **X1:** lock-delay shares do not translate directly to throughput improvement.
  Removing a percentage of sampled waiting is not the same as removing that
  percentage of a query's critical path.
- **X2/X3:** do not infer statement cost by dividing all wide-query elapsed time
  by statement count. Do not apply an eight-reader CPU fraction to one-reader
  wall time as a decode-cost measurement. Profile the target workload itself.
- **X4:** commit batching is justified as an experiment, but the fsync share is
  still inferred. CacheWrite pages and commit counts are useful; they do not
  themselves measure sync duration or physical device write amplification.
- **X5/X6:** the underlying append cost and commit cost need isolation before
  assigning confidence to the throughput ranges.
- **X8:** the table uses historical pre-dictionary file sizes. Establish the
  current baseline and identical series/sample cohorts before projecting digest
  savings. Measure actual b-tree pages rather than assuming byte savings become
  file savings.
- **X9:** the fraction formula assumes a distribution of block phases relative
  to buckets. Aligned discrete scrape/block grids can give a different fraction.
  Measure the intended alignment and query workload; do not present 34% as a
  universal one-hour limit.

## A shorter implementation sequence

| round | work | gate |
|---|---|---|
| 1 | correct instruments and baseline; fairness, recoverable failure isolation and active-work bounds | trustworthy counters and operational tests |
| 2 | posting counters, label validation without discarded JSON, writer statements, one UPDATE, incremental head | less work per operation with unchanged correctness contracts |
| 3 | bounded batched reads and maintenance publication | compare CPU, mutex delay, RSS, page writes, throughput and latency on identical inputs |
| 4 | decide on Raw execution, group-commit actor and further layout changes from the remaining profile | demonstrable benefit sufficient to pay for added machinery |

Define aggregate arithmetic separately before implementing summary shortcuts.
The expected result is a shared runtime extracted from demonstrated needs across
engines. The next large saving should come from doing fewer operations, not from
requiring every proposed runtime abstraction up front.
