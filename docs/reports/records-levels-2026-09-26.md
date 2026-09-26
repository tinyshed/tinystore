# Levels found in colour, and logfmt lines kept as their pairs — 2026-09-26

[The lines round](records-lines-2026-09-26.md) left 14.9 % of the corpus's
records without a level, and a logfmt line as text. A census of those records
by the shape of their first bytes, digits masked, found where the levels were:
three in four were words between colour codes, which the level search read
as `mINFO`, the letter that ends a colour glued to the word; the rest were
zerolog's three letters, Redis's mark after its time, and logfmt lines whose
`level=` came after the first 96 bytes. Now:

- a word between colour codes, spaces aside, counts as one in brackets, in
  any case, Postgres's `LOG` without its colon included; a colour is skipped
  as a whole rather than read as letters;
- zerolog's `INF`, `WRN`, `ERR`, `DBG`, `FTL` and `TRC` are level names;
- Redis's mark after its time is its level: `.` and `-` debug, `*` notice,
  `#` warning;
- a `level=` or `lvl=` pair counts anywhere on a line's first line;
- a logfmt line keeps its pairs as attributes when one rule spells them back
  as the line, byte for byte, and stays text otherwise: pairs one space
  apart, a value bare when it holds no space, quote or control byte and
  quoted as the JSON string it is otherwise.

```text
level=info msg="slow request" ms=1200   → logfmt: level "info", msg "slow request", ms 1200
level=info msg="ok"                     → log: the rule spells ok bare, so the line stays text
\x1b[32mINFO\x1b[0m started              → log, level info: a word in colour counts as one in brackets
```

A record says how it spells its line again by its name: `log` for text kept
as its body, `json` for a JSON object's fields, `logfmt` for logfmt pairs.
`FuzzLinesLoseNoByte` spells every record back by its name and checks the
bytes, and ran two million inputs; a record whose fields weigh more than a
record may is kept as text, and the join of a record's lines stops at the
text its stream leaves room for, where it stopped a thousand bytes short.

## Environment and reproduction

- The baseline at `865d580`; the candidate at `04e5f2c`, whose engine is
  `f9f2dcc` and whose runner counts the records of `lines` by name.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, 65
  containers, each container's entries written to a writer of `Lines` at the
  time docker received them, as the lines round did. Aggregates only.

```sh
git -C <repo> archive 865d580 | tar -x -C <base>
git -C <repo> archive 04e5f2c | tar -x -C <cand>
docker run --rm -v <base>:/src-base:ro -v <cand>:/src-cand:ro -v <module cache>:/go/pkg/mod:ro \
  -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' \
  -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in base cand; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in base cand; do /tmp/records-$rev -stage lines -corpus /corpus -dir /data/$rev; rm -rf /data/$rev; done
  for rev in base cand; do (cd /src-$rev && go test -c -o /tmp/records-$rev.test ./records); done
  for round in 1 2 3 4 5 6; do for rev in base cand; do
    /tmp/records-$rev.test -test.run "^$" -test.bench "^BenchmarkLines$" -test.benchtime=20x -test.benchmem -test.cpu=1
  done; done'
```

## The corpus through writers of `Lines`

| | `865d580` | `04e5f2c` |
|---|---:|---:|
| records | 1,060,245 | 1,060,245 |
| named `log`, `json`, `logfmt` | all `log` | 902,231, 127,493, 30,521 |
| with a level | 902,559, 85.1 % | 1,038,935, 98.0 % |
| debug, info, warn | 53,025, 828,208, 20,541 | 89,709, 926,489, 21,225 |
| error, fatal | 691, 94 | 1,419, 93 |
| the file | 20,766,720 bytes | 20,787,200 bytes |
| bytes an entry | 15.6484 | 15.6638 |

The 21,310 records still without a level are request lines of a web
framework's logger, `--> GET /metrics 200 12ms` and `<-- GET /metrics`,
19,196 of them, which write none, and a few plain lines.

The logfmt lines cost the file 20 KB, 0.1 %: as typed columns the blocks
take 97,280 bytes less, and the blooms over their id-like values and the keys
their segments list take 117,760 more. What they buy is a query by pair:
`Attrs` finds `ms=1200` as it finds a JSON line's field.

| Query, over all time | Rows | Blocks | Bytes | Time |
|---|---:|---:|---:|---:|
| `MinLevel` error, `865d580` | 785 | 31 | 366,442 | 11.0 ms |
| the same, `04e5f2c` | 1,512 | 97 | 1,786,247 | 32.0 ms |

The candidate finds twice the errors, the ones written in colour among them,
in three times the blocks; single runs, warm cache.

## Speed

`BenchmarkLines`, one CPU, the two revisions interleaved, six runs of twenty
passes over 4,100 lines each: the median goes from 97.9 MB/s to 92.0, about
1.73 million lines a second to 1.63, 6 % fewer, the price of trying each line
as logfmt and of looking for a level past colour codes; 493 bytes and 10.0
allocations a line against 497 and 10.4.

## What this settles, and what it leaves

- Programs that write their level in colour, zerolog, Redis and logfmt lines
  give it to `MinLevel` now: 2.0 % of the corpus's records name none, all of
  them lines that write none.
- A logfmt line's pairs are attributes a query finds, at no cost in bytes.
- Lines that name no level are not given one: the store does not guess.
