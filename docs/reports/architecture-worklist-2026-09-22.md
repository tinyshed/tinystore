# Architecture review: implementation and measurement state

This tracks the work packages in [the execution review](execution-review-2026-09-22.md)
against [the verdict](execution-review-verdict-2026-09-22.md). The final
implementation is `ce5564a`, the perf harness is `20f5c93`, and both are on
`codex/architecture-measurements` above `f4568cc`. Individual before/after
runs used transient intermediate working-tree states. Their old binaries and
overlays remain on the local measurement volume, but the intermediate source
states were not committed; treat those dated comparisons as provisional.
An unchecked proposal is not shipped behavior or a measured gain.

| package | done on this branch | still open |
|---|---|---|
| WP1 instruments | corrected RSS/WAL labels, linear batch construction, registration/append split, rotating-key read, fresh read/mixed copies; [round](architecture-instruments-2026-09-22.md) | actual WAL frames, syncs, SQLite cache writes/spills, checkpoint counters, dedicated sealing stage |
| WP2 maintenance | fair ready cursor, local-failure suspension with persisted reason, bounded retry and reopen, and ready only for a sealable prefix; [fairness](maintenance-fairness-2026-09-22.md), [isolation](maintenance-isolation-2026-09-22.md), [watermark](ready-watermark-2026-09-22.md) | cross-store admission remains in WP8 |
| WP3 reader mechanics | healthy reader survives application error, warm idle reuse; [round](reader-reuse-2026-09-22.md) | statement eviction policy, normalized SQL shapes; L1a is benchmark-only until cancellation remains effective |
| WP4 registry reads | transactional exact posting counts, one-matcher skip, over-1024 gate, validation without discarded JSON and batched broad matcher ids; [counts](posting-counts-2026-09-22.md), [labels](read-labels-2026-09-22.md), [batching](matcher-batching-2026-09-23.md) | steady churn cost after WP10 reclamation |
| WP5 raw reads | bounded batched heads, selected payloads and group descriptors after byte reservation; [heads](batched-head-reads-2026-09-23.md), [groups and payloads](batched-group-and-payload-reads-2026-09-23.md) | narrow-head decoding and redundant codec envelope |
| WP6 writer | one state read/UPDATE and unchanged-index exclusion, encoded head prefix reuse, ordered-input fast path and bounded writer statements; [D3](ingest-state-2026-09-22.md), [A11](head-reuse-2026-09-22.md), [ordered input](ordered-ingest-2026-09-23.md), [prepared writes](prepared-writes-2026-09-23.md) | `CacheWrite` and physical write comparison |
| WP7 publication | byte-bounded maintenance batches, per-series savepoints and commit-only accounting; [round](batched-maintenance-2026-09-23.md) | instrument actual commit and sync work before considering further batch sizes |
| WP8 runtime | per-store active read and ingest admission; [round](active-work-admission-2026-09-23.md) | cross-store weighted admission; only then compare a group-commit actor and checkpoint policy |
| WP9 execution comparison | — | `sql.Conn.Raw`, proxy and direct-binding benchmarks; writer cache and page-size comparisons |
| WP10 cardinality | retention reclaims empty series, postings and last-owner dictionary pairs; tests cover kind/frontier lifecycle and shared labels; [round](series-reclamation-2026-09-23.md) | churn cost and file-page economics on a representative rotating-cardinality corpus |
| WP11 aggregates | — | exact sum/counter contract and representation spike before summary execution |
| WP12 density | — | current real-corpus census, binary digest and column layout with per-object `dbstat` |
| WP13 decisions | — | decide append-only head and streaming only after WP6/WP8 profiles |

The next independent decisions need WP1's missing physical write and checkpoint
instruments, plus a rotating-cardinality corpus. WP3's cache eviction,
WP5's narrow-head/codec work, cross-store admission, aggregates and execution
layer comparisons remain open. No broad throughput improvement is inferred
from the old review's expected-effects table.

The current root `task check` passes on Windows, including the portable
`task size` report. The benchmark module separately passed
`go -C bench/perf test ./...`; Linux race tests passed for
`internal/sqlite` and `metrics`.
