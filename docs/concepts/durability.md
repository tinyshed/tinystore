# Durability

This page lists what is on disk when a call returns, and what a crash can
lose. Most calls return only after their data is synced to disk. A few parts
of TinyStore keep data in memory on purpose, to make a frequent operation
cheap, and each of them says how much a crash can lose.

## What a returned call guarantees

| Call | When it returns | A crash loses |
|---|---|---|
| KV `set`, `delete`, `take`, a quota's `allow` | after the disk sync | nothing |
| SQL `exec`, `batch`, Go `Tx` | after the disk sync | nothing |
| jobs `enqueue`, `update`, `cancel` | after the disk sync | nothing |
| blobs `put`, `move`, `copy`, `delete` | after the file and the row are synced | nothing |
| records `append` | after the disk sync | nothing |
| metrics `ingest` | after the disk sync | nothing |
| KV counters with `loseAtMost` | at once, in memory | the changes since the last write, at most the interval |
| the rate limiter | at once, in memory | up to one second of requests |
| a sliding expiry's extension | at once, in memory | extensions from the last second |
| a logger's line | at once, in memory | lines from the last second |
| a metrics instrument | at once, in memory | values from the last 15 seconds |
| a job's progress | at once, in memory | the progress, which the next attempt reports again |

Every engine uses SQLite in write-ahead log mode with full syncs. TinyStore's
tests kill the process at every step of a write and check what the next open
finds. A power failure can't be tested the same way: its safety follows from
the order of the syncs.

## Grouped commits

A disk sync takes about a millisecond on a good SSD, and much longer on a
cloud disk. If every write waited for its own sync, a busy program would wait
mostly for the disk. TinyStore instead commits all writes that arrive while
the previous commit runs as one group, with one sync. Each write still has its
own savepoint, so a write that fails is rolled back alone.

## Outcome unknown

If the sync of a group fails, TinyStore can't know whether the data reached
the disk. Each write of the group then fails with an outcome unknown error
(`ErrOutcomeUnknown` in Go, `OutcomeUnknownError` in Bun and Python). Read
what you wrote, then decide whether to write it again. The same error is
returned when a connection to the server drops while a write is in flight.

## Jobs run at least once

A job is saved before `enqueue` returns, and it is removed only after its
handler finished. If the process crashes between the handler's work and that
removal, the job runs again. Make handlers safe to run twice, for example
with `on conflict do nothing` or an idempotency key. See
[Jobs](../jobs/README.md#at-least-once).

## Blob uploads

A blob's bytes are written to a temporary file, synced, moved into place, and
only then named by a committed database row. A database row never points to a
file that is incomplete, and the next open removes what an interrupted upload
left behind. See [Uploads](../blobs/uploads.md).

## See also

- [Concurrency](concurrency.md): writers, readers and groups.
- [Backups](../running/backups.md): copies that survive losing the disk.
