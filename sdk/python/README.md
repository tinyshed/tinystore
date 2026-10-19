<!--
This is the page PyPI shows. The headline, the sample and the engines are the
landing page's: task readme writes them between the landing: markers from
web/landing.md. Every link is absolute, because PyPI cannot follow a relative
one. Every call, by engine, is in docs/reference/python/.
-->

# TinyStore for Python

<!-- landing:headline -->

**A small storage runtime for applications.** SQL, key-value state, durable jobs, files, search, logs and metrics in one directory, with one lifecycle, one memory budget and one backup.

<!-- /landing:headline -->

> **Not released yet.** Until `v0.1.0`, the API may still change, and a file
> written by one version may not open in the next.

The SDK starts a small `tinystore` server next to your program, called the
sidecar, and talks to it over a local connection, so that several processes
share one store. With `embedded=True` the core runs inside your process
instead. The server and the core come with the wheel, so there is nothing
else to install. It runs on Python 3.12 or later, on asyncio.

<!-- landing:sample -->

```python
async with await tinystore.open("data") as store:
    db = await store.database("app", migrations="migrations")
    await db.exec("insert into users (id, name) values (?, ?)", 42, "Ada")

    sessions = store.bucket("sessions", Session, idle="30d")
    await sessions.set(digest(token), Session(user_id=42))

    emails = store.queue("emails", Email)
    await emails.add(Email(to="ada@example.com"))

    avatars = store.files("avatars")
    await avatars.put("42.png", Path("avatar.png").read_bytes())

    store.logger("api").info("user created", user_id=42)
    store.metrics.counter("signups_total").inc()
```

```sh
pip install tinyshed-tinystore
```

<!-- /landing:sample -->

## Engines

<!-- landing:engines -->

|                                                                                           |                   |                                                                         |
|-------------------------------------------------------------------------------------------|-------------------|-------------------------------------------------------------------------|
| [SQL](https://github.com/tinyshed/tinystore/blob/main/docs/chat/messages.md)              | Relational state  | Your own SQLite databases: checked migrations, a query builder.         |
| [KV](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)       | Application state | Values with expiry and versions; counters, rate limits, quotas, `once`. |
| [Jobs](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)     | Background work   | Work that runs at its time: retries, steps, schedules, limits.          |
| [Blobs](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)    | Files             | Files by path, written whole or not at all, checked when read.          |
| [Search](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)   | Words and vectors | Indexes of documents or of your tables, by words, by meaning, or both.  |
| [Channels](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines) | Messages          | Messages to whoever listens now, sent from a transaction at its commit. |
| [Locks](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)    | Coordination      | Locks and semaphores with fences, across processes and connections.     |
| [Config](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)   | Settings          | Files and the environment in layers; secrets; settings changed live.    |
| [Records](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)  | Logs and events   | Read back by time, level, fields and text; followed as they arrive.     |
| [Metrics](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)  | Time series       | Samples kept bit for bit, answered exactly.                             |
| [Backup](https://github.com/tinyshed/tinystore/blob/main/docs/start/why.md#the-engines)   | The whole store   | One checked zip of every engine, taken while it runs.                   |

<!-- /landing:engines -->

## Documentation

- [Your first store](https://github.com/tinyshed/tinystore/blob/main/docs/start/first-store.md): install it and write a first program
- [One directory, many engines](https://github.com/tinyshed/tinystore/blob/main/docs/start/directory.md): the files, and the ways to reach a store
- [The documentation](https://github.com/tinyshed/tinystore/blob/main/docs/README.md): the book, each example in TypeScript, Python, Go and Rust
- [Python API](https://github.com/tinyshed/tinystore/blob/main/docs/reference/python/store.md): every call, by engine

## License

Apache-2.0.
