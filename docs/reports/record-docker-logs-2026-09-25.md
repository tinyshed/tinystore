# v2 records on production container logs — 2026-09-25

The [v2 round](record-v2-2026-09-25.md) measured synthetic fixtures and one
public control. This round reads 1.32 million real log lines from the
containers of two production hosts and asks three questions: what real
structured logs cost, whether text logs compress better than zstd over the
same lines, and whether the queries an operator asks prune on real data.

The answers: structured application logs cost **17.75 bytes per record** as
`slog` would deliver them, 89 % of it one random request id per record; text
logs from third-party software cost 19.28 against zstd's 19.64; the whole
corpus is **19.95 bytes per record** in a SQLite file, against 209.6 bytes of
text per line. Two codec changes came out of it, described below.

## Corpus

- Two production hosts of the project's owner, 65 containers, fourteen days of
  docker `json-file` logs, 384,170,174 bytes in 77 files. The snapshot's
  fingerprint, `sha256` over the sorted per-file digests that
  `bench/fetch-docker-logs.sh` prints, is
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`.
- The corpus is production data. It is not in the repository and will not be;
  this report gives aggregates only and names no container or host. It cannot
  be fetched by a reader; the script collects the same shape from any docker
  hosts.
- Five containers run two in-house Node.js services writing pino JSON. One
  reverse proxy writes JSON. The other 59 write text: a search engine (glog), a
  BI server (log4j), dashboards (logfmt), proxies, databases, Python bots and
  one bot printing pretty JSON across lines.
- Every docker entry becomes one record after rejoining the pieces docker
  splits lines longer than 16 KiB into (3,527 pieces). Two entries padded with
  NUL by an unclean shutdown are skipped. No line was too large for a block.

A record keeps docker's receive time as event time (nanoseconds), its stream
(`stdout`, `stderr`) as event name and the container as stream. A line that is
a JSON object rebuilding byte for byte from its fields becomes attributes in
their order; any other line is the body, byte for byte. Every segment is
decoded and compared to its input.

The second reading of the pino services is what an embedded store would get
from `slog`: application time in milliseconds, level mapped from pino's 10…60
to slog's −8…12, message as body, the other fields as attributes, no receive
time.

## Environment and reproduction

Commit `9d01766`; Windows 11 Pro 10.0.26200, AMD Ryzen 7 7700, Go 1.27.1
windows/amd64, `modernc.org/sqlite` v1.59.0, `klauspost/compress` v1.19.0,
one CPU of work per codec, the store with 1 KiB pages. zstd baselines use
`SpeedDefault` on chunks of one segment, 16,384 lines, with a 4 MiB window.

```sh
sh bench/fetch-docker-logs.sh <corpus> <ssh target>...
TINYSTORE_SPIKE=1 TINYSTORE_RECORD_DOCKER=<corpus> \
  go test ./spike -run '^TestRecordV2DockerLogs$' -v -count=1 -timeout 60m
TINYSTORE_SPIKE=1 TINYSTORE_RECORD_DOCKER=<corpus> \
  go test ./spike -run '^TestRecordV2DockerTextScope$' -v -count=1 -timeout 60m
```

## Bytes per record

`content` is what the program wrote, one line per record. `zstd, lines` compresses
those lines; `zstd, stamped` compresses them prefixed with the receive time and
stream, the information v2 keeps.

| Group | Containers | Records | Content | zstd, lines | zstd, stamped | v2 payload |
|---|---:|---:|---:|---:|---:|---:|
| pino JSON services | 5 | 125,873 | 259.9 | 27.78 | 38.56 | **21.53** |
| the same, as `slog` records | 5 | 125,873 | — | — | — | **17.75** |
| reverse proxy JSON | 1 | 2,075 | 618.3 | 36.52 | 45.08 | 42.00 |
| text | 59 | 1,195,613 | 203.6 | 11.02 | 19.64 | 19.28 |
| all | 65 | 1,323,561 | 209.6 | 12.66 | 21.48 | 19.53 |

The complete store of all 1,323,561 records is **19.9545 bytes per record**:
blocks 25,296,896 allocated / 24,860,950 payload / 307,048 unused, segments
1,065,984 / 1,043,307 / 16,583, the time index 46,080.

### Where the bytes are

| pino services as `slog` | B/record | Text | B/record |
|---|---:|---|---:|
| `requestId` | 15.843 | body | 14.367 |
| time | 1.365 | time | 4.035 |
| `path` | 0.208 | segment rows, mostly text samples | 0.863 |
| body | 0.054 | block headers | 0.014 |
| everything else | 0.27 | | |

All 124,560 `requestId` values are distinct UUID version 4: 122 random bits per
record, a floor of 15.25 bytes that no codec reaches under. These services log
one line per request, so their density is decided by the ids they write, not
by the store; a time-ordered UUID version 7 would leave 74 random bits.

Text divides by compression scope on the same 1,195,595 bodies:

| Text bodies compressed | B/record |
|---|---:|
| zstd, one frame per segment | 11.30 |
| zstd, one frame per block of 1024 | 13.89 |
| zstd, per block, against a 64 KiB sample of the segment | 13.07 |
| v2 text column, per block | 14.15 |
| v2 text column against the sample, the sample included | 13.78 |

Independent blocks cost text about 23 % against one frame per segment. Many of
these lines carry their own timestamp as text beside the receive time.

## Two codec changes

- **Text without newlines needs no length column.** Values of different lengths
  and no newline are stored newline-separated; values of one length keep a
  length column of one width and no bits. Text lost 0.5 bytes per record to
  lengths before.
- **A text-heavy segment keeps a sample of its bodies.** When a segment's bodies
  hold at least 256 KiB, 64 KiB taken at an even stride go into the segment row
  and every block's text blobs compress against it. A block still needs only
  its own row and the segment row; one decoded without its segment's sample is
  refused. Net gain on this corpus: 0.37 bytes per text record.

On the synthetic fixtures neither change moves the file: one million frontend
records stay at 7.8520 bytes, backend goes from 20.7217 to 20.7268.

## Queries

All records in one store. Each result was compared to a scan of the input.
Single runs, warm cache.

| Query | Rows | Segments | Blocks read | Blocks decoded | Bytes | Time |
|---|---:|---:|---:|---:|---:|---:|
| the busiest minute, every container | 4,770 | 27 | 35 | 20 | 502,750 | 9.4 ms |
| one `requestId` value, all time | 1 | 152 | 160 | 1 | 3,725,982 | 43 ms |
| `level = 50` inside JSON, all time | 31 | 152 | 186 | 2 | 3,906,056 | 35 ms |

- Time pruning holds on real data: one minute of 65 containers read 35 blocks.
- An attribute lookup reads the column in every block that has the key: 160
  blocks for one id. Id-like columns want the per-block bloom trace ids have.
- A level written inside JSON is an attribute, invisible to the level mask:
  186 blocks read to find 31 errors in 2. An adapter that maps it into the
  level column prunes the rest.

## What remains open for records

- Text templates with typed variables, including a line's own timestamp, are
  the remaining lever for text; per-segment zstd shows about 2.5 bytes a
  record between independent blocks and one frame.
- A continuation rule: one bot prints JSON over many lines and a per-line
  model splits it.
- Adapters for pino, logfmt, glog and log4j lines, mapping time, level and
  message instead of keeping them inside the body or attributes.
- Per-block blooms for id-like attribute columns.
