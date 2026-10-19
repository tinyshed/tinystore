# Console output

A logger prints each line to standard error, pretty on a terminal and as JSON
anywhere else, so a developer reads it and a log collector parses it. Every
language prints the same bytes for the same line.

```text
16:42:24.094 INFO  api  signed in  user=42
16:42:24.094 WARN  api  slow request  requestId=r-1 route=/notes ms=1200
16:42:24.094 ERROR api  charge failed  order=7
    error: Error: card declined
        at charge (billing.ts:41:9)
```

```text
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

| Option       | Values                                       | Unless given                        |
|--------------|----------------------------------------------|-------------------------------------|
| `console`    | `pretty`, `json`, `off`                      | `pretty` on a terminal, else `json` |
| `time`       | `clock`, `full`, `off`: a pretty line's time | `clock`                             |
| `hideStream` | leaves the stream out of a pretty line       | shown                               |
| `to`         | where lines go                               | standard error                      |
| `level`      | the least level printed and kept             | every level                         |

`console: 'off'` prints nothing and keeps every line: a server that ships its
console elsewhere, or a test.

## The environment wins

So that a deployment changes how lines look without a build, the environment
goes over the code:

| Variable     | Values                                    |
|--------------|-------------------------------------------|
| `LOG_LEVEL`  | `trace`, `debug`, `info`, `warn`, `error` |
| `LOG_FORMAT` | `pretty`, `json`                          |
| `LOG_TIME`   | `clock`, `full`, `off`                    |

- `env: { prefix: 'APP' }` reads `APP_LOG_LEVEL` and the rest instead;
  `env: false` reads none.
- The environment never turns on a console the code turned off.
- A value none of them can mean is ignored, and said once.

Colours need a terminal, and neither `NO_COLOR` nor `TERM=dumb`.
`FORCE_COLOR` makes a pipe pretty and coloured; `NO_COLOR` still wins.

## A console without a store

A program that wants the logger and no store, a CLI, a script, takes it from
the package. The same calls, the same options but `buffer` and `keep`, the
same lines, kept nowhere:

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
