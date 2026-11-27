# events

`tinystore events` writes and reads a store's events from a shell, with no
SDK. `append` writes one event given in words, or another program's output
on stdin, a line per event. It writes through a server, as
[every write](README.md#who-writes) of the command line does. `list` reads
one page of a stream, and `export` writes every event in a range as JSON
lines. Both read beside the program or server that holds the directory.

```sh
tinystore events ./data append audit role.changed user:=42 role=admin
my-worker 2>&1 | tinystore events ./data append worker
tinystore events ./data list audit --since 1h --where user:=42
tinystore events ./data export audit --since 7d > audit.jsonl
```

## Append one event

```sh
tinystore events ./data append audit role.changed user:=42 role=admin --id delivery-81
```

```text
appended role.changed to audit
```

- The words are the stream, the event's name, and its fields.
- `role=admin` is text, and `user:=42` is a JSON value: a number, `true`,
  `null`, an array or an object. Nothing is guessed from a value's look, so
  `zip=02134` stays the text `"02134"`.
- `--id delivery-81` names the event in its stream. A stream that already
  holds the id writes nothing, so a command sent again is harmless:

```text
audit holds the id delivery-81: nothing written
```

- `--at 14:30` gives the event's time, as `--from` takes one. Without it, the
  event happens when it is appended.
- `--level warn` gives it a level: `trace`, `debug`, `info`, `warn` or
  `error`.

## Append another program's output

```sh
my-worker 2>&1 | tinystore events ./data append worker
```

```text
appended 1834 lines to worker
```

Without an event's name, `append` reads stdin until it ends and writes it as
the SDKs' `lines()` does. [Other programs' output](../records/other-programs.md)
says what each line becomes, and why a line that doesn't fit the writer's
buffer is dropped and counted. The last line says how many:
`appended 1834 lines to worker, 2 of them dropped`.

`--id`, `--at` and `--level` name one event, so they are refused with stdin.
On a terminal, without a pipe, `append` refuses instead of waiting for you to
type.

## List a stream

```sh
tinystore events ./data list audit --since 1h --where user:=42
```

```text
14:32:05.120 EVENT audit  role.changed  role=admin user=42
14:40:11.004 WARN  audit  role.changed  role=viewer user=7
```

- Each event prints as `tinystore logs` prints a line. `--json` gives one
  object per event, as `logs --json` does.
- `--where user:=42` keeps the events whose field holds that JSON value, and
  `--where role=admin` that text. The text `user=42` doesn't match the number
  `42`. Give `--where` again for more fields.
- `--level warn` keeps `warn` and above.
- `--since`, `--from` and `--to` take the same values as `metrics read`.
- `--limit` and `--after` read a long stream a page at a time, as
  [Pages](README.md#pages) says.

A stream the store doesn't hold is named as missing, and a read beside the
holder makes nothing. A page holds 10,000 events at most. If `--limit` is
larger, the command fails and names the bound. Use `export` to read them all.

## Export a stream

```sh
tinystore events ./data export audit --from 2026-11-01T00:00Z --to 2026-11-02T00:00Z > day.jsonl
```

```text
tinystore events export: wrote 48211 events of audit
```

- Each line of stdout is one event, oldest first. It is the same object that
  `list --json` prints: `{"time":"2026-11-01T00:00:02.120Z","stream":"audit","event":"login","user":43}`.
- `export` reads one page at a time until the range ends, and writes each
  page before it reads the next. So it keeps only one page in memory, however
  long the stream is. You don't need `--limit` or `--after`.
- Without `--since`, `--from` or `--to`, it writes everything the stream
  keeps. With `--to` alone, it writes everything before that time. `--where`
  and `--level` filter the same way as in `list`.
- `-o day.jsonl` writes to that file instead of stdout. The command first
  writes `day.jsonl.part` next to it and renames it when it is complete. So a
  reader never sees half a file, and a failed export leaves the old file
  unchanged.
- The count of events it wrote goes to stderr, separate from the events.

## Limits and defaults

| What                              | Value                                        |
|-----------------------------------|----------------------------------------------|
| a stream's name                   | `[a-z0-9][a-z0-9_-]{0,63}`                   |
| an id                             | 1 to 128 bytes                               |
| a line of output                  | 256 KiB; a longer one is dropped and counted |
| a page of `list`                  | 100 events by default, 10,000 at most        |
| `list`'s range, when none given   | the last hour                                |
| `export`'s range, when none given | everything the stream keeps                  |

## See also

- [Events](../records/events.md): the same events from a program.
- [status and logs](status-and-logs.md): every stream's lines, followed live.
