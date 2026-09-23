# Architecture review: implementation and measurement state

This tracks the work packages in [the execution review](execution-review-2026-09-22.md)
against [the verdict](execution-review-verdict-2026-09-22.md). The initial
architecture branch was `bfa7232` above `f4568cc`. Follow-up instrumentation,
engine API, representation spike and benchmark harness are saved as `aecd7b2`,
`4589144`, `6a0465e` and `dd4a6e5`. Earlier individual comparisons used
transient working-tree states and remain provisional. The
[saved-revision round](final-architecture-benchmark-2026-09-23.md) repeats
broad before/after stages at `f4568cc` and `bfa7232`; the long-head suffix
gain, aggregate checks and shared-budget comparison were repeated from the
later saved revisions.
An unchecked proposal is not shipped behavior or a measured gain.

| package | done on this branch | still open |
|---|---|---|
| WP1 instruments | corrected RSS/WAL labels, linear batch construction, registration/append split, rotating-key read and fresh copies; actual WAL frames, CacheWrite/Spill, PASSIVE checkpoint counts, commits and Linux sync syscalls for register/append/seal; [round](final-architecture-benchmark-2026-09-23.md) | physical device write amplification and sync duration remain unmeasured |
| WP2 maintenance | fair ready cursor, local-failure suspension with persisted reason, bounded retry and reopen, and ready only for a sealable prefix; [fairness](maintenance-fairness-2026-09-22.md), [isolation](maintenance-isolation-2026-09-22.md), [watermark](ready-watermark-2026-09-22.md) | cross-store admission remains in WP8 |
| WP3 reader mechanics | healthy reader survives application error, warm idle reuse and recently used statement eviction; dynamic SQL shapes measured against a one-shape JSON candidate on both corpora and stress input, with no production normalization justified; [round](architecture-hypotheses-2026-09-23.md) | L1a remains benchmark-only until cancellation remains effective |
| WP4 registry reads | transactional exact posting counts, one-matcher skip, over-1024 gate, validation without discarded JSON and batched broad matcher ids; [counts](posting-counts-2026-09-22.md), [labels](read-labels-2026-09-22.md), [batching](matcher-batching-2026-09-23.md) | real multi-day churn remains unmeasured |
| WP5 raw reads | bounded batched heads, selected payloads and group descriptors after byte reservation, plus selected-chunk decoding for packed heads; envelope cost isolated at about 0.6 µs per 240-point block and left unchanged; [round](architecture-hypotheses-2026-09-23.md) | a future already-verified value-stream decoder still needs a corruption and decode-budget gate |
| WP6 writer | one state read/UPDATE and unchanged-index exclusion, encoded head prefix reuse, ordered-input fast path, bounded writer statements and suffix-only long-head decode; saved-revision ABBA [head round](head-suffix-2026-09-23.md), [physical writes](final-architecture-benchmark-2026-09-23.md) | no append-only migration selected |
| WP7 publication | byte-bounded maintenance batches, per-series savepoints and commit-only accounting; seal stage measured 8 commits, 62 WAL frames/CacheWrite pages and 9 Linux WAL syncs for 64 blocks; [round](final-architecture-benchmark-2026-09-23.md) | further batch sizes need an explicit latency and memory gate |
| WP8 runtime | per-store slots plus optional shared weighted admission across handles, with cancellation/race gate and two-store RSS comparison; [round](shared-work-budget-2026-09-23.md). Group-commit/cancellation rules specified in [contract](../group-commit-contract.md); explicit batching's commit/WAL/sync economics measured in [round](grouping-ceiling-2026-09-23.md) | no group-commit actor ships; caller batching remains the choice until independent low-volume requests justify queue/savepoint complexity. Keep checkpoint policy a separate experiment |
| WP9 execution comparison | `sql.Conn.Raw`, direct and proxy lookup microbenchmarks; 1/4 MiB writer-cache ABBA and 4/8 KiB page comparisons; [round](architecture-hypotheses-2026-09-23.md) | no execution-layer or page/cache migration justified; any future candidate needs real-path cancellation and reuse gates |
| WP10 cardinality | retention reclaims empty registrations and dictionary pairs; 30 synthetic epochs reclaimed 30,000 series and 20 real-Telegraf-label-derived epochs reclaimed 20,000, with first/last latency and per-object file pages; [round](architecture-hypotheses-2026-09-23.md) | an observed multi-day rotating-container capture and the roughly four-second per-1,000-series reclamation cost need further work |
| WP11 aggregates | [numerical and counter contract](../aggregate-contract.md), raw-decoding exact `Aggregate` API with block/reset/retention and real-corpus tests, and [representation spike](aggregate-representation-2026-09-23.md) favor a variable exact integer; [API round](exact-aggregates-2026-09-23.md) | a versioned persisted exact summary and shortcut are not built |
| WP12 density | current TSBS/Telegraf corpus census, binary digest and tail-last candidates with per-object `dbstat` and lookup timings; [round](architecture-hypotheses-2026-09-23.md) | no layout migration justified by 1.1–2.2% file savings and no measured lookup gain |
| WP13 decisions | separate `Stream` callback API added after WP6/WP8 profiles; [RSS round](ram-comparison-2026-09-23.md). Long-head append now decodes only its mutable suffix, retaining the existing format; [round](head-suffix-2026-09-23.md) | no append-only head migration now; an append-only candidate needs to beat suffix decode on the same workload and account for pages and WAL |

The requested aggregate API, group-commit cancellation rules, shared admission,
physical write counters, layout candidates, real-label-derived churn, native
streaming RSS and append-only decision now have explicit evidence. A versioned
exact summary shortcut, an optional group-commit actor and an observed
multi-day churn capture remain future work rather than shipped promises. The
old expected-effects table remains hypotheses, not a broad throughput
forecast.

The current root `task check` passes on Windows, including a platform-correct
`task size` label. The benchmark module passed `go -C bench/perf test ./...`.
Linux `go test -race -shuffle=on ./...` and Darwin amd64/arm64 CGO-free
cross-builds and metrics test-binary compilations passed. Native macOS runtime
and race tests did not run because the GitHub Actions quota was exhausted; the
user explicitly waived that platform run for this round.
