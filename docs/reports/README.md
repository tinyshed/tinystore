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
| [records-layout-2026-09-24.md](records-layout-2026-09-24.md) | bytes a log record costs as rows, with FTS5, with dictionaries and in zstd blocks, FTS5 per block, and level masks with bloom filters, on a synthetic corpus |
| [loghub-templates-2026-09-24.md](loghub-templates-2026-09-24.md) | Drain-style templates against plain zstd on ten Loghub samples, every line decoded byte for byte |
| [record-events-2026-09-25.md](record-events-2026-09-25.md) | normalized events and logs, bounded shapes, context dictionaries and exact reconstruction, against block and segment zstd |
| [record-reconstruction-2026-09-25.md](record-reconstruction-2026-09-25.md) | lossless 9.8304 B/record in the complete SQLite file on a fixed frontend fixture, candidate ablations and a real GitHub-event control |
| [record-speed-2026-09-25.md](record-speed-2026-09-25.md) | one-CPU codec throughput and cumulative allocations on the fixed frontend fixture; the current encoder's high search cost |
| [record-v2-2026-09-25.md](record-v2-2026-09-25.md) | event-time order, segment and block rows, screened predictions: 7.85 B/record in the file at one million records, a ~480,000 records/s encoder, and query cost for time, level, attribute, session and trace |
| [record-docker-logs-2026-09-25.md](record-docker-logs-2026-09-25.md) | v2 on 1.32 million production container log lines: 17.75 B/record for structured services as slog records, text against zstd by compression scope, and query pruning on real data |
| [record-sealing-2026-09-25.md](record-sealing-2026-09-25.md) | when a head seals on a quiet fleet, what head flushes cost, late records and a late head, blooms for id-like attributes, and one segment's memory |
| [records-research-2026-09-25.md](records-research-2026-09-25.md) | closes the records research: each decision with its evidence, what the first engine takes and leaves, what it must build, its gates and the numbers it must hold |
| [records-engine-2026-09-25.md](records-engine-2026-09-25.md) | the engine on the research's fixtures and production corpus: 7.89 B/record frontend, 20.90 production without the text sample, the speed, reads and late-record gates, and what hourly sealing costs |
| [records-late-reference-2026-09-25.md](records-late-reference-2026-09-25.md) | a late record measured against its stream's newest record rather than its batch's median: a record appended alone is late again, 4.63 blocks a one-second read in the research's batches |
| [records-rice-2026-09-26.md](records-rice-2026-09-26.md) | the rice parameter chosen among all 64: the production corpus at 20.77 B/record in full segments and 22.83 sealed hourly, the frontend fixture unchanged, the speed within this machine's spread |
| [records-stamps-2026-09-26.md](records-stamps-2026-09-26.md) | a line's own time kept behind its record's: the production corpus at 16.55 B/record in full segments and 18.93 sealed hourly, text from 20.02 to 15.48, the frontend fixture and its reads unchanged, about 1.2 µs a text line to find its times |
| [records-merge-2026-09-26.md](records-merge-2026-09-26.md) | a quiet stream's small segments merged four of a size, each keeping its place for Follow: the corpus sealed hourly at 16.77 B/record against 18.93, every record written again 1.53 times, the operator's queries and a follower behind by the whole corpus measured |
| [records-spans-2026-09-26.md](records-spans-2026-09-26.md) | the time index walked within each width of block: a one-second read at the start of ten million records from 2.16 to 1.31 ms, the same as at their end, the blocks fetched unchanged, a byte a block in the file |
| [records-late-2026-09-26.md](records-late-2026-09-26.md) | a late record measured against the newest its stream showed before it and late at ten seconds: one-second reads of the late fixture at 2.93 blocks in flushes of 1024 and batches of 16,384 alike, against 5.24 and 4.63 |
| [records-lines-2026-09-26.md](records-lines-2026-09-26.md) | another program's lines through a writer that joins stack traces and finds levels: the corpus's 1.33 million entries as 1.06 million records, 85 % with a level, 5.3 % less file, every container's errors in 31 blocks |
| [records-load-2026-09-26.md](records-load-2026-09-26.md) | one-second reads beside a writer appending and sealing: p50 1.67 to 1.86 ms and p99 2.69 to 3.73 ms with two readers, under 67 MiB resident and a write-ahead log near a megabyte |
| [records-follow-2026-09-26.md](records-follow-2026-09-26.md) | a follower far behind keeps the blocks it fetched, block ids never given twice and a place building only its rows: the whole corpus followed fetching 21.4 MB against 81.2, in 2.56 s against 2.95 |
| [records-levels-2026-09-26.md](records-levels-2026-09-26.md) | levels found in colour, zerolog's letters and Redis's marks, and logfmt lines kept as their pairs: the corpus's records without a level from 14.9 % to 2.0 %, 30,521 logfmt lines found by their pairs, the file 0.1 % larger |
| [records-text-2026-09-26.md](records-text-2026-09-26.md) | text templates with typed numbers measured against zstd on the corpus, 10.61 to 11.38 B/record against 9.37, and not built; a segment's text at the better-compression level: the corpus at 16.24 B/record in full segments, the speed gates unchanged |

## 26 September 2026 — kv

| | |
|---|---|
| [kv-mechanics-2026-09-26.md](kv-mechanics-2026-09-26.md) | what kv's mechanics cost before the engine exists: durable Sets a transaction each against a group, point Gets with and without View's transaction, a LoseAtMost counter's flush, the file by object at 1 and 4 KiB pages, Clear, and production request rates as aggregates |
| [kv-engine-2026-09-26.md](kv-engine-2026-09-26.md) | the kv engine on its five cases against hand-written tables, and bare reads and writes against its prototype: grouped Sets at 43,002 a second against 53,338, reads at 289,717 against 244,127, every case 25 to 190 times the tables at 512 goroutines |

## 27 September 2026 — jobs

| | |
|---|---|
| [jobs-mechanics-2026-09-27.md](jobs-mechanics-2026-09-27.md) | what jobs' mechanics cost before the storage is settled: a million jobs due in one minute drained from four layouts, where a lease lives, the file by object, values in the row and spilled, grouped Enqueues, a Work loop and its wake-up, a handler's own writes, and raw Claim and Ack |
| [jobs-engine-2026-09-27.md](jobs-engine-2026-09-27.md) | the engine under a burst: a Work loop that wrote twice for a job fixed, two jobs a worker, 350 to 41,820 jobs a second from 1 to 512 workers; a random key halving a burst, keys kept apart in a prototype 2.6 times faster; a million-job queue opened in 110 ms; sqldb's grouped inserts at 63,727 a second |

## 27 September 2026 — blobs

| | |
|---|---|
| [blobs-mechanics-2026-09-27.md](blobs-mechanics-2026-09-27.md) | where a blob's bytes go before the engine exists, on Linux and Windows: inline up to 16 KiB beats a file both ways, a file above it; a reader keeps its bytes through a delete only when opened through an `os.Root` on Windows; a pack gains 1.3 to 1.9 times on Linux and loses on Windows; a million files created steadily, linked faster than copied; a checked whole read at 2 GB/s; a large upload synced every 256 MiB |
| [blobs-engine-2026-09-27.md](blobs-engine-2026-09-27.md) | the public blobs engine beside its prototype on Windows and Linux: placement, 4 GiB streaming writes, concurrent uploads, readers beside scrub, and Usage over 100,000 objects; raw logs and reproduction commands |
| [blobs-write-2026-09-27.md](blobs-write-2026-09-27.md) | the Windows write gap profiled: rooted creation and rename account for 97% of summed syscall delay; admitting one at a time raises concurrent 64 KiB writes 4.3 to 6.8 times in paired runs, preserving file syncs, publication and recovery |
| [blobs-completion-2026-09-27.md](blobs-completion-2026-09-27.md) | long Put gets an optional memory-reserved 64 KiB buffer: 23% less time on Windows and 9–11% on Linux; mixed reads and mutations, actual scheduled scrub, forced Windows rename retries, 64 concurrent large uploads within finite budgets, a million-object Usage, and cold or uncached read diagnostics |

## 27 September 2026 — readability and diagnostics

| | |
|---|---|
| [readability-2026-09-27.md](readability-2026-09-27.md) | four complete engine reads, conservative comment cleanup and two metrics names clarified; independent engineering verdicts, existing shared mechanics and candidates, and an audit of missing diagnostic events and background-log scope |
| [logging-2026-09-27.md](logging-2026-09-27.md) | background engine identity and records self-log routing, committed metrics quarantine events, per-engine Debug maintenance summaries, KV error phase and bucket, records drop reasons, and failed Close outcome |
| [modernize-2026-09-27.md](modernize-2026-09-27.md) | Go 1.27 modernizers across the root and selected bench modules, useful `AsType` without disabling errcheck, one fixed example race, a dead constant, and triage of GoLand warnings |

## 28 September 2026 — sqldb

| | |
|---|---|
| [sqldb-mechanics-2026-09-28.md](sqldb-mechanics-2026-09-28.md) | what sqldb's mechanics cost before its first full version, on Windows and in the container: a point read as a prepared statement without a transaction 38 to 73 % above today's path; `limit ?` compiled every call, 11 to 14 µs a page; a cache holding the texts in use worth half again, one smaller than them worse than none; a plan per type decoding as fast as by hand; uuids as text 1.64 times the bytes, version 7 five to six times faster to insert; a parent rebuilt with foreign keys on deleting all its children |
| [sqldb-engine-2026-09-28.md](sqldb-engine-2026-09-28.md) | the built sqldb: `One[Note]` at 86 to 95 % of a statement scanned by hand, 23 to 111 % above the transaction path it replaced; against `database/sql` as a program opens it, 24 to 206 % more reads and 32 to 116 times the writes from 64 and 512 goroutines, which failed with `database is locked` there; `Insert` returning only what the database generated; a cache door not built |
| [records-read-review-2026-09-28.md](records-read-review-2026-09-28.md) | bounded candidate walks for records pages, measured against the previous engine in two alternating Linux container passes; broad pages use much less time and memory, with the cost of two indexes shown per object |
| [rpc-mechanics-2026-09-28.md](rpc-mechanics-2026-09-28.md) | what reaching a sidecar costs from Go, Bun and Python, on Windows and in the container: a named pipe the fastest on Windows, a `Get` in 31 to 43 µs; one call in flight a round trip, 256 the sidecar's ceiling for every client; the first sender writing a connection's frames; the sidecar held back by stacks a goroutine a call grows and a collector at its default target, and at the embedded rate with 256 workers and GOGC 400; 1 MiB of credit a stream; WSL2's latency its virtual machine's |

## 27 September 2026 — kv after its review

| | |
|---|---|
| [kv-review-fixes-2026-09-27.md](kv-review-fixes-2026-09-27.md) | the review's seven defects fixed, each under a test the reviewed code fails; a hot counter 1.5 to 2.8 times faster through an atomic gate and fewer allocations; a read beside a thousand Clear marks from 146.9 µs to 10.8 through lookups of its own branches, their prefix lengths packed in one parameter; the writer's page cache at 1 to 16 MiB left to decide |

## 29 September 2026 — the server

| | |
|---|---|
| [rpc-server-2026-09-29.md](rpc-server-2026-09-29.md) | the built server through `tinystore serve` beside the prototype, in the same session, on Windows and in the container: at depth 267 to 331 thousand gets a second from Go and Bun on Windows where the prototype as its round ran it served 150 to 268, and 68 to 78 % of its best sidecar, at up to twice its CPU a get; one call in flight the round trip's; sets the group's; a point read whose context can end 27 to 35 % slower embedded, the goroutine `database/sql` starts to watch it |

## 29 September 2026 — comments

| | |
|---|---|
| [comment-cleanup-2026-09-29.md](comment-cleanup-2026-09-29.md) | every comment read once: about 8 % needed an edit, mostly run-on sentences and restated names, and fifteen were stale, one on the SQL check's security boundary; width 80, field notes on their fields; both rounds done and tests green, nothing committed |
