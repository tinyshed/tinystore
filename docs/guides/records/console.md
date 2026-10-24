# Console output

A logger prints each line to standard error, pretty on a terminal and as JSON
anywhere else, so a developer reads it and a log collector parses it. Every
language prints the same bytes for the same line.

```log
16:42:24.094 INFO  api  signed in  user=42
16:42:24.094 WARN  api  slow request  requestId=r-1 route=/notes ms=1200
16:42:24.094 ERROR api  charge failed  order=7
    error: Error: card declined
        at charge (billing.ts:41:9)
```

```json
{"time":"2026-10-19T16:42:24.094Z","level":"INFO","stream":"api","msg":"signed in","user":42}
```

## Choosing the format

```ts
const log = store.logger('api', { console: 'json', time: 'full', hideStream: true })
```

```python
log = store.logger("api", console="json", time="full", hide_stream=True)
```

```go
handler := records.NewHandler(store, "api", console.JSON, console.TimeFull, console.HideStream)
```

```rust
let log = store.logger("api").console(Format::Json).time(Time::Full).hide_stream().layer()?;
```

| Option       | Values                                                    | Unless given                        |
|--------------|-----------------------------------------------------------|-------------------------------------|
| `console`    | `pretty`, `json`, `off`                                   | `pretty` on a terminal, else `json` |
| `time`       | `clock`, `full`, `off`: a pretty line's time              | `clock`                             |
| `hideStream` | leaves the stream out of a pretty line                    | shown                               |
| `to`         | where lines go                                            | standard error                      |
| `level`      | the least level kept and printed, or `{ store, console }` | every level                         |

`console: 'off'` prints nothing and keeps every line: a server that ships its
console elsewhere, or a test.

## A quiet console

A store's logger can print less than it keeps. Give `level` a level for
each place: `store` is the least level kept, and `console` the least level
printed. In this example the console shows warnings and errors, and the store
keeps every line from `info`, so a query still finds what the terminal didn't
show:

```ts
const log = store.logger('api', { level: { store: 'info', console: 'warn' } })
```

```python
log = store.logger("api", level={"store": "info", "console": "warning"})
```

```go
handler := records.NewHandler(store, "api", records.Levels{Store: slog.LevelInfo, Console: slog.LevelWarn})
```

```rust
let levels = Levels { store: Some(Level::INFO), console: Some(Level::WARN) };
let log = store.logger("api").level(levels).layer()?;
```

A part that you leave out works as if `level` weren't set for it. Without
`store`, the store keeps every level:

```ts
const log = store.logger('api', { level: { console: 'warn' } }) // keeps every line, prints warnings and errors
```

```python
log = store.logger("api", level={"console": "warning"})  # keeps every line, prints warnings and errors
```

```go
handler := records.NewHandler(store, "api", records.Levels{Console: slog.LevelWarn}) // keeps every line, prints warnings and errors
```

```rust
let levels = Levels { console: Some(Level::WARN), ..Levels::default() }; // keeps every line, prints warnings and errors
let log = store.logger("api").level(levels).layer()?;
```

- One level, such as `level: 'info'`, sets both places. It is the same as
  `{ store: 'info' }`.
- Go spells the two parts `records.Levels`, whose `Store` and `Console` are
  `slog.Leveler` values, so a part you leave out is `nil`. Rust spells them
  `records::Levels`, whose fields are `Option<Level>`. In Go,
  `console.Level(slog.LevelInfo)` is `records.Levels{Store: slog.LevelInfo}`.
- The `console` part changes only what is printed. A line at the `store`
  level or above is kept with the same fields and context, whatever the
  `console` part is.
- The two parts are independent. With `{ store: 'info', console: 'debug' }`,
  the console prints debug lines that the store doesn't keep.
- An event from `log.event` has no level, so it is printed and kept.
- Without a `console` part, the console prints what the store keeps. When the
  config `log` changes the level, the console follows it. A `console` part
  set in the code, or `LOG_CONSOLE_LEVEL`, stays as it is.
- A logger of the console alone takes one level, because it only prints.
  `LOG_CONSOLE_LEVEL` still sets what it prints, as the next section shows.

## The environment wins

So that a deployment changes how lines look without a build, each variable
goes over its own option in the code:

| Variable            | Values                                    | Wins over                     |
|---------------------|-------------------------------------------|-------------------------------|
| `LOG_LEVEL`         | `trace`, `debug`, `info`, `warn`, `error` | `level`, or its `store` part  |
| `LOG_CONSOLE_LEVEL` | `trace`, `debug`, `info`, `warn`, `error` | the `console` part of `level` |
| `LOG_FORMAT`        | `pretty`, `json`, `off`                   | `console`                     |
| `LOG_TIME`          | `clock`, `full`, `off`                    | `time`                        |

- `LOG_LEVEL=debug` changes what a store keeps. The console prints from it too,
  unless `level` has a `console` part. A `console` part that the code gave
  stays as it is.
- `LOG_CONSOLE_LEVEL` changes only what is printed, and never what a store
  keeps. It sets what every console prints: a store's logger, a logger of the
  console alone, and the stderr of `tinystore serve`.
- A logger of the console alone only prints, so `LOG_CONSOLE_LEVEL` wins over
  its `level` and over `LOG_LEVEL`. For example, `LOG_CONSOLE_LEVEL=warn` with
  `LOG_LEVEL=debug` prints warnings and errors.
- `env: { prefix: 'APP' }` reads `APP_LOG_LEVEL` and the rest instead;
  `env: false` reads none.
- The environment never turns on a console the code turned off.
- A value none of them can mean is ignored, and said once.

Colours need a terminal, and neither `NO_COLOR` nor `TERM=dumb`.
`FORCE_COLOR` makes a pipe pretty and coloured; `NO_COLOR` still wins.

## A console without a store

A program that wants the logger and no store, a CLI, a script, takes it from
the package. The same calls, the same options but `buffer` and `keep`, the
same lines, kept nowhere. Its `level` is one level, because it only prints:

```ts
import { logger, secrets } from '@tinyshed/tinystore'

const log = logger('app', { redact: secrets })
```

```python
from tinystore import logger, secrets

log = logger("app", redact=secrets)
```

```go
log := slog.New(console.NewHandler("app", console.Redact(console.Secrets...)))
```

```rust
let log = tinystore::console::logger("app").redact(SECRETS).layer();
tracing_subscriber::registry().with(log).init();
```

In TypeScript and Python it loads nothing native. Go's package `console`
imports neither the store nor cgo. In Rust the crate's `console` feature
builds it alone, without a store or SQLite. Swapping `logger('app')` for
`store.logger('app')` later keeps the lines too, with no other change.

## Same bytes everywhere

A line printed is the same bytes in TypeScript, Python, Go and Rust: one file
of the repository holds the lines every SDK's tests print byte for byte. A
program in two languages behind one log collector parses one format.

## See also

- [Records](README.md): the engine's overview.
- [Logging](logging.md): levels, fields, `with` and secrets.
