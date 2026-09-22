# Reports

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
