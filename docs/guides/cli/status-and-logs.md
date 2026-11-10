# status and logs

`status` says what a store holds and who serves it. `logs` prints the
application's logs, and follows them as they arrive with `-f`. Both read
beside the program or server that holds the directory, without stopping it
and without writing a byte (contract CLI-01).

## status

```sh
tinystore status ./data
tinystore status ./data --json
```

```text
○ data  not served

  sql/shop  88 KiB
  ────────────────
  total     88 KiB
```

A served store adds who serves it, its clients and its memory; a node adds its
files, its links and how far behind its copies are. A directory no server
holds says so and exits 0. `--json` gives one document, for a health check or
a script, and its `names` lists everything the store holds by name: buckets
with what each is for, queues, sets of files, streams, metrics, databases and
search indexes.

```sh
tinystore status ./data --json | jq '.names'
```

```json
{"buckets": [{"name": "sessions", "role": "values"}], "queues": ["emails"], "files": ["avatars"],
 "streams": ["audit"], "metrics": ["ping_rtt_ms"], "databases": ["app"], "indexes": []}
```

## What each name holds

```sh
tinystore status ./data --names
```

```text
○ ./data  not served

  kv       2.0 MiB
    sessions        500 keys       1.9 MiB
    pins             50 keys         790 B
  search   808 KiB
    courses        3000 documents  273 KiB
  records   38 KiB
    audit          2000 events     1.7 KiB   keep 400d   2026-10-31 … 2026-10-31
    client-errors     1 event         62 B   keep 90d    2026-10-31 … 2026-10-31
  ────────────────
  total    2.8 MiB
```

`--names` lists every bucket, queue, set of files, stream, metric and search
index under its engine's file, the heaviest first. Each name shows its count
and its bytes. A stream and a metric also show how long they keep data, and
the dates of their oldest and newest entries. Use it to find which bucket
grows with your users, or which stream keeps too much.

| Engine  | Counts                                       | Bytes                                                      |
|---------|----------------------------------------------|------------------------------------------------------------|
| kv      | live keys, without expired or cleared ones   | the keys and their values                                  |
| jobs    | jobs that wait, run, failed or are kept done | their ids, values, errors and step answers                 |
| blobs   | files                                        | the files' sizes, so a copy that shares bytes counts twice |
| records | events                                       | the compressed rows and blocks, and the events' ids        |
| search  | documents                                    | the documents, their words and their postings              |
| metrics | series                                       | none: a metric's blocks are not stored per name            |

The bytes leave out SQLite's pages, its indexes and its free space, so the
names under a file add up to less than the file.

> [!NOTE]
> **`--names` reads every name whole**
> It scans each bucket, queue, set of files, stream and index, one name per
> read, so on a big store it takes longer than `status`. Plain `status`, Tab
> completion and the MCP server's status tool read only the names.

With `--json`, each name becomes an object with its `name`, `count` and
`bytes`. A bucket adds its `role`. A stream and a metric add `keep`, `oldest`
and `newest`. A metric's `bytes` and a database's `count` are `null`, and a
database's `bytes` is its file's size.

```sh
tinystore status ./data --names --json | jq '.names.buckets'
```

```json
[{"name": "sessions", "role": "values", "count": 500, "bytes": 2002890},
 {"name": "pins", "role": "values", "count": 50, "bytes": 790}]
```

## logs

```sh
tinystore logs ./data -f
tinystore logs ./data --since 1h --level warn
tinystore logs ./data --stream api --grep "connection reset"
```

```log
19:42:24.252 ERROR worker  json  level=error msg=boom code=7
19:42:24.252 WARN  worker  logfmt  level=warn msg="disk low" free=3
```

| Flag              | Does                                                           |
|-------------------|----------------------------------------------------------------|
| `-f`              | follow new lines as they come, until Ctrl+C                    |
| `-n <count>`      | how many of the last lines to print first; 20 unless `--since` |
| `--level <level>` | `trace`, `debug`, `info`, `warn` or `error`, and above         |
| `--since <span>`  | lines from the last span: `30s`, `15m`, `1h`, `2d`             |
| `--grep <text>`   | lines that hold the text, in any case                          |
| `--stream <name>` | one stream, as `api`                                           |
| `--json`          | one JSON object a line                                         |

`logs` prints a line as the logger's [console](../records/console.md) does,
so `-f` in a terminal reads like the program's own output.
`-f` reads the store again every half second, from ten seconds before the
newest line it printed, and prints the lines it hasn't printed yet. So a line
that another process commits a little late, with an earlier time, still
shows.

Like the console, `logs` never passes a [control
character](../records/console.md#control-characters-in-what-you-log) from a
stored line to the terminal. A message's lines after the first, and a value
of several lines, go under the event's line, indented four spaces, as a stack
trace's lines do, so no stored line looks like a log line of its own. Any
other control character except a tab prints as its escape, such as `\u001b`
or `\r`. The exception is a colour that another program wrote into its
output, such as `\x1b[31m`: it shows when `logs` prints colour itself, reset
at the end of its line so it doesn't colour what follows, and is dropped in a
pipe or with colour off, so plain output holds no escape codes.

## In a script

```sh
tinystore status ./data --json | jq '.files[] | select(.bytes > 1e9)'
tinystore logs ./data --since 1h --level error --json | jq -r '.msg' | sort | uniq -c
```

A pipe gets plain lines, and `--json` gives what a script parses, as
[How it behaves](README.md#how-it-behaves) says.

## See also

- [The command line](README.md).
- [Reading](../records/reading.md) events from a program.
- [Running it](../../handbook/production/running-it.md) in the Handbook.
