# Other programs' output

Store the output of another program, such as a child process, a worker in
another language or a followed log file, as records. TinyStore splits the
output into lines, joins the lines of a stack trace into one record, finds
each line's level, and keeps every byte of the text.

## Capture a child process

```ts
const worker = Bun.spawn(['python', 'worker.py'], { stdout: 'pipe', stderr: 'pipe' })
const lines = store.records.lines('worker')

for await (const chunk of worker.stderr) {
	lines.write(chunk)
}
await lines.end()
```

```python
process = await asyncio.create_subprocess_exec("node", "worker.js", stderr=asyncio.subprocess.PIPE)
lines = store.records.lines("worker")

async for chunk in process.stderr:
    lines.write(chunk)
await lines.end()
```

```go
cmd := exec.Command("node", "worker.js")
cmd.Stdout = logs.Lines("worker")
cmd.Stderr = logs.Lines("worker")
err = cmd.Run()
```

You can write the output in pieces of any size, cut anywhere. A write never
waits: if the buffer is full, a line is dropped and counted, as the logger
does. When the stream ends, the last line is written too, even without a
trailing newline.

## What a line becomes

```text
level=info msg="slow request" ms=1200   → name logfmt, level info, attributes msg and ms
{"level":"error","msg":"db down"}         → name json, level error, attributes level and msg
[ERROR] 2026-10-03 failed to connect      → name log, level error, the line as its body
\x1b[32mINFO\x1b[0m started                → name log, level info: a colored level counts too
```

- **Text** is kept byte for byte as the record's body. A record named `log`
  can always be printed back exactly as it arrived.
- **A JSON object or a logfmt line** keeps its fields as attributes, so that
  you can search by them. TinyStore does this only if the fields can be
  written back as exactly the same line. Otherwise the line stays text.
- **The level** is found where common loggers write it: pino, logfmt, glog,
  Redis, log4j, zerolog and PostgreSQL, with or without colors. A `minLevel`
  query then finds their errors.
- **The time** of a record is when its first line arrived. A time written
  inside the line stays in the line's text, where it is compressed to a few
  bits.

## Stack traces

```text
Traceback (most recent call last):
  File "worker.py", line 12, in run
    handle(job)
ValueError: bad input
```

A stack trace, a Python traceback, a Java exception or a JSON value printed
over several lines becomes one record. A line that is indented, or that
continues a traceback or an exception, is joined to the record before it. A
record that may still grow is written once a second passes without a new
line joining it.

## Limits and defaults

| | |
|---|---|
| A line | up to 256 KiB; a longer line is dropped and counted |
| A record's time | when its first line arrived |
| Writes to the store | every second |

## See also

- [Reading](reading.md): search the captured lines.
- [records/README.md](../../records/README.md): the full contract of `lines`.
