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

The wheel installs the binary as the `tinystore` command too:
`tinystore logs ./data -f`, or without installing anything
`uvx --from tinyshed-tinystore tinystore status ./data`, and for an AI agent
`claude mcp add tinystore -- uvx --from tinyshed-tinystore tinystore mcp ./data`.

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

```python
cfg = await store.kv.config("app", Settings, file=tomllib.load(f), env_file=".env")
cfg.value.port  # PORT=3000 in .env makes it 3000
await cfg.update({"port": 4000})  # kept: 4000 after a restart too, at once in every process
await cfg.reset("port")  # back to .env's 3000
cfg.watch(lambda c: server.set_port(c.port))

limit = store.kv.limiter("api", rate="100/s", burst=20)
ok, left, retry_after = await limit.of(tenant).allow(user_id)

ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
usage = await ai.allow(user.id)  # one of each window, or none: usage.windows["weekly"].left
```

A config is a dataclass or a model whose defaults are its own: a file's
values, then the environment, then what `update` kept go over them, a variable
named by its field, `db_url` as `DB_URL`, after `prefix` when given. A number,
`true`, a list `a.com,b.com` or JSON read as the default's kind, and one that
does not is `InvalidError` at open, naming it. `env={"db_url":
"DATABASE_URL"}` names a variable itself; `secret` fields are never kept;
`validate` checks each change. A quota's windows count a use together or not
at all, each from a key's first use; `get` reads them without using any,
`refund` gives uses back.

```python
charges = store.kv.once("charges", Receipt)  # answers kept a day
receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))
```

`run` returns the answer kept under a key, or runs the function and keeps its
answer: a call of the key meanwhile, from any client, waits for it and gets
the same answer, and an exception keeps nothing, so the next call runs again.
The function runs once a key while its call lives; an effect outside the
store, a charge, carries the key too.

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

```python
videos = store.jobs.queue("videos", Video, max_running=2)
await videos.enqueue(video, key=video.id)
async for s in videos.watch(video.id):  # waiting 3 … running 0.4 … done
    await send(s.state, s.ahead, s.progress)


async def transcode(job: tinystore.Job[Video]) -> None:
    await encode(job.value, on_progress=job.progress)


await videos.work(transcode)
```

`get` says where a job is: `waiting`, with how many jobs run `ahead` of it,
`running`, with the `progress` its handler last reported, `failed`, or `done`
while `keep_done` keeps its key; `watch` yields it again at each change until
it ends, `cancelled` included, and `ran` and `took` say when the last run a
handler finished began and how many seconds it took: a schedule's last run
beside its next, `at`. `job.progress` takes any JSON within 4 KiB and
sends the latest at most ten times a second. `cancel` takes a running job too:
its handler's task is cancelled, and what it leaves settles nothing.
`max_running` bounds the jobs running at once across every worker of the
store.

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
latency = store.metrics.timer("http_request_ms")
with latency.labels(route="/users").measure():  # await inside is timed too
    user = await users.get(user_id)

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

A timer's `measure()` times its block whether it returns or raises, in a `with`
or an `async with`; `record(d)` adds a duration of its own, seconds, a
`timedelta` or `"250ms"`. Every flush writes `http_request_ms_count` and
`http_request_ms_sum`, counters, and the longest since the flush before,
`http_request_ms_max`, so that a range's mean is the increase of its sum over
the increase of its count.

## Durations, errors and cancellation

A duration is a `timedelta`, seconds, or text such as `"1h30m"`. Every error
is its code's class, a `TinystoreError`: `InvalidError`, `ConflictError`,
`LimitError`, `TooOldError` and the rest, each carrying what it names; a
`LimitError` names the bound, what the call wanted and the bound. A cancelled
task cancels its call, one `CANCEL` on the wire. `await store.status()` says
what the server is: its version, its protocol, its engines.
