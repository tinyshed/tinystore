# What a late record is behind — 2026-09-25

Follow-up: [the late round of 26 September](records-late-2026-09-26.md) measures
a record against the newest its stream showed before it and calls it late ten
seconds behind: one-second reads of the late fixture fetch 2.93 blocks at
every batch size.

A record goes to its stream's late head when it lags the stream by more than
a minute. [The engine round](records-engine-2026-09-25.md) measured the stream
by the median of the batch the record came in, as the research did, and
found a record appended alone never late: the median of one record is that
record. This round measures the stream by the newest record it has shown,
in the batch or waiting on time in its head, on the late fixture at four ways
of appending it.

## Environment and reproduction

The engine and its runner at the commit that adds this report, on the engine
round's machine and container: AMD Ryzen 7 7700, Docker Desktop 29.6.2,
`golang:1.27` (go1.27.1 linux/amd64), the store on a tmpfs, 1 KiB pages. The
late fixture is the research's frontend fixture with one record in a hundred
moved up to ten minutes into the past, seeded as the research seeded it.

```sh
docker run --rm -v <repo>:/src -v <module cache>:/go/pkg/mod:ro -w /src/bench/records \
  -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS=-mod=readonly -e GOPROXY=off \
  --tmpfs /data:rw,size=6g golang:1.27 sh -c 'go build -o /tmp/records . &&
  /tmp/records -stage late -records 200000 -batch 1 -dir /data/a &&
  /tmp/records -stage late -records 200000 -batch 1024 -dir /data/b &&
  /tmp/records -stage late -batch 1024 -dir /data/c &&
  /tmp/records -stage late -batch 16384 -dir /data/d'
```

The median column ran the same commands on the engine round's commit,
`49dc7b1`.

## Blocks a one-second read fetches

| Appended | Records | By the batch's median | By the stream's newest |
|---|---:|---:|---:|
| one at a time | 200,000 | 8.75 | 4.68 |
| 1,024 at a time, a handler's flush | 200,000 | 4.81 | 4.75 |
| 1,024 at a time | 1,000,000 | 5.26 | 5.24 |
| 16,384 at a time, the research's batches | 1,000,000 | 5.12 | 4.63 |

Appended one at a time, no record was late by its median, and the widest
block spanned 9 minutes 59 seconds; by the newest, 6 minutes 55, as at 1,024.
A batch's median sits in its middle, so a long batch let records a little
under a minute behind its newest stay on time and stretch their blocks; the
newest ends that. The files are within 0.1 % of each other, and the production
corpus, whose receive times only grow within a container, is byte for byte the
same: 20.8984 bytes a record.

## What this settles, and what it leaves

- A record is late when it lags the newest record its stream has shown, in its
  batch or waiting on time in its head. The newest is kept in memory and
  forgotten when the head seals empty, so a record from a wrong clock sends
  its stream to the late head for one head at most, and a restart starts from
  the batches again.
- Appended in a handler's flushes of 1,024, one-second reads fetch 5.24
  blocks, above the 5.2 the hand-off set in the research's batches, where they
  now fetch 4.63. The minute itself is the lever left: a record under it
  stretches its block.
