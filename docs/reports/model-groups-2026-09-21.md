# Decimal models and independent block groups

Same pinned NAB subset and normalization as [the first corpus run](nab-2026-09-21.md):
24 series, 137256 samples. Ryzen 7 7700, Docker linux/amd64, Go 1.27.1,
SQLite driver 1.59.0, klauspost/compress 1.19.0.

## The model worked

For each scale k, compute q = round(value * 10^k), encode q with the existing
codec, and encode the signed distance between the ordered IEEE bit patterns
of the original value and q / 10^k. Reconstruction adds this distance as integer
bits, never as floating-point arithmetic. Values outside finite, exactly
representable integer q fall back to the existing codec. The original first
value remains in the external head; the model derives its integer seed from it.

The correction stream is signed varints, optionally zstd-compressed. A model
block includes its mode, scale, compression flag, base-stream length, nested
codec body and an additional CRC over original head plus complete model body.
All these bytes are measured. The control also pays a one-byte mode wrapper.

| Payload, B/sample | Result |
|---|---:|
| Existing codec plus wrapper | 3.9124 |
| Best scale 0..9 independently for every block | 2.4524 |
| Scale learned from the first block of each series | 2.4640 |

The learned variant searches all scales only on the first block; later blocks
try that scale and keep the original codec if smaller. No future samples are
used to select the scale. The scale is stored in every model block, so decoding
has no dependency on preceding blocks. If the first block selects the original
codec, that series continues with the original codec in this experiment.

405 of 580 blocks select a model under exhaustive search. Scales 3 and 8
account for 360 model selections. Examples (control includes the wrapper):

| Series | Before | Model, B/sample |
|---|---:|---:|
| ec2_cpu_utilization_5f5533 | 5.5841 | 2.4127 |
| ec2_cpu_utilization_ac20cd | 5.2180 | 2.3723 |
| cpu_utilization_asg_misconfiguration | 5.4058 | 2.6510 |
| machine_temperature_system_failure | 6.6352 | 4.2894 |

## Whole SQLite files

Each physical group belongs to one series and contains 1, 8 or 16 independently
decodable microblocks of at most 240 samples. The parent has the same gauge
summary columns and retention state as the control. For grouped rows, a separate
directory in the metadata table retains every microblock's start, end, count,
exact first bits, min, max, sum, last, byte offset and byte length. Its fixed-width
columns are transposed and zstd-compressed; count and CRC are included. Payload
bodies are concatenated without cross-block compression. Group-directory raw
size is at most 1280 bytes. This experiment covers gauges, not counter summary
composition.

Both layouts include the minimal registry and index from the prior corpus run.
Insertion is ordered by series in one transaction; all measurements follow a
truncate checkpoint. The full dataset is read back from SQLite and every
timestamp and float bit is checked, in all nine layout/model combinations.

| Encoding | Group size | File bytes | B/sample |
|---|---:|---:|---:|
| Current | 1 | 700416 | 5.1030 |
| Current | 8 | 651264 | 4.7449 |
| Current | 16 | 638976 | 4.6554 |
| Exhaustive model | 1 | 466944 | 3.4020 |
| Exhaustive model | 8 | 446464 | 3.2528 |
| Exhaustive model | 16 | 438272 | 3.1931 |
| Learned scale | 1 | 471040 | 3.4318 |
| Learned scale | 8 | 446464 | 3.2528 |
| Learned scale | 16 | 438272 | 3.1931 |

Learning once retains almost the whole compression benefit. The practical
first candidate is the model without grouping: 32.8% less file than the control.
Grouping 16 brings the total reduction to 37.4%, but its costs are separate.

## Costs and limits

Instrumented encoding over the corpus: existing codec about 23 ms, exhaustive
search adds 393 ms, first-block learning adds 42 ms. Those are one-run diagnostic
timings: roughly 18x and 2.8x total encoding cost respectively, not established
ingest throughput. Encoding runs outside the SQLite insertion timing.

One known point of the first series, 200 warm SQL reads with complete selected
microblock decoding, gave roughly 23–24 us for ungrouped control/model and
30 us for the learned model grouped by 16. The latter fetched 4062 bytes of
directory plus payload, versus 211 for the same ungrouped model: about 19x.
Only one microblock's samples are decoded, but all physical group bytes are
fetched. These are not cold-cache or range-query benchmarks.

A whole-file summary scan falls from about 50 us to 13 us with 16-block groups.
Per-microblock summaries remain in the directory; arbitrary partial-group
summary query latency has not been measured. Physical reclamation now waits for
the group, so retention slack increases. WAL under incremental ingest, ongoing
retention, crash recovery, mixed insertion order and a bounded compactor have
not been measured. This is a prototype, not a production format or migration.
Its parser has checksums but has not been hardened or fuzzed on malformed input.

The previous VM run used 596047 logical bytes including 242574 cache bytes;
its data and metadata total was 353473 bytes. Our 438272-byte best prototype is
smaller than that complete directory, but larger than VM data plus metadata.
The VM HTTP round trip also changed 13872 float bit patterns. Consequently this
does not establish a general win over VM or an equivalent-precision engine
comparison. It establishes a substantial lossless reduction on identical input.

## Reproduce

```powershell
docker run --rm -v <repo>:/src -v <corpus>:/corpus -v tinystore-gocache:/go -w /src -e TINYSTORE_CORPUS=/corpus -e GOCACHE=/go/build-cache golang:1.27 go test ./spike -run '^TestModelAndGroupsOnCorpus$' -v -count=1
```

Fetch the pinned corpus first with `bench/run-nab.ps1` if needed. The new code
is only in `spike/model_group_spike_test.go`; production codec changes from the
previous task are preserved. No dependency or released format changed.
