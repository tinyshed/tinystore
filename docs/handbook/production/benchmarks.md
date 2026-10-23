# Benchmarks

How TinyStore compares with the services it replaces: PostgreSQL, Redis,
MinIO, VictoriaMetrics and Loki. The numbers come from one machine and one
round of measurements of the `0.1.0` release. This page shows where TinyStore
wins, where it loses, and what each test did.

TinyStore is not faster than each of these services at its own job. It
replaces five processes with one, and an application on it uses a fraction
of the memory. Under load it answers more requests, because a call does not
cross a network. A single client that writes waits longer, because a write
that touches two engines syncs two files.

## At rest

Each stack opens everything a small application uses, writes to each engine
once, and then rests for a minute.

| At rest                   | TinyStore | The five services                                                         |
|---------------------------|-----------|---------------------------------------------------------------------------|
| memory, every engine open | 14.7 MiB  | 402 MiB: Loki 133, PostgreSQL 126, MinIO 108, VictoriaMetrics 26, Redis 9 |
| ready after start         | 0.13 s    | 15.5 s, Loki's; PostgreSQL 0.07 s, MinIO 0.37 s                           |
| CPU per minute of rest    | 0.01 s    | 0.6 s                                                                     |
| processes                 | 1         | 5 servers, PostgreSQL's background workers and backends among them        |

The memory is TinyStore's server, `tinystore serve`. With the core loaded into
the program instead, `open(dir, { embedded: true })`, a Bun program holds 17
MiB more than Bun alone, which holds 38. Valkey alone holds 9 MiB at rest and
Prometheus 101.

## An application

A request reads a user's session and a note, logs a line and counts itself.
One request in ten also updates the note and queues a job to index it, and
one in fifty uploads a 16 KiB attachment. A worker in the same program runs
the jobs. Every write is on the disk before the request returns.

| Requests per second | TinyStore | In process | The five services |
|---------------------|-----------|------------|-------------------|
| 1 client            | 756       | 788        | 2,000             |
| 8 clients           | 3,700     | 4,000      | 8,800             |
| 64 clients          | 26,000    | 28,000     | 15,000            |
| 256 clients         | 47,000    | 39,000     | 21,000            |

| At 64 clients            | TinyStore | In process | The five services |
|--------------------------|-----------|------------|-------------------|
| p99 latency              | 24 ms     | 24 ms      | 23 ms             |
| memory, program included | 205 MiB   | 196 MiB    | 878 MiB           |
| CPU per request          | 127 µs    | 86 µs      | 191 µs            |

With one to eight clients the services answer more requests. A request that
writes updates the note in `app.db` and queues its job in `jobs.db`, so it
waits for two disk syncs, one per file. PostgreSQL writes the note and the
job in one transaction and syncs once. Under load, TinyStore groups the
writes of many requests into one sync per file, and wins. Every side kept
every job and every log line in every test.

## One engine against its service

**KV against Redis and Valkey**, 128-byte values over 100,000 keys:

| Calls per second              | TinyStore | In process | Redis   | Valkey  |
|-------------------------------|-----------|------------|---------|---------|
| a read, 1 in flight           | 11,000    | 141,000    | 20,000  | 20,000  |
| a read, 64 in flight          | 217,000   | 352,000    | 357,000 | 355,000 |
| a synced write, 64 in flight  | 12,000    | 12,000     | 9,700   | 9,900   |
| an `'os'` write, 64 in flight | 29,000    | 29,000     | 189,000 | 192,000 |

A read in process takes 6 µs, and through Redis 43 µs. A synced write waits
for the disk on both sides: TinyStore's `'full'` against Redis's `appendfsync
always`. An `'os'` write returns before the sync: TinyStore's `'os'` against
Redis's `appendfsync everysec`. Both survive a crash of the program, and Redis
writes six times as many. With a million keys, Redis holds 211 MiB in memory
at rest and TinyStore's server 18 MiB, with its 162 MiB file in the operating
system's page cache.

**SQL against PostgreSQL**, one table of 100,000 rows:

| Calls per second     | TinyStore | In process | PostgreSQL |
|----------------------|-----------|------------|------------|
| a row by its key, 1  | 6,900     | 32,000     | 11,000     |
| a row by its key, 64 | 136,000   | 180,000    | 27,000     |
| an insert, 1         | 342       | 363        | 599        |
| an insert, 64        | 12,000    | 14,000     | 18,000     |
| an insert, 256       | 26,000    | 33,000     | 20,000     |

**Files against MinIO**, through Bun's S3 client:

| Calls per second | TinyStore | In process | MinIO  |
|------------------|-----------|------------|--------|
| read 16 KiB, 1   | 5,100     | 17,000     | 2,500  |
| read 16 KiB, 64  | 21,000    | 30,000     | 36,000 |
| write 16 KiB, 64 | 2,800     | 2,800      | 606    |
| read 1 MiB, 16   | 472       | 990        | 2,700  |
| write 1 MiB, 16  | 425       | 495        | 276    |

## Logs and metrics on disk

Logs and metrics are measured on the Rust core, because a JavaScript client
would hide the engine behind its own encoding.

**Metrics against VictoriaMetrics and Prometheus**, TSBS DevOps: 1,010 series
over three days, 26 million samples.

| TSBS, 26 million samples    | TinyStore | VictoriaMetrics | Prometheus              |
|-----------------------------|-----------|-----------------|-------------------------|
| bytes per sample on disk    | 1.00      | 0.71            | 1.58                    |
| samples ingested per second | 1.4 M     | 2.1 M           | 50,000, by its backfill |
| memory after ingest         | 15 MiB    | 258 MiB         | 104 MiB                 |
| `cpu-max-all-8`             | 2.2 ms    | 4.3 ms          | 14 ms                   |
| `double-groupby-1`          | 0.3 ms    | 0.5 ms          | 2.3 ms                  |

**Logs against Loki**, 13 datasets of LogHub, 2.6 GiB of text, each line at
the time its own text spells:

| LogHub, 2.6 GiB                 | TinyStore  | Loki      | zstd -19 of the text |
|---------------------------------|------------|-----------|----------------------|
| on disk, share of the text      | 14.2 %     | 19.3 %    | 5.5 %                |
| lines ingested per second       | 0.8–2.2 M  | 136–276 k |                      |
| memory, HDFS's 11 million lines | 18 MiB     | 1,285 MiB |                      |
| the newest 100 lines            | 0.1–0.3 ms | 2–122 ms  |                      |
| count a rare word, BGL          | 1.3 s      | 15 s      |                      |
| count a rare word, HDFS         | 3.1 s      | 1.4 s     |                      |

Loki's queries cost more as their time range grows, and TinyStore's as the
data grows: LogHub's BGL covers seven months, and HDFS's 11 million lines two
days. zstd compresses the whole file at once, and nothing can be found in it
without reading all of it.

## Where TinyStore loses

- **A few clients that write.** With one to eight clients the services answer
  twice the requests, because a write that touches two engines syncs two
  files. Keep a job in the same database as its rows, `db.queue`, to commit
  both in one file.
- **Writes that do not wait for the disk.** With `'os'`, kv writes stop at
  about 30,000 per second, where Redis reaches 190,000.
- **Large files.** MinIO reads a 1 MiB file 2.7 times as fast.
- **Metrics on disk.** VictoriaMetrics keeps a sample in 0.71 bytes, against
  TinyStore's 1.00.
- **A search over many lines in a short range.** Loki counts a word over
  HDFS's 11 million lines in half the time, because it reads on every core.

## How it was measured

Every test ran in one Linux container on an AMD Ryzen 7 7700 (8 cores, 16
threads) under Docker Desktop on WSL2, one stack at a time. Each figure is
the median of three passes, or two for logs and metrics. The programs are
written in Bun 1.4.2 and reach TinyStore through `@tinyshed/tinystore@0.1.0`
from npm. They reach the services through Bun's own Redis, PostgreSQL and S3
clients, the fastest a Bun program has. Logs and metrics use the Rust crate
`tinystore` `0.1.0`.

- **Memory** is the proportional set size (PSS) of every process of a stack,
  read ten times a second. PSS counts a page that several processes share
  once, so PostgreSQL's backends are not counted twice.
- **A write that waits for the disk** is compared only with a write that
  also waits: TinyStore's `'full'` with Redis's `appendfsync always` and
  PostgreSQL's default. TinyStore's `'os'` is compared with Redis's
  `appendfsync everysec`: both survive a crash of the program, and neither
  survives a power loss for the last moments.
- **Each service runs as it ships**, except where a test needs the same
  durability from both sides or the service would refuse the load. The
  report lists every change.
- **The disk syncs in about 3 ms**, slower than a server's disk. Every synced
  rate is this disk's, and the comparisons between contenders are what
  carries over.

The full method, every pass, the raw data and the commands that reproduce
the round are in the
[round's report](https://github.com/tinyshed/research/blob/main/tinystore/reports/release-2026-10-23.md).

## See also

- [Why TinyStore](../start/why.md): what TinyStore is for, and what it is not.
- [SQLite as TinyStore runs it](../inside/sqlite.md): one writer per file, and
  why a write syncs once for many callers.
- [How logs and metrics pack](../inside/how-logs-and-metrics-pack.md): what
  the bytes on disk are made of.
- [Limits and defaults](limits.md): every bound, durability among them.
