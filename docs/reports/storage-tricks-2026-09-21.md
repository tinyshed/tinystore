# Shared clocks, separate payloads, and one integrity envelope

Same normalized Alibaba and TSBS inputs as [the clock round](clock-2026-09-21.md):
12431885 and 5090400 samples. Ryzen 7 7700, Go 1.27.1 linux/amd64 in Docker,
modernc SQLite 1.59.0, klauspost/compress 1.19.0. Four layout hypotheses run as
parallel subtests on precomputed, immutable blocks. No timing comparison is
claimed from those parallel runs.

## Three mechanisms, then their combination

**One envelope.** The former clock wrapper contained another complete codec
envelope. For the plain value path the new prototype stores its value flags and
stream once, and one CRC binding the original head, clock and value stream.
The old envelope is reconstructed in memory solely to reuse the existing
decoder, after verifying the new checksum. Grid-model bodies retain their
format. Regular timestamps derive from start/end/count and store no clock
bytes. The selector keeps the original representation when smaller.

**Grouped metadata, independent payload rows.** The compact directory covers
8, 16, 32 or 64 independently decodable microblocks. In the separated layout,
each body retains its own SQLite row; the group stores the first payload id
and assigns consecutive ids within that transaction. Directory lengths and
checksums are retained. This decouples metadata amortization from BLOB packing.
The verification reader fetches all bodies to check the full corpus; point
reads and their allocation cost were not benchmarked here.

**Share exact clock streams.** Clock bundles are deduplicated by SHA256 and
complete byte comparison. A real SQLite `clocks` table stores the digest,
compressed-clock bytes, reference count and id; its unique digest index and
every reference are charged. No host-name heuristic or assumed simultaneity
is used. Each value body's checksum binds its original head and referenced
clock bytes, so swapping a clock cannot silently change its timestamps.
Completely regular groups require no clock object. Clocks are sequences of
relative deltas and can be shared whenever the encoded pattern matches.

## Alibaba: whole SQLite files, B/sample

| Layout | Group 8 | Group 16 | Group 32 | Group 64 |
|---|---:|---:|---:|---:|
| Compact envelope, concatenated bodies | 0.513652 | 0.533091 | 0.502450 | 0.552201 |
| Compact envelope, separate bodies | 0.521560 | 0.486306 | 0.470820 | 0.483011 |
| Shared clocks, concatenated bodies | 0.486306 | 0.487624 | 0.449404 | 0.481364 |
| **Shared clocks, separate bodies** | 0.495531 | 0.452370 | **0.434578** | 0.442485 |

The winner is **5402624 bytes**, versus 6832128 in the previous clock round
and 9224192 at the original Alibaba baseline: another 20.9% reduction, or 41.4%
from that original baseline. Group 64 loses, so the chosen result is not an
unbounded increase in group size. The sample decode limit stays at 240.

The simpler separate-body layout without shared-clock ownership costs 5853184
bytes (0.470820). It is a viable trade if the clock registry is not worth the
additional lifecycle complexity.

## TSBS check

With groups of 16 or more, each series fits in one group on this short corpus:

| Layout | Bytes | B/sample |
|---|---:|---:|
| Compact envelope, concatenated bodies | 4968448 | 0.976043 |
| Compact envelope, separate bodies | 5189632 | 1.019494 |
| Shared-clock format, concatenated bodies | **4919296** | **0.966387** |
| Shared-clock format, separate bodies | 5152768 | 1.012252 |

No shared clock objects are needed on regular TSBS. That format's smaller
envelope still helps. One common policy, shared-clock format plus separate
bodies and group 32, gives 0.434578 on Alibaba and 1.012252 on TSBS. The density
winner on TSBS instead concatenates bodies; the measurements do not establish
one universal layout optimum.

## What the comparison does and does not establish

The previously measured VM Alibaba complete directory was 6338675 bytes,
including 3056685 cache bytes. The new SQLite prototype is **14.8% smaller than
that complete directory** and round-trips every input bit. VM's data plus
metadata alone were 3281990 bytes and remain substantially smaller than our
complete SQLite file. VM was not rerun or retuned in this round. Therefore
this is a win over the reported full-directory footprint under that policy,
not a claim that TinyStore's durable data are denser than VM's.

On TSBS the previous VM total was 1.0774 B/sample, but its HTTP path did not
return every original float bit. That precision distinction still applies.

No summaries, label registry, due index, source samples or checksum protection
were removed. Every configuration is read back from SQLite and compared with
every original timestamp, float bit and per-microblock summary. Reference
counts are checked against actual group references. Deleting the first series
removes its payloads and decrements clock ownership in one transaction; the
check then verifies remaining counts, absence of dangling clock references and
the expected number of payload rows. This is a limited lifecycle gate, not a
crash-recovery or concurrent-retention test.

Files are measured after truncate checkpoint, before the deletion probe.
The Alibaba matrix additionally asserts `page_count * page_size == dbstat`
and zero freelist pages. Per-object reports include the clock digest index.

These remain bulk-load prototypes: no live-ingest WAL/RSS measurements, cold
range queries, representative update order, restart tests, parser fuzzing or
production retention implementation. Grouped directories add read/decode work
and complicate partial retention. All validation readbacks currently assemble
whole groups. No production codec or released format changed in this round.

## Reproduce

```powershell
docker run --rm -m 6g -v <repo>:/src -v <corpus>:/corpus -v tinystore-gocache:/go -w /src -e TINYSTORE_JSONL=/corpus/series.jsonl -e GOCACHE=/go/build-cache golang:1.27 go test ./spike -run '^TestStorageTricks$' -parallel 4 -v -count=1
```

Use `tinystore-tsbs` as the corpus directory for the other run. Code is in
`spike/tricks_spike_test.go`, with one experimental dispatch hook in
`spike/clock_spike_test.go`. Normal tests and spike lint passed; full
`task check` was not run.
