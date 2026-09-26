# Text templates measured against zstd, and a segment's text at the stronger level — 2026-09-26

[The stamps round](records-stamps-2026-09-26.md) named text's next lever:
the rest of a template, numbers typed where they stand, which a line's own
time was the first of. This round measures it on the production corpus
before building it, with the engine's own column encoders, against what the
engine writes and against zstd at other levels and scopes. Templates do not
pay; the stronger zstd level does, and the engine now writes a segment's text
with it.

## What a text record's body costs

`TestTextTemplatesAgainstZstd` in `records/corpus_test.go` writes each
container's entries through a writer of `Lines` at the times docker received
them, keeps the records kept as text, and measures their bodies in segments
of 16,384 records and blocks of 1024:

- **the body column** as the engine writes it, its stamps and the rest;
- **the rest past the stamps**, a block at a time through zstd at the
  default level, at the better-compression level and at the best, and as one
  zstd frame a segment;
- **templated**: each rest cut into a template, the numbers standing in it,
  canonical digit runs a letter or digit does not precede, and its hex words
  of eight digits or more; a dictionary a segment of the templates seen twice
  or more, under zstd; and a block of each value's template id, its numbers,
  its hex words and the values of no template as text, the numbers in one
  column a block or in a column for each place in each template.

The corpus's 902,231 text records, bytes a record:

| | B/record |
|---|---:|
| the body column as `e635f5e` writes it, stamps and rest | 10.19 |
| the stamps alone: counts, layouts, gaps, distances | 0.81 |
| the rest, a block at a time, default level | 10.13 |
| the same, better-compression level | 9.37 |
| the same, best level | 9.08 |
| the rest as one frame a segment, better-compression level | 8.89 |
| templated, the numbers in one column a block, dictionary included | 11.38 |
| templated, a column for each place in each template, dictionary included | 10.61 |
| of which the template dictionary | 0.26 |

Templates cost more than the text they come from: zstd already finds a
block's repeated template, a few bytes a record, and a number is cheaper as
digits among their neighbours than as a value of a column, even one for each
place in each template, whose widths and bases are paid a column at a time.
One frame a segment would take 0.48 bytes a record more than blocks, and a
read would decode a whole segment for one block; the engine keeps its blocks
apart. zstd took 317 ms over the rests at the default level, 457 ms at the
better-compression level and 1,533 ms at the best, one CPU.

## The engine at the better-compression level

A segment's text is written once and kept for weeks, so the engine now
compresses it at zstd's better-compression level; a head row lives an hour at
most and stays at the default, so that an append pays nothing more. Decoding
does not depend on the level.

| Bytes in the file | `04e5f2c` | `e635f5e` |
|---|---:|---:|
| production corpus, full segments, a record | 16.5612 | 16.2355 |
| its text records' payload, a record | 15.4765 | 15.1440 |
| its JSON records' payload, a record | 20.7027 | 20.6504 |
| the corpus through writers of `Lines`, an entry | 15.6638 | 15.3144 |
| one million frontend records, a record | 7.8940 | 7.8940 |

Appending and sealing the corpus took 12.42 s and 12.11 s, through `Lines`
10.22 s and 10.14 s, single runs.

`BenchmarkSegment`, one CPU, the two revisions interleaved, six runs of
twenty passes each, records a second:

| | `04e5f2c` | `e635f5e` |
|---|---:|---:|
| frontend encode | 547,937 | 540,337 |
| frontend decode | 1,345,561 | 1,328,871 |
| backend encode | 755,019 | 773,623 |
| backend decode | 1,303,084 | 1,339,346 |
| text encode | 871,769 | 896,106 |
| text decode | 1,825,288 | 1,908,015 |

Each pair lies within the other's range: the fixtures' text blocks are small
or random enough that the level changes neither their bytes nor their time,
where the corpus's text pays 44 % more zstd time for 7.5 % fewer bytes.

## Environment and reproduction

- The baseline at `04e5f2c`; the candidate at `e635f5e`, which changes the
  level alone; the measurement at `d27ad2c`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Windows 11 Pro
  10.0.26200, Docker Desktop 29.6.2; every figure ran in the `golang:1.27`
  container (go1.27.1 linux/amd64) with the module cache mounted read-only and
  the store on a tmpfs. `modernc.org/sqlite` v1.59.0, `klauspost/compress`
  v1.19.0, 1 KiB pages.
- The research's fixtures, and the private production snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, read as
  the engine round and the lines round read it. Aggregates only.

```sh
git -C <repo> archive 04e5f2c | tar -x -C <base>
git -C <repo> archive e635f5e | tar -x -C <cand>
git -C <repo> archive d27ad2c | tar -x -C <meas>
docker run --rm -v <base>:/src-b:ro -v <cand>:/src-c:ro -v <meas>:/src-d:ro -v <module cache>:/go/pkg/mod:ro \
  -v <corpus>:/corpus:ro -e GOWORK=off -e GOTOOLCHAIN=local -e GOFLAGS='-mod=readonly -buildvcs=false' \
  -e GOPROXY=off --tmpfs /data:rw,size=6g golang:1.27 sh -c '
  for rev in b c; do (cd /src-$rev/bench/records && go build -o /tmp/records-$rev .); done
  for rev in b c; do
    /tmp/records-$rev -stage lines -corpus /corpus -dir /data/$rev-lines
    /tmp/records-$rev -stage density -dir /data/$rev-density
    /tmp/records-$rev -stage docker -corpus /corpus -dir /data/$rev-docker
    rm -rf /data/$rev-*
  done
  for rev in b c; do (cd /src-$rev && go test -c -o /tmp/records-$rev.test ./records); done
  for round in 1 2 3 4 5 6; do for rev in b c; do
    /tmp/records-$rev.test -test.run "^$" -test.bench "^BenchmarkSegment$" -test.benchtime=20x -test.benchmem -test.cpu=1
  done; done
  cd /src-d && TINYSTORE_SPIKE=1 TINYSTORE_CORPUS=/corpus go test ./records -run "^TestTextTemplatesAgainstZstd$" -v -count=1'
```

## What this settles, and what it leaves

- Text templates with typed numbers are not built: on the only real text
  there is to measure, they cost 1.2 to 2.0 bytes a record more than zstd.
  The time a line spells, which stamps keep, was the template worth taking.
- A segment's text at the better-compression level: the production corpus
  at 16.24 bytes a record in full segments, 15.31 an entry through `Lines`,
  the frontend fixture and the speed gates where they were.
- The research's 7 to 9 bytes a record hold where the data allows them, the
  frontend fixture at 7.89. On the production corpus a text record's body
  keeps 9.4 bytes of words zstd cannot shrink further a block at a time, and,
  as the research measured, a JSON service's record 16 bytes of random
  request id; the best level would take 0.3 bytes more at five times the zstd
  time, and one frame a segment 0.5 at the cost of independent blocks.
