# TinyStore for Python

kv, jobs, blobs, SQL, records and metrics in one directory, served by a
sidecar the SDK starts. A Go program embeds the same engines;
[docs/sdk.md](https://github.com/tinyshed/tinystore/blob/main/docs/sdk.md) has
the three languages side by side.

```sh
pip install tinyshed-tinystore
```

Then `import tinystore`. This platform's wheel carries the `tinystore` binary;
`TINYSTORE_BIN`, `open`'s `binary` or `PATH` name another. Python 3.12 or
later, asyncio.

## Open

```python
import tinystore

async with tinystore.open("./data") as store:  # the directory's sidecar, started when none runs
    ...
async with tinystore.open(directory, private=True) as store:  # a child of this process, for tests and scripts
    ...
async with tinystore.connect("tls://db.internal:7070", token=token) as store:
    ...
```

`open` returns once the server has answered, so a directory that cannot be
served fails there. Closing hands over the handlers' records and the
instruments' last values; a sidecar leaves once its last connection has been
idle 30 s.

## kv

```python
sessions = store.kv.bucket("sessions", Session, sliding="30d")
await sessions.of(user.id).set(token, session)
found = await sessions.of(user.id).get(token)  # None when absent or expired

views = store.kv.counters("views")
await views.add("/home")

page = await sessions.scan(limit=100)  # Page(items, next); next goes back as after
async for entry in sessions.all():
    ...
```

A bucket's type is given once, where it opens: `str`, `bytes`, `int`,
`float`, `bool`, or a dataclass, a `TypedDict` or anything else JSON holds.

## jobs

```python
reminders = store.jobs.queue("reminders", Reminder)
await reminders.enqueue(Reminder(note=1), after="1h", key="note/1")


async def remind(job: tinystore.Job[Reminder]) -> None:
    await send(job.value.note)


await reminders.work(remind, workers=4)  # until the task is cancelled
```

A handler's return acknowledges its job and an exception retries it, waiting
longer each time; `job.retry`, `job.fail` and `job.snooze` say otherwise.
Cancelled, `work` gives back the jobs its handlers did not finish, uncounted.

## blobs

```python
files = store.blobs.bucket("files")
await files.put("avatars/42.png", Path("avatar.png").read_bytes(), content_type="image/png")
avatar = await files.get("avatars/42.png")  # None when absent
data = await avatar.read()  # a whole read checks every byte
```

## SQL

```python
app = await store.sql("app", migrations="./migrations")
await app.exec("insert into notes (body) values (?)", body)
notes = await app.all(Note, "select id, body from notes where author = ?", author)
async with app.batch() as tx:  # one transaction, all or none
    tx.exec("update notes set body = ? where id = ?", body, note_id)
    tx.exec("insert into edits (note) values (?)", note_id)
```

On Python 3.14 a statement may be a template, `t"… where author = {author}"`,
its values arguments and never SQL. A value comes back as SQLite keeps it.

## records

```python
logging.getLogger().addHandler(store.records.handler("api", redact=["password"]))  # never waits for the server
with tinystore.trace(trace_id, span_id):  # what is logged inside carries the trace
    logging.info("charged")

page = await store.records.scan(since="1h", min_level="warn", limit=100)
resets = await store.records.scan(since="1h", search="connection reset")  # case ignored
more = await store.records.scan(since="1h", min_level="warn", limit=100, after=page.next)
async for record in store.records.all(since="24h", trace_id=trace):
    ...

worker = store.records.lines("worker")  # another program's output, cut anywhere
async for chunk in process.stdout:
    worker.write(chunk)
```

A handler holds 1024 records and hands them over every second; what does not
fit is dropped and counted.

Each line also goes to stderr as it is logged: pretty on a terminal, one JSON
object a line otherwise, the bytes Go's and Bun's loggers write.
`console="pretty" | "json" | "off"` and `stdout=True` choose; `redact` hides
the values of fields of those names, at any depth, the case ignored, in the
store and on the console. A program that wants the logger and not the records
takes a handler without a store; its children are `logging`'s own:

```python
logging.basicConfig(handlers=[tinystore.handler("app", redact=["password"])], level=logging.INFO)
log = logging.getLogger("app.db")  # the console alone, nothing kept
```

## metrics

```python
store.metrics.counter("http_requests_total").labels(route="/users").inc()
store.metrics.gauge("queue_depth").set(12)

await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"}, "samples": [(now, 0.42)]})
series = await store.metrics.read(name="cpu", match={"host": "web-1"}, since="1h")
failing = await store.metrics.read(
    name="http_requests_total", since="1h", where={"status": tinystore.one_of("500", "502")}
)
buckets = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="increase")
routes = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="rate", by=["route"])
```

A sample comes back bit for bit, `-0.0` and a NaN's payload included; a range
is `since`, or `from_` and `to`. `metrics.explain(...)` says what a read or an
aggregate would spend of its limits before it runs.

## Durations, errors and cancellation

A duration is a `timedelta`, seconds, or text such as `"1h30m"`. Every error
is its code's class, a `TinystoreError`: `InvalidError`, `ConflictError`,
`LimitError`, `TooOldError` and the rest, each carrying what it names; a
`LimitError` names the bound, what the call wanted and the bound. A cancelled
task cancels its call, one `CANCEL` on the wire. `await store.status()` says
what the server is: its version, its protocol, its engines.
