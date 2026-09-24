# Reports

The 22–23 September architecture implementation rounds on
`codex/architecture-measurements` measured intermediate uncommitted states.
Implementation revisions run through `7bcc8ec` and harness revisions through
`64ce638`;
individual comparison baselines are provisional until their source states
are archived as reproducible revisions.

Dated measurement rounds, in the order they happened. They are history rather
than documentation: each one records what was measured on a given day, on which
machine, against which corpus, and with the command that reproduces it. A later
round may supersede an earlier one without the earlier one being wrong — it says
what was true when it ran.

What the engine promises today is in [design.md](../design.md),
[format.md](../format.md) and [measurements.md](../measurements.md); what is
still open is in [research.md](../research.md). Read those first. Come here when
you want to check where a number came from.

Reproduction commands use `<repo>` for the repository and `<corpus>` for the
prepared corpus directory. Corpora are never committed; the runners in `bench/`
fetch and normalise them.

## 21 September 2026 — the codec and the corpora

| | |
|---|---|
| [night-log.md](night-log.md) | the running log of one autonomous session, failures included |
| [nab-2026-09-21.md](nab-2026-09-21.md) | first real corpus: NAB |
| [model-groups-2026-09-21.md](model-groups-2026-09-21.md) | decimal models and independent block groups |
| [grid-2026-09-21.md](grid-2026-09-21.md) | residual modes, rational grids, and where the bytes actually are |
| [tsbs-2026-09-21.md](tsbs-2026-09-21.md) | second corpus: TSBS DevOps, against VictoriaMetrics and Prometheus |
| [alibaba-2026-09-21.md](alibaba-2026-09-21.md) | third corpus: the Alibaba cluster trace, and the first irregular timestamps |
| [clock-2026-09-21.md](clock-2026-09-21.md) | the Alibaba clock and metadata follow-up |
| [storage-tricks-2026-09-21.md](storage-tricks-2026-09-21.md) | shared clocks, separate payloads, and one integrity envelope |
| [combined-2026-09-21.md](combined-2026-09-21.md) | the combined storage prototype |
| [review-2026-09-21.md](review-2026-09-21.md) | a review of the overnight measurements, and which conclusions did not follow |

## 21 September 2026 — the first engine

| | |
|---|---|
| [metrics-slice-2026-09-21.md](metrics-slice-2026-09-21.md) | the architecture proposal this engine answers |
| [implementation-2026-09-21.md](implementation-2026-09-21.md) | the first durable metrics slice, and its measured limits |
| [status-2026-09-21.md](status-2026-09-21.md) | where the engine stood against the prototypes, before the packed head |
| [packed-head-2026-09-21.md](packed-head-2026-09-21.md) | the packed head, and the engine approaching the research result |
| [label-dictionary-2026-09-21.md](label-dictionary-2026-09-21.md) | the label dictionary in the real engine |

## 22 September 2026 — values, real telemetry and time

| | |
|---|---|
| [value-structure-2026-09-22.md](value-structure-2026-09-22.md) | what is left in TSBS values, measured |
| [real-corpus-2026-09-22.md](real-corpus-2026-09-22.md) | the same census on telemetry nobody shaped for us |
| [performance-2026-09-22.md](performance-2026-09-22.md) | what the engine costs in time rather than in bytes |
| [selectivity-2026-09-22.md](selectivity-2026-09-22.md) | closing the two defects that round named, and what the fix costs |
| [prepared-reads-2026-09-22.md](prepared-reads-2026-09-22.md) | profiling repeated SQL compilation, bounded statement reuse and the parameterized-limit trap |
| [group-merging-2026-09-22.md](group-merging-2026-09-22.md) | incremental grouping without payload relocation, file accounting and the short-read trade |
| [engine-audit-2026-09-22.md](engine-audit-2026-09-22.md) | remaining costs, reproduced operational limits, aggregate rounding and corrections to RSS/WAL interpretation |
| [execution-review-2026-09-22.md](execution-review-2026-09-22.md) | where the reader plateau waits, per-ingest index churn, writer and maintenance defects, and runtime proposals split into work packages |
| [execution-review-verdict-2026-09-22.md](execution-review-verdict-2026-09-22.md) | decisions and corrections for the execution review's proposed optimization sequence |
| [reader-reuse-2026-09-22.md](reader-reuse-2026-09-22.md) | the first measured change: retaining healthy readers and reusing warm idle connections |
| [maintenance-fairness-2026-09-22.md](maintenance-fairness-2026-09-22.md) | rotating ready-series selection and the two-pass fairness result |
| [maintenance-isolation-2026-09-22.md](maintenance-isolation-2026-09-22.md) | persisted per-series maintenance failures, bounded retry and recovery after a limit change |
| [architecture-instruments-2026-09-22.md](architecture-instruments-2026-09-22.md) | corrected RSS/WAL labels and separated registration, append and rotating-key read baselines |
| [ingest-state-2026-09-22.md](ingest-state-2026-09-22.md) | one state read and UPDATE per packed-head ingest, with measured append and registration effects |
| [head-reuse-2026-09-22.md](head-reuse-2026-09-22.md) | reuse of unchanged encoded head chunks, bit-exact tests and long-head append measurements |
| [architecture-worklist-2026-09-22.md](architecture-worklist-2026-09-22.md) | completed and open work packages from the audit, review and verdict |
| [posting-counts-2026-09-22.md](posting-counts-2026-09-22.md) | exact posting counts, read gains, registration cost and per-object file accounting |
| [read-labels-2026-09-22.md](read-labels-2026-09-22.md) | avoid discarded identity JSON on reads, with fixed, rotating and wide query comparisons |
| [ready-watermark-2026-09-22.md](ready-watermark-2026-09-22.md) | stop maintenance churn until a full prefix is strictly before the watermark |
| [matcher-batching-2026-09-23.md](matcher-batching-2026-09-23.md) | reject universal matcher batching, retain prepared short lookups and batch broad selectors |
| [batched-head-reads-2026-09-23.md](batched-head-reads-2026-09-23.md) | reserve head bytes before bounded batch fetches, with wide and 20-series read comparisons |
| [batched-group-and-payload-reads-2026-09-23.md](batched-group-and-payload-reads-2026-09-23.md) | bounded selected-payload and group-directory batches, with one- and eight-reader comparisons |
| [ordered-ingest-2026-09-23.md](ordered-ingest-2026-09-23.md) | ordered-input ingest preparation with exact fallback and separate write-stage comparisons |
| [prepared-writes-2026-09-23.md](prepared-writes-2026-09-23.md) | bounded prepared writes on the owning connection and separate append/registration gains |
| [batched-maintenance-2026-09-23.md](batched-maintenance-2026-09-23.md) | byte-bounded savepoint publication and the 64-series maintenance comparison |
| [active-work-admission-2026-09-23.md](active-work-admission-2026-09-23.md) | bound active work per store and measure the wide-read memory/throughput tradeoff |
| [series-reclamation-2026-09-23.md](series-reclamation-2026-09-23.md) | reclaim empty registrations and dictionary ownership, with per-object page accounting |
| [prepared-eviction-2026-09-23.md](prepared-eviction-2026-09-23.md) | retain recently used prepared programs without a measured stable-query QPS change |
| [narrow-head-2026-09-23.md](narrow-head-2026-09-23.md) | validate a whole packed head while decoding only selected chunks and charging their samples |
| [post-batching-profile-2026-09-23.md](post-batching-profile-2026-09-23.md) | fresh fixed-key and wide-read profiles after batching, with D7 and execution-layer decision gates |
| [aggregate-representation-2026-09-23.md](aggregate-representation-2026-09-23.md) | exact sum contract and variable integer, fixed accumulator and expansion measurements on both corpora |
| [final-architecture-benchmark-2026-09-23.md](final-architecture-benchmark-2026-09-23.md) | saved-revision throughput, complete two-corpus three-engine files, remote-write service rates and physical SQLite writes |
| [shared-work-budget-2026-09-23.md](shared-work-budget-2026-09-23.md) | shared active-work admission over two Store handles, cancellation and the RSS/throughput trade |
| [ram-comparison-2026-09-23.md](ram-comparison-2026-09-23.md) | seven process-RSS shapes across TinyStore, VictoriaMetrics and Prometheus on the full TSBS corpus |
| [head-suffix-2026-09-23.md](head-suffix-2026-09-23.md) | long-head suffix-only decode and the append-only head decision |
| [architecture-hypotheses-2026-09-23.md](architecture-hypotheses-2026-09-23.md) | WP9 execution and page/cache gates, WP10 churn, WP12 digest/layout, and value-envelope isolation |
| [exact-aggregates-2026-09-23.md](exact-aggregates-2026-09-23.md) | raw-decoding exact sums and counter transitions, reopened-corpus checks and aggregate read costs |
| [grouping-ceiling-2026-09-23.md](grouping-ceiling-2026-09-23.md) | measured commit, WAL and sync savings of explicit batching before any group-commit actor |
| [records-layout-2026-09-24.md](records-layout-2026-09-24.md) | bytes a log record costs as rows, with FTS5, with dictionaries and in zstd blocks, on a synthetic corpus |
