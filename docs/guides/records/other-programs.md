# Other programs' output

A program often runs others: a worker in another language, `ffmpeg`, a
database's own process. `lines()` turns their output into events of a
stream, a line each, with a stack trace joined into one event and a JSON or
logfmt line kept as its fields. A writer never waits, as a logger never does.

```ts
const out = store.events('worker').lines()
child.stdout.on('data', chunk => out.write(chunk))
child.on('close', () => out.end()) // hands over what it holds
```

```python
async with store.events("worker").lines() as out:  # leaving the block hands over what it holds
    async for chunk in child.stdout:
        await out.write(chunk)
```

```go
worker, err := records.OpenEvents(store, "worker")
out, err := worker.Lines(ctx) // an io.WriteCloser
cmd.Stdout = out
err = cmd.Run()
err = out.Close() // hands over what it holds
```

```rust
let mut out = store.events("worker").open()?.lines()?;
std::io::copy(child.stdout.as_mut().unwrap(), &mut out)?; // dropped, it hands over what it holds
```

## What a line becomes

```text
2026-09-26 12:00:01,500 ERROR request failed   ┐
Traceback (most recent call last):             │ one event named log, its level error,
  File "app.py", line 3, in handle             │ its message the four lines
ValueError: bad input                          ┘
{"level":30,"msg":"ok"}                        an event named json: its fields level and msg
level=warn msg="slow request" ms=1200          an event named logfmt: its fields its pairs
```

| A line                                  | Becomes                                                   |
|-----------------------------------------|-----------------------------------------------------------|
| text                                    | an event named `log`, the line its message, byte for byte |
| a stack trace or a traceback            | joined with the lines before and after it into one event  |
| a JSON object                           | an event named `json`, its keys the event's `fields`      |
| a JSON value printed over several lines | one event                                                 |
| `key=value` pairs, logfmt               | an event named `logfmt`, its pairs the event's `fields`   |

- Each line becomes an event at the time it arrived.
- A JSON or logfmt line becomes fields only when they spell the line again;
  otherwise it is kept as text. A query finds those fields with `where`.
- The level is found where pino, logfmt, glog, Redis, log4j, zerolog and
  Postgres write it, in colour or not, so `minLevel: 'warn'` works on another
  program's output too.
- A line that is not UTF-8 keeps U+FFFD for each byte that spells nothing.

## Never waits

What does not fit the writer's buffer is dropped and counted in
`out.dropped`, and a line longer than an event holds, 256 KiB, is dropped
whole and counted. A chatty child never makes its parent wait on a disk.

A child's output has no trace: run each child with its own stream, or put the
job's id in the stream's name, `worker-41`, and read it back by stream.

## See also

- [Records](README.md): the engine's overview.
- [Reading](reading.md): finding those lines again.
