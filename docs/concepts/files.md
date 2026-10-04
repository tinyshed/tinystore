# Files on disk

A store is one directory, and each engine has its own file in it. This page
shows what each file holds, which files appear when, and how the directory
grows and shrinks.

## The directory

```text
data/
├── LOCK               held by the process that owns the store
├── kv.db              KV
├── jobs.db            jobs
├── records.db         records
├── metrics.db         metrics
├── blobs/
│   ├── blobs.db       blob rows, and files up to 16 KiB
│   ├── objects/       larger files, one file each, named by number
│   └── uploads/       uploads in progress
├── sql/
│   └── app.db         your SQL databases, one file each
└── server/            the sidecar's address and socket, while it runs
```

An engine creates its file the first time you open the engine. A program that
uses only KV has only `kv.db`. Next to each `.db` file, SQLite keeps a
write-ahead log, `-wal`, and an index of it, `-shm`, while the file is open.

## One process owns the directory

The process that opens a store locks `LOCK` until it closes the store. A
second process that tries to open the same directory fails at once with
`ErrInUse`, instead of corrupting files. The operating system releases the
lock when the process dies, so a crash never leaves a directory locked.

In Bun and Python, the sidecar owns the directory, and your programs reach it
through the sidecar. See [The sidecar](../running/sidecar.md).

## Each engine is separate

Each file has its own writer, its own migrations and its own identity. A
damaged file stops only its own engine: if `metrics.db` can't be opened, your
program still opens KV and jobs. Your SQL databases live only under `sql/`,
so their names never collide with an engine's file.

When a jobs store is opened inside an SQL database, its tables live in that
database's file, named `_tinystore_jobs…`. Tables whose names start with
`_tinystore_` belong to TinyStore.

## Growth and cleanup

| Data            | Removed by                                              |
|-----------------|---------------------------------------------------------|
| expired KV keys | maintenance, up to 600,000 keys per minute              |
| finished jobs   | when they finish; failed jobs after 7 days              |
| expired blobs   | maintenance, with their files once no key uses them     |
| records         | retention, 14 days by default, whole segments at a time |
| metrics         | retention, 30 days by default                           |

SQLite reuses the pages of deleted data, so a file stops growing once the
amount of live data is stable. It doesn't shrink after a deletion. Freed pages
are not overwritten either, so a deletion is not a secure erase.

Maintenance runs in the background every minute. A Go store opened with
`Manual` runs it only when you call each engine's `Maintain`.

## Copying the directory

Don't copy the files of a store that is open: the copy may contain half of a
write. Use a [backup](../running/backups.md), or close the store first. A
blob file under `objects/` is never changed after it is stored, so those files
can be copied at any time.

## See also

- [Backups](../running/backups.md): a consistent copy of every file.
- [design/architecture.md](https://github.com/tinyshed/research/blob/main/tinystore/design/architecture.md)
  in the research repository: why each engine has its own file.
