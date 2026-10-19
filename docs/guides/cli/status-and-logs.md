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
a script.

## logs

```sh
tinystore logs ./data -f
tinystore logs ./data --since 1h --level warn
tinystore logs ./data --stream api --grep "connection reset"
```

```log
19:42:24.252 ERROR worker  json  level=error msg=boom code=7
19:42:24.252 WARN  worker  level=warn msg="disk low" free=3
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

`logs` prints a line as the logger's console does, in the same bytes every
language prints, so `-f` in a terminal reads like the program's own output.
`-f` is a [tail](../records/tail-and-poll.md): a reader slower than the
writers is told how many lines it skipped.

## In a script

```sh
tinystore status ./data --json | jq '.files[] | select(.bytes > 1e9)'
tinystore logs ./data --since 1h --level error --json | jq -r '.msg' | sort | uniq -c
```

A terminal gets colour and words, a pipe gets plain lines, and `--json` gives
what a script parses. Colour appears only on a terminal, never under
`NO_COLOR`.

## See also

- [The command line](README.md).
- [Reading](../records/reading.md) events from a program.
- [Running it](../../handbook/production/running-it.md) in the Handbook.
