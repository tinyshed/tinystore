# The gate: the same census on telemetry nobody shaped for us

[The value-structure round](value-structure-2026-09-22.md) found that 40% of a
TSBS payload sits in series that are exact functions of their siblings, and
ended with one obligation: TSBS is modelled on telegraf, so the finding had to
be repeated on a corpus a real agent produced. This is that measurement, and it
takes the prize down by a factor of four — while turning up a larger one
somewhere else, and a limit that real data simply breaks.

The research lives in `spike/`. One thing it found is
built and measured, at the end of this file: the registry, which turned out to
be a third of a real file.

## The corpus

Five Ubuntu servers belonging to the author of this repository — one four-core
and four smaller, two of them running 17 and 27 real containers. A stock
`telegraf:alpine` container ran on each host for 24.8 minutes, reading the
host's `/proc` and `/sys` read-only and the Docker socket read-only, capped at
half a core and 512 MB, writing influx line protocol to a file. Nothing was
installed on any host: the container, its image and its directory were removed
afterwards and each host reported itself clean.

Host inputs ran at one second, the Docker input at two:

```text
cpu (per cpu and total: raw jiffies and the percentages telegraf derives)
mem  swap  system  processes  kernel  kernel_vmstat  linux_sysctl_fs
nstat  netstat  net  interrupts  conntrack  disk  diskio  temp  internal
docker, and docker_container_cpu / mem / net / blkio, per container
```

**4435 series, 4 939 641 samples**, 513 137 protocol lines, 886 distinct label
pairs, 4 316 070 integer and 623 571 floating values; 84 642 string and boolean
fields a real agent also writes were left out. TSBS, for comparison, is 2020
series and 5 090 400 samples — the two corpora are the same size and nothing
else about them is the same.

The corpus is not in the repository and will not be: it carries the hosts'
container names and mount points, and a corpus never enters this repository
anyway. `bench/telegraf` converts the line protocol into the same normalized
JSONL the TSBS runner writes, so one harness reads both corpora, and it checks
that every value survives its own text form before the corpus is used.

## What the search actually does

The instrument has no field names in it, because an engine cannot have any.
For every series it:

1. groups series by **labels** — everything except `__name__`, so the host, the
   disk, the mount point, the container — which is "what one producer wrote
   about one thing";
2. takes every field in that group written at the same instants as candidates;
3. offers the target every candidate under a fixed set of algebraic forms: a
   copy, a negation, a complement, a ratio, a sum, a difference, a ratio of two
   deltas, and the same forms with the constant **fitted from the data** rather
   than read out of a sibling;
4. also grows, greedily, the set of fields that adds up with the target to
   something that does not move — the parts of a whole, which no pair of fields
   can see;
5. accepts a relation only when prediction plus a stored correction reproduces
   **every bit of every sample**, and stores the correction as a distance in
   units in the last place, so the predictor never has to be the producer's own
   expression.

Two guards keep a quiet window from inventing relations: a target must take at
least sixteen distinct values, and every fitted constant is taken from the first
quarter of the series and then has to survive the other three. Relations with no
fitted parameter are reported apart from relations with one, because only the
second kind can be a coincidence a longer window would break.

One rule decides the size of the prize: **a relation among k fields saves one
field, not k−1.** A predicted field may not itself be used as a prediction, so
what a group of mutually derivable fields yields is its rank deficiency. Ten
`mem` fields of rank three give seven; eleven CPU percentages with one
constraint between them give one.

## The answer

| | TSBS | real telegraf | real, without the agent's own metrics |
|---|---:|---:|---:|
| series | 2020 | 4435 | 3720 |
| samples | 5 090 400 | 4 939 641 | 3 866 855 |
| payload now, B/sample | 0.5739 | 0.5729 | 0.5727 |
| with relations | **0.3306** | **0.5172** | **0.5083** |
| smaller by | **42.4%** | **9.7%** | **11.2%** |
| of that, structural | 12.95% | 4.10% | 4.01% |
| of that, a fitted constant | 30.26% | 6.14% | 7.84% |

The two corpora agree on what a payload costs to four decimal places and
disagree by a factor of four on how much of it is derivable. **TSBS overstates
this mechanism about four times over.** The last column exists because telegraf
measures itself: `internal_*` is 18% of the real payload, and removing it moves
the answer by one and a half points rather than changing it.

## What it found, knowing no names

```text
cpu_usage_idle          <- 100 − cpu_usage_active            26 series, 12 exact
internal_memstats_heap_alloc_bytes == internal_memstats_alloc_bytes   35 series
docker_container_mem_usage_percent <- usage / limit × 100    34 series, all exact
docker_container_mem_usage_percent <- usage × a constant     20 series, 18 exact
soft_interrupts_total   <- CPU0 + CPU1                        6 series
soft_interrupts_total   <- CPU2 + a constant                  2 series
internal_write_metrics_written <- added − buffer_size         4 series
mem_write_back          <- kernel_vmstat_nr_writeback × 4096  1 series
```

The first line is the one the previous round predicted. `usage_active` and
`usage_idle` sum to a hundred, and **on 14 of 26 series they do not do it bit
for bit** — the producer's rounding lands one place away. A rule demanding the
exact expression keeps twelve and loses fourteen; the correction stream keeps
all twenty-six, costs a three-symbol alphabet, and is exact by construction.
The last line is a relation across two measurements: a kernel counter in pages
against the same quantity in bytes.

What it did **not** find is as informative. `mem_used_percent` on real telegraf
is not `used / total × 100`: gopsutil computes it from `available`, so the pair
`used_percent, available_percent` is the relation and `used_percent, used` is
not. An instrument built out of field names would have looked for the wrong one.

## The short window was inventing relations, and the long one killed them

A five-minute slice of the same capture was measured first. On it the search
claimed `docker_container_net_tx_packets` was an affine function of
`net_tx_bytes` across 34 series — true, and true only while every packet is the
same size. On the 24.8-minute capture that claim collapses to a single series:

| form | 5 minutes | 24.8 minutes |
|---|---:|---:|
| affine | 34 series, 0.34% of payload | 1 series, 0.02% |
| complement with a fitted constant | 27 series | 26 series |
| ratio to a constant field | 26 series | 34 series |

The forms with nothing fitted did not move. This is the guard working, and it is
the reason the structural and fitted columns are reported separately above.

## Two ideas stayed dead

Predicting one instance of a field from another, on the same 4.29 M samples that
share an instant:

| stream | bits a sample |
|---|---:|
| own delta | **2.004** |
| own delta of delta | 2.386 |
| residual against another host at the same instant | 4.413 |
| the delta of that residual | 2.255 |

Subtracting a neighbour costs 2.2 times the delta it replaces, and the mean
correlation between two hosts' deltas is **0.0049** over 2507 pairs — on TSBS it
was 0.359, and even that was not enough. A second difference is again worse than
the first. Both ideas are now dead on a synthetic corpus and on a real one.

## One idea generalises

A Huffman table shared by a field's instances, stored once and referenced the
way a clock object already is. Taking the smaller of the current body and the
shared-table body, block by block, tables included:

| | TSBS | real telegraf |
|---|---:|---:|
| current codec, values only | 0.5389 | 0.4788 |
| current, or the shared table when smaller | 0.5176 | 0.4587 |
| gain | **3.95%** | **4.2%** |
| blocks that chose it | 11 652 | 4 808 |
| all tables together | 1 504 B | 10 147 B |

It is the only value-side mechanism in either round that lands on the same
number on both corpora. Small, and it does not depend on what the data is.

What is left between the blocks we already write: compressing the concatenation
of every body of a field takes the real corpus from 2 829 977 to 2 532 236
bytes, **10.5% of redundancy**, against 7.0% on TSBS — recoverable only by
giving up independently decodable blocks, and the shared table already takes
four points of it while keeping them.

## The other half of the file, which is the whole story on real data

An ideal coder over own deltas would take the real payload from 0.5729 to
0.3914 B/sample, and to 0.3666 with every derivable field dropped. That is the
value side, and it is not where this corpus' bytes are.

Canonical labels are still stored as text. On the real corpus:

| | total | per series |
|---|---:|---:|
| canonical label text | 1 872 409 B | 422.2 |
| the same as sorted dictionary ids | 48 463 B | **10.9** |
| the dictionary behind them | 38 947 B over 886 pairs | |

Content is not a file, so the registry was built four ways in SQLite and
measured with `dbstat`, each one checked by reconstructing every series'
canonical labels exactly:

| registry layout | file | B/series |
|---|---:|---:|
| labels as canonical text, digest as base64 text — today | 2 863 104 | 645.6 |
| labels as dictionary ids | 671 744 | 151.5 |
| and the digest as a blob rather than base64 | 557 056 | 125.6 |
| and the digest truncated to 16 bytes | **409 600** | **92.4** |

**Seven times smaller.** The identity invariant survives every step: a digest is
a lookup key, the engine already compares full canonical labels on a match, and
an id list reconstructs those labels exactly — which is what the test checks
rather than assumes. Truncating the digest is safe for the same reason: a
collision is resolved, not fatal.

## The engine, end to end, on real telemetry

The first time the public engine has been measured on something a real agent
produced. Ingest, Maintain, Close, measure, reopen, read every series back and
compare every bit:

```text
series=4377  samples=4 917 833  file=7 360 512 B  B/sample=1.496698
readback after reopen: every timestamp and every float bit matched
```

| object | bytes | share |
|---|---:|---:|
| payloads | 2 351 104 | 31.9% |
| series — the canonical labels | 2 338 816 | 31.8% |
| series_state — the packed heads | 1 241 088 | 16.9% |
| groups | 544 768 | 7.4% |
| postings | 413 696 | 5.6% |
| the unique index over identity | 249 856 | 3.4% |
| label_values and its index | 102 400 | 1.4% |
| everything else | 118 784 | 1.6% |

On TSBS the same engine is 1.0018 B/sample. Real telemetry is worse per sample
for a reason that has nothing to do with the codec: its series are 1123 samples
long instead of 2520, so everything paid once per series is paid twice as often.

**The registry and its index are 35.2% of this file.** Carrying the measured
layout over is arithmetic rather than a run — page slack does not scale
linearly — but it is 2 588 672 bytes against about 299 008, and it takes the
file to roughly **1.03 B/sample, 31% off, from one change that stores no fewer
facts.**

## A limit that real data breaks

The engine refused 58 of the 4435 series outright:

```text
invalid metrics request: expected 1..32 labels
```

They carry 36 and 37 labels, they are all container series, and the labels are
the image's own — `com.docker.compose.*`, `io.k8s.*`, `vcs-ref`, `maintainer`,
`summary`, `url`. Telegraf turns every one of them into a tag. The label counts
in the corpus are 2 to 4 for host metrics, 17 to 29 for most container metrics,
and 36 to 37 for the rest; TSBS has 10 to 14 for everything, so the benchmark
could never have found this.

`maxLabels = 32` in [registry.go:17](../../metrics/registry.go:17) is not a
degradation, it is a refusal, and it makes the engine unable to ingest a stock
telegraf watching stock containers. The engine measurement above is on a corpus
with those 58 series left out, which is 1.3% of series and 0.44% of samples.

## Where the real payload sits

| field | series | B/sample | share of payload |
|---|---:|---:|---:|
| `internal_gather_gather_time_ns` | 90 | 2.5851 | 12.33% |
| `docker_container_cpu_usage_percent` | 52 | 6.4805 | 4.53% |
| `cpu_usage_active` | 14 | 5.3665 | 3.98% |
| `cpu_usage_idle` | 14 | 5.2801 | 3.92% |
| `cpu_usage_user` | 14 | 5.0529 | 3.75% |
| `cpu_usage_system` | 14 | 4.3166 | 3.20% |
| `docker_container_cpu_usage_system` | 52 | 4.2983 | 3.00% |

The shape is the one TSBS showed: **floating-point percentages are the expensive
fields**, at four to seven bytes a sample against the 0.2 to 0.5 a counter
costs. What differs is that on real telemetry most of them are not a function of
one sibling — `usage_user` is a share of the sum of ten deltas — so the
mechanism reaches far fewer of them.

## What to do, ordered by what was measured

| change | effect on the real file | evidence |
|---|---:|---|
| labels as dictionary ids, digest as a short blob | **−31%** | measured in SQLite, four layouts, labels reconstructed exactly |
| raise or remove the 32-label cap | unblocks 1.3% of real series | a refusal, not a cost |
| a shared Huffman table per field family | −1.3% | −4.0% of payload on both corpora |
| value projections — predict from a sibling | −3.1% | −9.7% of payload here against −42.4% on TSBS |

The order is the opposite of the order the first round suggested, and the reason
is that the first round measured a corpus whose series are twice as long and
whose derived fields are four times as dense.

**Value projections should not be built now.** The gate was set at "at least ten
to twenty percent of a real payload" and the answer is 9.7 to 11.2 — the bottom
edge, for the most expensive mechanism on the list: a value block that depends
on another series needs a reference retention cannot break, ingest that delivers
both fields in one batch, a two-block decode budget, a planner that knows a
series may sit behind another, and a cycle rule. A clock object is passive; this
is a dependency graph. If it is ever built, the shape the measurement supports
is the general one — a source, a model, and a correction stream, with no
knowledge of what a percentage is — and a graph one level deep.

## The first two rows, built and measured

The registry change and the label budget are implemented. Migration 0006 adds
one column; a series row stores its dictionary ids as a gap-coded blob instead
of canonical label text, and a query rebuilds the labels from one dictionary
read for the whole match. The identity invariant is unchanged and cheaper to
honour: the ids *are* the labels, so comparing them is the full check a digest
match requires, without a join.

The same corpus, the same test, before and after:

| | before | after |
|---|---:|---:|
| whole file | 7 360 512 B | **5 332 992 B** |
| B/sample | 1.496698 | **1.084419** |
| the `series` b-tree | 2 338 816 | **319 488** |
| ingest and maintain | 19.50 s | 19.58 s |
| reopened bitwise readback | 3.00 s | 3.22 s |

**27.5% off a real file.** The projection said 31%; the difference is the
unique index over identity, which is the second step and is not done. Ingest
did not move: the extra dictionary lookup the identity check now makes is
invisible next to the head rewrite. Reads pay about 7% for the dictionary join.

TSBS, unchanged corpus, for a second point:

| | before | after |
|---|---:|---:|
| whole file | 5 099 520 B | **4 546 560 B** |
| B/sample | 1.001792 | **0.893164** |

The label budget is now four numbers instead of one count — 128 labels, 16 KiB
across a series, 256 bytes for a name, 4 KiB for a value — because a hundred
short labels and thirty enormous ones are different threats and a count cannot
tell them apart. The full real corpus now goes in: 4435 series, 4 939 641
samples, 5 431 296 bytes, 1.099533 B/sample, every bit back after reopen. The
58 container series the engine used to refuse are among them.

What is deliberately **not** done is the identity column. Turning it into a
short binary digest needs its own migration, because SQLite cannot compute
sha256 and the column is `not null unique` — and that constraint is the trap:
a unique index over a truncated digest forbids the very collision the code
promises to resolve by comparing labels. The digest has to become a
non-unique lookup bucket first, and the lookup has to accept several candidates
and pick by label equality. It is worth 249 856 bytes on this corpus; it is not
worth doing carelessly.

## What this does not establish

Five hosts of one owner, 24.8 minutes, one agent, one operating system. Another
exporter writes different fields: node_exporter emits raw counters and almost no
derived ones, so its derivable share would be lower still, while a cloud agent
reporting only percentages would have none of the raw fields a relation needs.
A longer capture would amortise the identity cost the engine measurement is
dominated by, and would give the fitted forms a harder time than 24.8 minutes
did.

The engine number is one run, one machine, and includes a mutable head holding
383 273 samples. No timing here is a throughput result. The file-level
projections are arithmetic over measured objects and are labelled as such.

## Reproduce

The capture, on each host, with nothing installed:

```sh
docker run -d --name tsprobe --user root --entrypoint /usr/bin/telegraf \
  --group-add "$(stat -c '%g' /var/run/docker.sock)" --cpus=0.5 --memory=512m \
  -v /tmp/tsprobe:/probe -v /:/hostfs:ro -v /var/run/docker.sock:/var/run/docker.sock:ro \
  -e HOST_MOUNT_PREFIX=/hostfs -e HOST_PROC=/hostfs/proc \
  -e HOST_SYS=/hostfs/sys -e HOST_ETC=/hostfs/etc \
  telegraf:alpine --config /probe/telegraf.conf
```

with `interval = "1s"`, `precision = "1s"`, an `outputs.file` in influx format,
and the inputs listed above; then

```powershell
go -C bench/telegraf run . -corpus ./bench/corpus/telegraf/series.jsonl ./bench/corpus/telegraf/srv-*.lp
$env:TINYSTORE_SPIKE=1
$env:TINYSTORE_JSONL='<repo>/bench/corpus/telegraf/series.jsonl'
go test ./spike -run '^TestRelationCensus$|^TestCorpusFloor$|^TestIdentityLayouts$|^TestValueBudgetByField$|^TestSharedModelsAgainstCrossSeriesPrediction$' -v -count=1
go -C bench/telegraf run . -corpus ./bench/corpus/telegraf/series-engine.jsonl -max-labels 32 ./bench/corpus/telegraf/srv-*.lp
$env:TINYSTORE_JSONL='<repo>/bench/corpus/telegraf/series-engine.jsonl'
go test ./metrics -run '^TestCorpusThroughPublicStore$' -v -count=1
```

New this round: `bench/telegraf`, `spike/relations_spike_test.go` and
`spike/identity_spike_test.go`. Go 1.27.1 windows/amd64, klauspost/compress
1.19.0, modernc SQLite 1.59.0; byte counts are deterministic and do not depend
on the host. `go test ./...`, `gofmt` and `golangci-lint run ./spike/...` pass.
`task check` was not run, no container run was made, and no commit was made.
