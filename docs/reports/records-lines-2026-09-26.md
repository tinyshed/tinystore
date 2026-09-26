# Another program's lines, joined and given their levels — 2026-09-26

The engine took records from `Append` and from `slog` through its handler.
A program that follows other programs' output, a child process's stdout or a
container's log, had to cut lines into records itself, and a stack trace
became a record a frame. `Lines(stream)` is a writer for that output. Each
line becomes a record named `log` at the time it arrived; the lines of one
record are joined first; the text is kept byte for byte, or a JSON object's
fields when they spell the line again; and the level is taken from where the
program wrote it, so that a read by level prunes blocks by their level masks.

```go
cmd.Stdout = logs.Lines("worker")
```

A line joins the record before it when it is indented, opens a traceback,
names an exception or its cause, or when the record's first line began a JSON
value still open; brackets that begin a line's own time open nothing:

```text
2026-09-26 12:00:01,500 ERROR request failed   a record
Traceback (most recent call last):             joins: a traceback
  File "app.py", line 3, in handle             joins: indented
ValueError: bad input                          joins: names an exception
[2026-09-26 12:00:03] info done                a record
```

A first rule joined every line that did not begin with a time to a record
that did. On this corpus it glued logfmt lines whose time comes after their
logger's name, Redis lines whose pid is long, and lines behind a colour code
to the line before them; and a line beginning with a bracketed time counted
as a JSON array, which its colour codes' brackets kept open for a thousand
lines. Neither is in the engine. `FuzzLinesLoseNoByte` checks that the
records a writer makes, one after another with a newline between them, are
the bytes it was given, however they were cut into writes.

## Environment and reproduction

- `1653b07`, whose runner has the `docker` stage, each docker entry a record
  as the research read them, and the `lines` stage, each container's entries
  written to a writer of `Lines` at the time docker received them, flushed
  every 512 entries, full segments sealed as they fill.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, 65
  containers. Aggregates only.

```sh
git -C <repo> archive 1653b07 | tar -x -C <cand>
docker run --rm -v <cand>:/src:ro -v <module cache>:/go/pkg/mod:ro -v <corpus>:/corpus:ro \
  -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' -e GOPROXY=off \
  --tmpfs /data:rw,size=6g golang:1.27 sh -c 'cd /src/bench/records && go build -o /tmp/records . &&
  /tmp/records -stage docker -corpus /corpus -dir /data/d &&
  /tmp/records -stage lines -corpus /corpus -dir /data/l &&
  cd /src && go test ./records -run "^$" -bench "^BenchmarkLines$" -benchtime=20x -benchmem -cpu=1 -count=6'
```

## What the writer made of the corpus

| | Each entry a record | Through `Lines` |
|---|---:|---:|
| docker entries | 1,327,088 | 1,327,088 |
| records | 1,323,561 | 1,060,245 |
| records of several lines | 3,527 pieces docker split, rejoined | 39,001 |
| records with a level | the JSON services' own field only | 902,559, 85.1 % |
| the file | 21,918,720 bytes | 20,765,696 bytes |
| the file, bytes an entry | 16.5164 | 15.6475 |

The levels found: 828,208 info, 53,025 debug, 20,541 warn, 691 error and 94
fatal; 157,686 records name none. A record of several lines holds 7.8 lines
on average: the corpus's containers print tracebacks, Java frames, and the
details of an event on indented lines after it. The file is 5.3 % smaller:
blocks take 19.3095 bytes a record against 16.3391, but a record now holds
1.25 entries, and the blocks' bytes an entry fall from 16.2957 to 15.4268.

## Reads by level

| Query, over all time | Rows | Blocks | Bytes | Time |
|---|---:|---:|---:|---:|
| `level = 50` inside the JSON services' lines, each entry a record | 31 | 186 | 2,726,342 | 15.3 ms |
| `MinLevel` error, through `Lines`, every container | 785 | 31 | 366,442 | 10.6 ms |

A level written inside a line is an attribute the level masks cannot see,
and a read for it decodes the attribute in every block holding the key.
Mapped into the record, it finds the errors of all 65 containers, not only
the JSON services', in a sixth of the blocks. Single runs, warm cache.

## Speed

`BenchmarkLines`, one CPU, six runs of twenty passes over 4,100 lines: the
text fixture's lines in the layouts stamps are found in, its JSON service's
lines, and a traceback. The median is 1,407,786 lines a second, the range
811,898 to 1,597,412, 497 bytes and 10.4 allocations a line; appending and
sealing the corpus through the writers took 10.558 s, where appending its
entries as records took 12.139.

## What this settles, and what it leaves

- A program following other programs' output gets their records whole, with
  their levels, in fewer bytes than one record an entry.
- 14.9 % of the corpus's records name no level the writer knows: plain text,
  and levels written as colour-coded words.
- A logfmt line stays text: its pairs would not spell it again. OTLP, and a
  server taking lines from programs that do not embed the store, are not
  built.
