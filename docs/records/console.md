# Console output

The records logger prints each line to the console when you log it: formatted
for people on a terminal, and as JSON everywhere else. You choose the format,
how the time looks, whether the stream is shown and where the lines go. The
same settings can come from environment variables, so a deployment changes its
logs without a new build.

## Choose the format

```ts
const log = store.records.logger('api', { console: 'json' })
```

```python
logging.basicConfig(handlers=[store.records.handler("api", console="json")], level=logging.INFO)
```

```go
import "github.com/tinyshed/tinystore/records/console"

logs, err := records.Open(ctx, store, records.Options{})
logger := slog.New(logs.Handler("api", console.JSON))
```

Without a format, a terminal gets pretty lines in color:

```log
11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
```

Anything else, such as a pipe in a container, gets one JSON object per line,
which log collectors expect:

```text
{"time":"2026-10-02T11:02:14.502Z","level":"WARN","stream":"api","msg":"slow request","requestId":"7f3a","ms":1200}
```

`'off'` prints nothing, and the store still keeps every line. Go, Bun and
Python print exactly the same bytes for the same line.

| Option                     | Go                                         | Bun                                    | Python                                |
|----------------------------|--------------------------------------------|----------------------------------------|---------------------------------------|
| format                     | `console.Pretty`, `JSON`, `Off`            | `console: 'pretty' \| 'json' \| 'off'` | `console="pretty" \| "json" \| "off"` |
| time on a pretty line      | `console.TimeClock`, `TimeFull`, `TimeOff` | `time: 'clock' \| 'full' \| 'off'`     | `time="clock" \| "full" \| "off"`     |
| no stream on a pretty line | `console.HideStream`                       | `hideStream: true`                     | `hide_stream=True`                    |
| where lines go             | `console.To(w)`                            | `to: process.stdout`                   | `to=sys.stdout`                       |
| lowest level kept          | `console.Level(slog.LevelInfo)`            | `level: 'info'`                        | `level=logging.INFO`                  |
| hidden fields              | `console.Redact("password")`               | `redact: ['password']`                 | `redact=["password"]`                 |
| where a line was logged    | `console.AddSource`                        | `source: true`                         | `source=True`                         |
| the environment's prefix   | `console.FromEnv("MYAPP")`                 | `env: fromEnv('MYAPP')`                | `env=tinystore.from_env("MYAPP")`     |
| no environment             | `console.NoEnv`                            | `env: false`                           | `env=False`                           |

## Change the time and the stream

```ts
const log = store.records.logger('api', { time: 'off', hideStream: true })
```

```python
handler = store.records.handler("api", time="off", hide_stream=True)
```

```go
logger := slog.New(logs.Handler("api", console.TimeOff, console.HideStream))
```

The same line as above now starts with its level:

```log
WARN  slow request  requestId=7f3a ms=1200
```

| `time`               | A pretty line starts with        |
|----------------------|----------------------------------|
| `clock`, the default | `11:02:14.502`                   |
| `full`               | `2026-10-02 11:02:14.502 +03:00` |
| `off`                | its level                        |

Use `off` when something else stamps each line already, such as
`docker logs -t` or journald. Use `full` for logs that you read days later. Hide
the stream when your program has one logger, and the name on every line tells
you nothing. These options change only pretty lines. A JSON line always has its
time and stream, because log collectors read them, and the store keeps the
stream either way.

## Print somewhere else

```ts
import { createWriteStream } from 'node:fs'

const log = store.records.logger('api', { to: createWriteStream('api.log', { flags: 'a' }) })
```

```python
handler = store.records.handler("api", to=open("api.log", "a", encoding="utf-8"))
```

```go
file, err := os.OpenFile("api.log", os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
logger := slog.New(logs.Handler("api", console.To(file)))
```

Lines go to stderr by default, because a library shouldn't write into what a
program prints to stdout, such as a command's output. `to` sends them to stdout,
a file or any other writer. A file is not a terminal, so its lines are JSON
unless you set the format to pretty.

## Set it from the environment

```sh for=bun
LOG_LEVEL=debug LOG_FORMAT=json bun app.ts
```

```sh for=python
LOG_LEVEL=debug LOG_FORMAT=json python app.py
```

```sh for=go
LOG_LEVEL=debug LOG_FORMAT=json go run .
```

| Variable     | Values                           | Replaces                  |
|--------------|----------------------------------|---------------------------|
| `LOG_LEVEL`  | `debug`, `info`, `warn`, `error` | the lowest level kept     |
| `LOG_FORMAT` | `pretty`, `json`, `off`          | the format                |
| `LOG_TIME`   | `clock`, `full`, `off`           | the time on a pretty line |

A variable wins over what your code says, so you can turn on debug lines in
production without a new build. A logger whose code turns its console off
stays off, so `LOG_FORMAT` never prints a logger of events. Case doesn't
matter. If a value isn't one of these, the logger ignores it and prints one
warning:

```log
11:02:14.502 WARN  tinystore  LOG_LEVEL is ignored  value=verbose expected="debug, info, warn or error"
```

In Python, a logger also has its own level, which `basicConfig` sets, and it
filters lines before any handler sees them. For `LOG_LEVEL=debug` to show debug
lines, give `basicConfig` the lowest level and the handler the usual one:

```python
logging.basicConfig(handlers=[store.records.handler("api", logging.INFO)], level=logging.DEBUG)
```

### Use your own prefix

```ts
import { fromEnv } from '@tinyshed/tinystore'

const log = store.records.logger('api', { env: fromEnv('MYAPP') }) // MYAPP_LOG_LEVEL and the rest
```

```python
handler = store.records.handler("api", env=tinystore.from_env("MYAPP"))  # MYAPP_LOG_LEVEL and the rest
```

```go
logger := slog.New(logs.Handler("api", console.FromEnv("MYAPP"))) // MYAPP_LOG_LEVEL and the rest
```

A compose file shared by several services often sets `LOG_LEVEL` for one of
them. With a prefix, the logger reads `MYAPP_LOG_LEVEL`, `MYAPP_LOG_FORMAT` and
`MYAPP_LOG_TIME`, and ignores the variables without it. `fromEnv` is the same
function that gives a [config](../kv/configs.md) its variables.

To read no variables at all, so that only your code decides, set `env: false`
in Bun, `env=False` in Python or `console.NoEnv` in Go. In Go,
`console.FromLookup("MYAPP", lookup)` reads the variables through your own
function instead of the process's environment, which keeps tests apart from
the machine they run on.

## Show where a line was logged

```ts
const log = store.records.logger('api', { source: true })
```

```python
handler = store.records.handler("api", source=True)
```

```go
logger := slog.New(logs.Handler("api", console.AddSource))
```

```log
11:02:14.502 DEBUG api  cache miss  key=user:42 source=server/cache.go:42
```

Each line gets a `source` field with the function, the file and the line that
logged it, spelled as slog spells it:
`{"function":"main.load","file":"/app/server/cache.go","line":42}`. A pretty
line shows only the file's directory, its name and the line. Finding the
caller costs time on every line, so turn it on while you debug.

## Colors in an IDE

The run console of an IDE, such as GoLand, WebStorm or PyCharm, reads your
program through a pipe, not a terminal. So by default the logger prints JSON
without colors there. Set `FORCE_COLOR=1` in the run configuration's
environment variables, and the logger prints pretty lines in color:

```sh for=bun
FORCE_COLOR=1 bun app.ts
```

```sh for=python
FORCE_COLOR=1 python app.py
```

```sh for=go
FORCE_COLOR=1 go run .
```

`FORCE_COLOR` turns colors on wherever the program runs, so set it where you
run the program, not in production. Colors are off when `NO_COLOR` is set or
`TERM` is `dumb`, and `NO_COLOR` wins over `FORCE_COLOR`.

## Use your own console

If you want a console that TinyStore doesn't print, turn its console off and
print your own beside it. The store still keeps every line.

```ts
import pino from 'pino'

const lines = store.records.lines('api')
const log = pino(pino.multistream([{ stream: process.stdout }, { stream: lines }]))
log.info({ port: 3000 }, 'server started') // pino prints it, and the store keeps it
```

```python
from rich.logging import RichHandler

logging.basicConfig(handlers=[store.records.handler("api", console="off"), RichHandler()], level=logging.INFO)
```

```go
logger := slog.New(slog.NewMultiHandler(
	logs.Handler("api", console.Off), // the store keeps every line
	tint.NewHandler(os.Stderr, nil),         // and any slog handler prints it
))
```

In Go and Python, the store's handler sits next to any other handler. In Bun,
your logger's output goes to the store through [`lines`](lines.md), which finds
the level and fields of pino's JSON lines.

## See also

- [Logging](logging.md): the logger, its context and its buffer.
- [records/README.md](../../records/README.md): the full contract of the
  console.
