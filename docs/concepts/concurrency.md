# Concurrency

Each engine has its own SQLite file, and each file has one writer and several
readers. This page explains how calls from many goroutines, tasks and
processes share them, and why a busy engine never slows down another one.

## One writer per file

```text
data/kv.db        one writer, its own readers
data/jobs.db      one writer, its own readers
data/sql/app.db   one writer, 8 readers
data/records.db   one writer, its own readers
data/metrics.db   one writer, 2 readers
```

SQLite allows one writer per file at a time. TinyStore gives every engine a
file of its own, so a burst of log lines never waits for a slow SQL write,
and a long KV transaction never blocks jobs.

The flip side: no write spans two engines. A KV write and a job are two
commits. When a job must be committed together with your rows, put the queue
into your SQL database. See [Jobs and your data](../jobs/your-data.md).

## Writes wait in a queue, not in a lock

Inside one file, writes wait for the writer in a queue, in the order they
arrived, and are committed in groups. You never see SQLite's `database is
locked` error. A caller whose context is cancelled while it waits leaves the
queue without writing anything. A write that has already started finishes
with its group.

## Readers don't block writers

Reads run on separate connections in parallel, each from a snapshot of the
last commit. A reader never blocks the writer, and the writer never blocks a
reader. A point read, such as a KV `get`, is a single statement and holds its
snapshot only for that statement.

A read that spans several statements, such as a view, holds one snapshot for
at most five seconds. A longer snapshot would make the write-ahead log grow,
because SQLite can't reuse the log while an old reader needs it.

## Transactions and batches

| | Holds the writer while your code runs | Disk syncs |
|---|---|---|
| a single write | no | shared with its group |
| a batch | no: it is built first, then committed in a group | shared with its group |
| a Go `Tx` | yes | one of its own |

A batch can be committed together with other writes because your code doesn't
run while the writer waits. A `Tx` runs your code, so it holds the writer
alone. Prefer a batch.

## Many processes

In Bun, Node and Python, every process that opens a directory talks to the
same sidecar, so their calls meet in the same queues and groups. Ten web
workers that write to KV share disk syncs as goroutines in one Go program do.

Many calls can be in flight on one connection at once. Throughput comes from
sending calls together, not from opening more connections. See
[Go, Bun and Python](../languages.md#send-calls-together).

## Jobs

Jobs of one queue run in order with one worker. With several workers, or
several processes calling `work`, they run in parallel, and each job runs in
only one worker at a time, protected by its lease. `maxRunning` limits how
many jobs of a queue run at once across all workers.

## See also

- [Durability](durability.md): what a returned write guarantees.
- [Memory and limits](memory.md): how concurrent work is bounded.
