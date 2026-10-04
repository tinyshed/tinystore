# Python API

Every call of the Python SDK, `tinyshed-tinystore`, by engine. Each section
shows the calls in one block and then explains what they guarantee. The guides
explain each feature in more detail, with examples in Bun, Python and Go side
by side.

This page shows Python only. The [Bun and Node API](bun.md) is the same list
for Bun and Node.

## Install

```sh
pip install tinyshed-tinystore
```

The package needs Python 3.12 or later and runs on asyncio. It imports as
`tinystore`.

The wheel for your platform contains the `tinystore` binary. To use another
binary, set `TINYSTORE_BIN`, pass `binary` to `open`, or put `tinystore` on
your `PATH`.

The wheel also installs the binary as the `tinystore` command. `uvx` runs it
without installing anything:

```sh
tinystore logs ./data -f
uvx --from tinyshed-tinystore tinystore status ./data
claude mcp add tinystore -- uvx --from tinyshed-tinystore tinystore mcp ./data
```

## Open a store

```python
import tinystore

async with tinystore.open("./data") as store:  # the directory's sidecar, started if none runs
    ...
async with tinystore.open(directory, private=True) as store:  # a child of this process, for tests and scripts
    ...
async with tinystore.connect("tls://db.internal:7070", token=token) as store:
    ...

async with tinystore.open(directory, private=True, clock=datetime(2026, 10, 3, 9, tzinfo=UTC)) as store:
    await store.clock.advance("1h")  # keys expire and jobs come due without waiting
    await store.backup("backup.zip")  # every engine in one checked zip
```

`open` returns after the server has answered. If the server can't serve the
directory, `open` fails.

`open` takes a directory, never an address. If a server already serves the
directory, for example `tinystore serve ./data`, `open("./data")` finds it
through the directory's `SERVE` file. An address such as `tcp://…` or
`pipe:…` fails with `InvalidError`, which says what to call instead.

When you close the store, the SDK first sends the records that handlers still
hold and the last values of the instruments. A sidecar exits after its last
connection has been idle for 30 seconds.

`tinystore restore` restores a backup into an empty directory. See
[Backups](../running/backups.md).

A duration is a `timedelta`, a number of seconds, or text such as `"1h30m"`.

## KV

```python
sessions = store.kv.bucket("sessions", Session, sliding="30d")
await sessions.of(user.id).set(token, session)
found = await sessions.of(user.id).get(token)  # None if absent or expired
created, entry = await sessions.of(user.id).set_entry_if_absent(token, session)  # this one, or the one already there

views = store.kv.counters("views")
await views.add("/home")

page = await sessions.scan(limit=100)  # Page(items, next): pass next as after for the next page
async for entry in sessions.all():
    ...


async def sign_in(tx: tinystore.Tx) -> int:  # runs again if a key it read has changed
    user_id = await codes.with_tx(tx).take(digest(code))
    if user_id is None:
        raise InvalidCodeError
    sessions.of(user_id).with_tx(tx).set(digest(token), session)
    return user_id


user_id = await store.kv.tx(sign_in)
```

You give a bucket's type once, when you open it: `str`, `bytes`, `int`,
`float`, `bool`, or a dataclass, a `TypedDict` or any other type that JSON can
hold.

`set_if_absent` and `set_entry_if_absent` write only if no live key exists, in
one write. If two processes create the same key at once, for example a daily
salt or a claim, only one of them succeeds. A `get` followed by a `set` can't
guarantee that, because both processes may write.

See [KV](../kv/README.md), [Transactions](../kv/transactions.md) and
[Sessions](../kv/sessions.md).

### Configs, rate limits and quotas

```python
cfg = await store.kv.config("app", Settings, file=tomllib.load(f), env_file=".env")
cfg.value.port  # 3000 if .env sets PORT=3000
await cfg.update({"port": 4000})  # stored: 4000 after a restart too, in every process at once
await cfg.reset("port")  # back to 3000 from .env
cfg.watch(lambda c: server.set_port(c.port))

limit = store.kv.limiter("api", rate="100/s", burst=20)
ok, left, retry_after = await limit.of(tenant).allow(user_id)

ai = store.kv.quota("ai", session="100/5h", weekly="300/7d")
usage = await ai.allow(user.id)  # counts in every window or in none: usage.windows["weekly"].left
```

A config is a dataclass or a model, and takes its fields, types and defaults
from it. Each layer overrides the one before it:

1. the defaults;
2. the values from `file`;
3. the environment;
4. the values that `update` stored.

Each field reads the variable with its name in upper case: `db_url` reads
`DB_URL`, after `prefix` if you set one. To use another name, pass
`env={"db_url": "DATABASE_URL"}`. A variable is read as the default's type: a
number, `true`, a list such as `a.com,b.com`, or JSON. If it doesn't parse,
`config` fails with `InvalidError`, which names the variable. Fields in
`secret` are never stored, and `validate` checks each change.

A quota counts a use in all its windows, or in none of them. Each window
starts at a key's first use. `get` reads the windows without using anything,
and `refund` gives uses back.

See [Configs](../kv/configs.md), [Rate limits](../kv/rate-limits.md) and
[Quotas](../kv/quotas.md).

### Once

```python
charges = store.kv.once("charges", Receipt)  # answers are stored for a day
receipt = await charges.run(request_id, lambda: pay.charge(order, request_id))
```

`run` returns the answer stored under the key. If there is none, it runs the
function and stores its answer. If another call with the same key arrives
meanwhile, from any client, it waits and gets the same answer. If the
function raises, nothing is stored, and the next call runs it again.

The function runs once per key only while its call is alive. If the function
has an effect outside the store, such as a charge, pass the key to that
service too. See [Once](../kv/once.md).

## Jobs

```python
reminders = store.jobs.queue("reminders", Reminder)
await reminders.enqueue(Reminder(note=1), after="1h", key="note/1")


async def remind(job: tinystore.Job[Reminder]) -> None:
    await send(job.value.note)


await reminders.work(remind, workers=4)  # until the task is cancelled
```

If the handler returns, the job is done. If it raises, the job is retried
later, and the wait grows with each attempt. To decide yourself, call
`job.retry`, `job.fail` or `job.snooze`. When the task that runs `work` is
cancelled, the jobs that its handlers didn't finish go back to the queue, and
the attempt isn't counted.

```python
videos = store.jobs.queue("videos", Video, max_running=2)
await videos.enqueue(video, key=video.id)
async for s in videos.watch(video.id):  # waiting 3 … running 0.4 … done
    await send(s.state, s.ahead, s.progress)


async def transcode(job: tinystore.Job[Video]) -> None:
    await encode(job.value, on_progress=job.progress)


await videos.work(transcode)
```

`get` returns the job's state:

| State       | What it includes                                |
|-------------|-------------------------------------------------|
| `waiting`   | `ahead`: how many jobs run before it            |
| `running`   | `progress`: the value the handler reported last |
| `failed`    |                                                 |
| `done`      | only while `keep_done` keeps the key            |
| `cancelled` |                                                 |

`watch` yields the state again at each change until the job ends. `ran` and
`took` tell when the last finished run started and how many seconds it took.
For a schedule, `at` is the next run.

`job.progress` accepts any JSON up to 4 KiB, and sends the latest value at
most ten times per second. `cancel` also stops a running job: the handler's
task is cancelled, and whatever it leaves behind changes nothing.
`max_running` limits how many jobs of the queue run at once, across all
workers of the store.

A step stores its answer, so the next attempt of the same run doesn't run it
again:

```python
hits = await job.step("search", lambda: search(q))
```

See [Jobs](../jobs/README.md), [Watching a job](../jobs/watching.md) and
[Steps](../jobs/steps.md).

## Blobs

```python
files = store.blobs.bucket("files")
await files.put("avatars/42.png", Path("avatar.png").read_bytes(), content_type="image/png")
avatar = await files.get("avatars/42.png")  # None if absent
data = await avatar.read()  # a whole read checks every byte
```

See [Blobs](../blobs/README.md).

## SQL

```python
app = await store.sql("app", migrations="./migrations")
await app.exec("insert into notes (body) values (?)", body)
notes = await app.all(Note, "select id, body from notes where author = ?", author)
async with app.batch() as tx:  # one transaction: all or nothing
    tx.exec("update notes set body = ? where id = ?", body, note_id)
    tx.exec("insert into edits (note) values (?)", note_id)

index = store.jobs.queue("index", int, in_=app)  # the queue is stored in sql/app.db
async with app.batch() as tx:
    tx.exec("update notes set body = ? where id = ?", body, note_id)
    index.with_tx(tx).enqueue(note_id)  # commits with the update, or not at all
```

Without `migrations`, `store.sql` opens the file as it is, and creates an
empty one if there is none. Nothing is applied or checked, which is handy for
a first try: `await (await store.sql("scratch")).scalar("select 1")`.

On Python 3.14, a statement can be a template string:
`t"… where author = {author}"`. Its values are passed as arguments, never as
SQL. A value comes back as SQLite stores it. See [SQL](../sql/README.md) and
[Jobs and your data](../jobs/your-data.md).

## Records

```python
logging.getLogger().addHandler(store.records.handler("api", redact=["password"]))  # never waits for the server
with tinystore.trace(trace_id, span_id):  # what is logged inside gets the trace
    logging.info("charged")
with tinystore.context(request_id=request_id):  # every line inside gets request_id, in the tasks it starts too
    logging.warning("slow request", extra={"ms": 1200})

page = await store.records.scan(since="1h", min_level="warn", limit=100)
resets = await store.records.scan(since="1h", search="connection reset")  # case-insensitive
more = await store.records.scan(since="1h", min_level="warn", limit=100, after=page.next)
async for record in store.records.all(since="24h", trace_id=trace):
    ms = tinystore.fields(record.attrs)["ms"]  # a record's (key, json) pairs, parsed

worker = store.records.lines("worker")  # another program's output, split anywhere
async for chunk in process.stdout:
    worker.write(chunk)
```

A handler buffers up to 1024 records. It sends them every second, or as soon
as half of the buffer is full. If the buffer is full, a new record is dropped,
counted in `handler.dropped` and reported on stderr. Use a handler for lines
you can afford to lose in a burst. For records that you must keep, call
`records.append`, which returns after the records are stored. A larger
`buffer` holds a longer burst.

A handler also writes each line to stderr: pretty on a terminal, and one JSON
object per line otherwise. The bytes are the same as Go's and Bun's loggers
write. To change this, set `console` to `"pretty"`, `"json"` or `"off"`, or
set `stdout=True`. A handler of events, such as one view per request, usually
sets `"off"`, so it doesn't fill the program's log.

`redact` hides the values of fields with those names, at any depth and in any
case, both in the store and on the console.

To log to the console without a store, use a handler without one. Its
children are the usual `logging` loggers:

```python
logging.basicConfig(handlers=[tinystore.handler("app", redact=["password"])], level=logging.INFO)
log = logging.getLogger("app.db")  # the console only, nothing stored
```

See [Records](../records/README.md) and [Logging](../records/logging.md).

## Metrics

```python
store.metrics.counter("http_requests_total").labels(route="/users").inc()
store.metrics.gauge("queue_depth").set(12)
latency = store.metrics.timer("http_request_ms")
with latency.labels(route="/users").measure():  # an await inside is timed too
    user = await users.get(user_id)

await store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"host": "web-1"}, "samples": [(now, 0.42)]})
series = await store.metrics.read(name="cpu", match={"host": "web-1"}, since="1h")  # [Series(..., times, values)]
failing = await store.metrics.read(
    name="http_requests_total", since="1h", where={"status": tinystore.one_of("500", "502")}
)
buckets = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="increase")
routes = await store.metrics.aggregate(name="http_requests_total", since="24h", width="1h", op="rate", by=["route"])
```

`read` returns each series as two columns: `times` in Unix milliseconds and
`values` as an `array("d")`. `ingest` accepts the same columns, or
`(time, value)` pairs. A sample comes back bit for bit, including `-0.0` and a
NaN's payload. A range is either `since`, or `from_` and `to`.
`metrics.explain(...)` tells how much of its limits a read or an aggregate
would use, before it runs.

A timer's `measure()` times its block whether the block returns or raises,
in a `with` or an `async with`. `record(d)` adds a duration that you measured
yourself: seconds, a `timedelta` or text such as `"250ms"`. At every flush, a
timer writes three series:

| Series                  | What it contains                          |
|-------------------------|-------------------------------------------|
| `http_request_ms_count` | a counter of the measured calls           |
| `http_request_ms_sum`   | a counter of their total time             |
| `http_request_ms_max`   | the longest time since the previous flush |

The mean over a range is the increase of the sum divided by the increase of
the count.

If the store refuses a series, for example because of a label it can't
store, the instrument stops writing it and reports it once on stderr. The
other instruments keep working. See [Metrics](../metrics/README.md) and
[Instruments](../metrics/instruments.md).

## Errors and cancellation

Every error is a `TinystoreError`, with a class per error code:
`InvalidError`, `ConflictError`, `LimitError`, `TooOldError` and others. Each
error carries the details of what failed. A `LimitError` tells which limit it
hit, what the call asked for and the limit's value. See
[Errors](../concepts/errors.md).

When a task is cancelled, its call is cancelled too, and the SDK sends one
`CANCEL` to the server.

`await store.status()` returns the server's version, its protocol and its
engines.

Some failures happen when no call is waiting, so the SDK writes them to
stderr, as a handler with the stream `tinystore`:

- a failed instrument flush or handler write, and its recovery;
- a gauge function that raised;
- a refused instrument;
- the lines that a full handler dropped.

A repeated failure is reported again after ten minutes, or when it changes.
Go's store logs its own failures the same way.

## See also

- [Go, Bun and Python](../languages.md): how each language reaches a store
- [The sidecar](../running/sidecar.md) and [A remote server](../running/server.md)
- [Limits and defaults](limits.md)
- the SDK's source, `sdk/python/src/tinystore/`, and its tests, `sdk/python/tests/`
