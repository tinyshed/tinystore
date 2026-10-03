# Introduction

TinyStore gives your application SQL databases, key-value storage, background
jobs, file storage, logs and metrics in a single directory. Go programs use it
as an embedded library. Bun, Node and Python programs use it through a small
sidecar process that their SDK starts automatically.

## Why TinyStore

An application on a single server usually ends up with a stack of services:
PostgreSQL for data, Redis for sessions and rate limits, a queue for
background jobs, S3 or a folder for uploads, a log system, and Prometheus for
metrics. You have to install, configure, secure, upgrade and back up each of
them, and each one can fail in its own way.

TinyStore replaces that stack with one directory that belongs to your
application:

```text
data/
├── sql/app.db     your SQL databases
├── kv.db          sessions, one-time codes, counters, quotas, settings
├── jobs.db        background jobs and schedules
├── blobs/         files
├── records.db     logs and events
└── metrics.db     metrics
```

You open the whole store with one call:

```ts
import { open } from '@tinyshed/tinystore'

await using store = await open('./data')
```

```python
import tinystore

async with tinystore.open("./data") as store:
    ...
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
if err != nil {
	return err
}
defer store.Close(ctx)
```

## What's included

TinyStore has six engines, and each one has its own section in these docs:

- **SQL**: your own SQLite databases. Migrations are checked when the program
  starts, and writes from concurrent requests are committed together.
- **KV**: key-value storage for sessions that stay alive while users are
  active, sign-in codes that work only once, counters, rate limits,
  [quotas](kv/quotas.md) such as "100 messages per 5 hours and 300 per week",
  settings that update in every process at the same time, and functions that
  run only once per idempotency key.
- **Jobs**: background work that runs at a given time, retries with a growing
  delay, survives crashes, and repeats on a schedule. You can show users their
  place in the queue and the progress of their job, and an AI agent can keep
  its finished steps when a job is retried.
- **Blobs**: files of any size. A file is either written completely or not at
  all. You can serve files with HTTP range requests, and TinyStore verifies a
  file's contents when you read it in full.
- **Records**: logs and events. The logger prints to your console and also
  stores every line, so you can search your logs by time, level, trace,
  fields and text. Records can also capture the output of other programs, and
  they join the lines of a stack trace into a single record.
- **Metrics**: counters, gauges and timers. TinyStore stores every value
  exactly, and rounds an aggregate only once, at the end.

The `tinystore` command line tool shows what a store contains, follows its
logs, and gives AI agents read-only access to it over MCP.

## How it runs

- **Go**: the engines run inside your process. You open only the engines you
  need, for example with `kv.Open(ctx, store, …)`, and your binary includes
  only those.
- **Bun, Node and Python**: the SDK talks to `tinystore serve`, a binary that
  ships inside the package. It connects over a local socket, or a named pipe
  on Windows. All processes that open the same directory share one sidecar,
  for example four web workers and a cron script. The sidecar exits after it
  has had no connections for 30 seconds.
- **Remote**: the same server can listen on TLS, so that clients on other
  machines can connect with a token.

Operations behave the same way in all three cases, because they run the same
engine code.

## What TinyStore is not

- **It is not a cluster.** One process owns a directory at a time, and other
  processes access the data through that process. If several machines need
  to write the same data, use a database server.
- **It is not a faster database engine.** All engines are built on SQLite,
  translated to Go so that you don't need cgo. Specialized databases are
  faster at their own workloads. TinyStore makes the combined workload of a
  single application cheap instead.

In our benchmark, an application with 64 concurrent clients handled 1.49 times
as many requests per second on TinyStore as on Redis, PostgreSQL,
VictoriaMetrics and plain files. It ran in a Linux container on a Ryzen 7 7700
with Go 1.27.1, with jobs committed in the same transaction as their rows. The
[full report](https://github.com/tinyshed/research/blob/main/tinystore/reports/runtime-continuation-2026-10-01.md)
has every setting and number.

> [!IMPORTANT]
> **Not released yet**
> All engines are built and tested, but nothing is published yet, and the API
> may still change. Files written by the current version may not open in the
> first release.

## How to read these docs

Start with [Getting started](getting-started.md) and read the "Start here"
pages in order. Then read the sections about the engines your application
uses. Each section starts with an overview, followed by one page per feature.
"Running it" covers the sidecar, servers, backups and testing. "How it works"
explains durability, time and limits in depth.

Every example is available in Bun, Python and Go. Choose your language with
the switch above any code block, and every page will remember your choice.
