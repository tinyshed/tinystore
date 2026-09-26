# A late record, measured against what came before it — 2026-09-26

A record goes to its stream's late head when it lags the stream, so that it
does not stretch the blocks of the records beside it in time. [The late
reference round](records-late-reference-2026-09-25.md) measured a record
against the newest record its batch held or its head had waiting, and found
one-second reads of the late fixture fetching 5.24 blocks when it arrives in
flushes of 1024, above the 5.2 the hand-off set, and 4.63 in batches of
16,384. The batch was the lever: its newest record includes those that arrived
after the one measured, so an on-time record early in a long batch lagged its
own batch, and the minute had to be long enough to let it through.

A record is now measured against the newest its stream showed before it:
earlier in its batch, or waiting on time in its head, capped by the store's
clock as before. A batch in time order makes none of its own records late,
however long it spans, and the minute can shrink to what a record stretches a
block by. It is ten seconds.

```text
clock 12:01, nothing waiting; batch 11:58:30 11:59:10 12:00:45   → all on time; by the batch's newest, two were late
clock 12:01, web waiting until 12:00:40; batch 12:00:20 alone    → 20 s behind: late
```

The examples are `TestARecordTenSecondsBehindItsStreamsNewestIsLate`.

## Environment and reproduction

- The baseline at `c252738`; the candidate at `ea25a25`, which changes
  `records/` alone, and three builds of it with `lateness` in
  `records/options.go` set to 60, 30 and 5 seconds.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The late fixture of the engine round: the research's frontend fixture with
  one record in a hundred moved up to ten minutes into the past, seeded as the
  research seeded it; the frontend fixture itself; and the private production
  snapshot `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`,
  read as the engine round read it. Aggregates only.

```sh
git -C <repo> archive c252738 | tar -x -C <root>/base
for v in 10s 60s 30s 5s; do git -C <repo> archive ea25a25 | tar -x -C <root>/cand-$v; done
for v in 60 30 5; do sed -i "s/lateness           = 10 \* time.Second/lateness           = $v * time.Second/" \
  <root>/cand-${v}s/records/options.go; done
docker run --rm -v <root>:/late:ro -v <module cache>:/go/pkg/mod:ro -v <corpus>:/corpus:ro \
  -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' -e GOPROXY=off \
  --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand-10s cand-60s cand-30s cand-5s; do (cd /late/$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand-10s cand-60s cand-30s cand-5s; do
    /tmp/records-$rev -stage late -batch 1024 -dir /data/$rev-a
    /tmp/records-$rev -stage late -batch 16384 -dir /data/$rev-b
    /tmp/records-$rev -stage late -records 200000 -batch 1 -dir /data/$rev-c
    rm -rf /data/$rev-a /data/$rev-b /data/$rev-c
  done
  for rev in base cand-10s; do
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
    /tmp/records-$rev -stage replay -seal-age 1h -corpus /corpus -dir /data/$rev-replay
    rm -rf /data/$rev-density /data/$rev-docker /data/$rev-replay
  done'
```

## Blocks a one-second read fetches

| Late when behind | Measured against | Flushes of 1024 | Batches of 16,384 | One at a time, 200,000 |
|---|---|---:|---:|---:|
| a minute, `c252738` | the newest of its batch | 5.24 | 4.63 | 4.68 |
| a minute | the newest before it | 5.24 | 5.24 | 4.68 |
| 30 s | the newest before it | 3.68 | 3.68 | 3.49 |
| 10 s, `ea25a25` | the newest before it | 2.93 | 2.93 | 2.75 |
| 5 s | the newest before it | 2.84 | 2.84 | 2.77 |

Records on time, the density stage, fetch 1.92 in both revisions. Measured
against what came before it, a record is late alike whatever the batch it
arrives in; the ten seconds take the reads from 5.24 blocks to 2.93, and
halving them again would save a tenth of a block. The late fixture's file
goes from 7.9299 bytes a record to 7.9196 at ten seconds.

The production corpus has no late records, its receive times growing within a
container, and its files are the same to the byte: 16.5604 in full segments,
16.7794 sealed hourly, with the same blocks and bytes for the operator's
queries and a follower.

## What this settles, and what it leaves

- A record is late ten seconds behind the newest its stream showed before it;
  the hand-off's 5.2 blocks hold at every batch size, at 2.93.
- A producer whose records arrive more than ten seconds out of order, two
  hosts' clocks apart or a queue that reorders, sends them to the late head,
  sealed on their own; measured on no corpus that does.
