# events

`tinystore events` writes and reads a store's events from a shell, with no
SDK. `append` writes one event given in words, or another program's output
on stdin, a line per event. `list` reads a stream beside the program or server
that holds the directory.

```sh
tinystore events ./data append audit role.changed user:=42 role=admin
my-worker 2>&1 | tinystore events ./data append worker
tinystore events ./data list audit --since 1h --where user:=42
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
the SDKs' `lines()` does:

- Each line becomes an event at the time it arrived.
- A stack trace or a traceback is joined into one event with its lines.
- A JSON line becomes an event named `json`, and a `key=value` line one named
  `logfmt`, their keys its fields. Other text becomes an event named `log`.
- The level is found where common loggers write it.

The server never waits for a slow disk: a line that doesn't fit the writer's
buffer is dropped and counted, and the last line says how many:
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
  Without them, `list` reads the last hour.
- A page holds 100 events unless `--limit` says. When there are more, the
  last line says which `--after` value reads the next page.

A stream the store doesn't hold is named as missing, and a read beside the
holder makes nothing.

## Who writes

`append` writes through the server that holds the directory, or starts a
sidecar that stops 30 seconds after its last client, as
[metrics](metrics.md#who-writes) does. A directory that a program opened
`embedded` is refused, because no other process can write to it.

## Limits and defaults

| What                            | Value                                        |
|---------------------------------|----------------------------------------------|
| a stream's name                 | `[a-z0-9][a-z0-9_-]{0,63}`                   |
| an id                           | 1 to 128 bytes                               |
| a line of output                | 256 KiB; a longer one is dropped and counted |
| a page of `list`                | 100 events                                   |
| `list`'s range, when none given | the last hour                                |

## See also

- [Events](../records/events.md): the same events from a program.
- [Other programs' output](../records/other-programs.md): what a line becomes.
- [status and logs](status-and-logs.md): every stream's lines, followed live.
