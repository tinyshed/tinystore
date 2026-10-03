<!--
This is the page PyPI shows. The headline, the sample and the engines are the
landing page's: task readme writes them between the landing: markers from
web/landing.md. Every link is absolute, because PyPI cannot follow a relative
one. Every call, by engine, is in docs/reference/python.md.
-->

# TinyStore for Python

<!-- landing:headline -->

**A small storage runtime for applications.** SQL, key-value state, durable jobs, files, metrics and logs in one directory, with one lifecycle, one memory budget and one backup.

<!-- /landing:headline -->

> **Release candidates first.** Until `v0.1.0`, the API may still change, and
> a file written by one release may not open in the next.

The SDK starts a small `tinystore` server next to your program, called the
sidecar, and talks to it over a local connection. The server binary comes with the
wheel, so there is nothing else to install. Go programs embed the same
engines, and every client of one directory sees the same data.

<!-- landing:sample -->

```python
import asyncio
import logging
from pathlib import Path

import tinystore


async def main() -> None:
    async with tinystore.open("./data") as store:
        db = await store.sql(
            "app", migrations={"0001_users.sql": "create table users (id integer primary key, name text)"}
        )
        await db.exec("insert into users (id, name) values (?, ?)", 42, "Ada")

        sessions = store.kv.bucket("sessions", str, sliding="30d")
        await sessions.of("42").set("token", "abc123")

        emails = store.jobs.queue("emails", dict[str, str])
        await emails.enqueue({"to": "ada@example.com"})

        files = store.blobs.bucket("files")
        await files.put("avatars/42.png", Path("avatar.png").read_bytes())

        logging.getLogger().addHandler(store.records.handler("api"))
        logging.info("user created", extra={"user_id": 42})

        store.metrics.counter("signups_total").inc()


asyncio.run(main())
```

```sh
pip install tinyshed-tinystore
```

<!-- /landing:sample -->

It needs Python 3.12 or later and runs on asyncio. The package installs as
`tinyshed-tinystore` and imports as `tinystore`.

## Engines

<!-- landing:engines -->

| | | |
|---|---|---|
| [SQL](https://github.com/tinyshed/tinystore/blob/main/docs/sql/README.md) | Relational state | The application's own SQL databases: tables from structs, checked migrations. |
| [KV](https://github.com/tinyshed/tinystore/blob/main/docs/kv/README.md) | Application state | Current state: typed buckets, counters, expiry, versions. |
| [Jobs](https://github.com/tinyshed/tinystore/blob/main/docs/jobs/README.md) | Durable background work | Work that runs at its time: retries, leases, repeats. |
| [Blobs](https://github.com/tinyshed/tinystore/blob/main/docs/blobs/README.md) | Files and objects | Files by path, checked when read whole. |
| [Records](https://github.com/tinyshed/tinystore/blob/main/docs/records/README.md) | Logs and events | Read by time, level and keys. |
| [Metrics](https://github.com/tinyshed/tinystore/blob/main/docs/metrics/README.md) | Time series | Samples kept bit for bit, answered exactly. |

<!-- /landing:engines -->

## Three ways to connect

```python
async with tinystore.open("./data") as store:  # the directory's sidecar, shared with other processes
    ...
async with tinystore.open("./data", private=True) as store:  # a server for this process alone, for tests and scripts
    ...
async with tinystore.connect("tls://db.internal:7070", token=token) as store:
    ...
```

[The sidecar](https://github.com/tinyshed/tinystore/blob/main/docs/running/sidecar.md)
and [A remote server](https://github.com/tinyshed/tinystore/blob/main/docs/running/server.md)
explain each one.

## The command line

The wheel also installs the `tinystore` command. It shows what a store
contains and follows its logs while your program runs. `uvx` runs it without
installing anything:

```sh
uvx --from tinyshed-tinystore tinystore status ./data                          # each engine's size, and who serves the directory
tinystore logs ./data -f                                                       # the application's logs, as they arrive
claude mcp add tinystore -- uvx --from tinyshed-tinystore tinystore mcp ./data # read-only access for an AI agent
```

See [The command line](https://github.com/tinyshed/tinystore/blob/main/docs/running/cli.md)
for every command.

## Documentation

- [Getting started](https://github.com/tinyshed/tinystore/blob/main/docs/getting-started.md): install it and write a first program
- [A tour](https://github.com/tinyshed/tinystore/blob/main/docs/tour.md): every engine on one page
- [The guides](https://github.com/tinyshed/tinystore/blob/main/docs/README.md): a page per feature, each example in Bun, Python and Go
- [Python API](https://github.com/tinyshed/tinystore/blob/main/docs/reference/python.md): every call, by engine

## License

Apache-2.0.
