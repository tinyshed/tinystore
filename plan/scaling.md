# Scaling

How TinyStore lets an application grow past one process and one machine
without rewriting it, and how the application leaves when it outgrows
TinyStore. Written on 14 October 2026 from a conversation with the owner. Past
the ladder's built steps nothing here is built or decided: the owner's calls
are under [Open](#open), and a step they accept moves into
[phases.md](phases.md) and [decisions.md](decisions.md).

## The promise

TinyStore does not become a distributed database. It lets an application go
as far as one machine and copies of it carry it, without infrastructure the
application did not ask for, and it never traps it: every step has a way out.

- Between steps the application changes an address or an option, never its
  own logic.
- A file has one writer on one machine. Scaling out is copies that read and
  stand by, and cells that each own their data; never two writers of a file.
- Each step says what it costs where it is offered: latency, the commits a
  failover can lose, how far a copy lags.

## The ladder

| Step                      | What it gives                                                                                   | What the code changes                                          | Status                                                    |
|---------------------------|-------------------------------------------------------------------------------------------------|----------------------------------------------------------------|-----------------------------------------------------------|
| 1. One folder             | the store inside the program, no server                                                         | `open('./data')`                                               | built                                                     |
| 2. A server               | many processes on one store over TCP and TLS, tokens that admit, write or only read             | `connect('tls://db:7443', { token })`                          | built                                                     |
| 3. Off the machine        | a backup in a cloud; then every commit in object storage within seconds, restored to any second | nothing: an address where the store opens or the server starts | the zip is built; the cloud and the stream are not        |
| 4. A standby              | a second server that follows the first, promoted by a command                                   | a second address in `connect`                                  | not built                                                 |
| 5. Reads near the program | replicas that read, and a copy inside the program that reads at the speed of a folder           | an option on the handles that may read a copy                  | not built                                                 |
| 6. Failover               | a lease decides the one primary, epochs fence the old one, no person needed                     | nothing                                                        | not built                                                 |
| 7. Cells                  | tenants spread over machines, a database each, moved while they work                            | the tenant's id picks its database                             | not built                                                 |
| The exit                  | the data out to Postgres, object storage, ClickHouse or Prometheus, from any step               | a command                                                      | sql's files are plain SQLite; the exporters are not built |

## What it will not be

- No consensus for data, no Raft: a lease decides who writes, and the data
  follows as pages.
- No two primaries, no merge of conflicting writes, no writes on a copy cut
  off from its primary. Writes on both sides while offline are another product
  (Turso's Sync is one) and another promise.
- No transaction across files, machines or cells: none exists across files on
  one machine today.

## One stream a file

Every step after the second stands on one mechanism: each file's commits, as
the pages they wrote, in order.

**Capture.** A file has one writer, and every commit passes through it
([architecture.md](architecture.md), the writer). After a commit the writer
reads the frames the commit appended to the WAL and hands them, with the
file's commit number, to the file's log. Litestream and LiteFS work from
outside the program, through a read transaction held open or a FUSE file
system; the writer is inside and knows each commit as it ends.

The adapter takes over checkpoints for this: `wal_autocheckpoint` off, and a
checkpoint of its own once a commit's frames are taken, so that no checkpoint
resets the WAL before its frames are read. Today SQLite checkpoints by itself,
and only `close` runs `wal_checkpoint(truncate)` (`sqlite/file.rs`).

**Pages, not operations.** A copy that applies pages is the primary's file
byte for byte, so:

- every engine and the application's own SQL replicate alike, migrations
  included, and a `now()` or `random()` in a statement changes nothing;
- what writes by itself — kv's expiry, records' packing and merges, metrics'
  packing and expiry, jobs' scheduler, `once` — runs on the primary alone and
  reaches a copy as pages;
- a copy proves by a checksum that it is the primary's file at a commit.

A small change carries a whole page. Grouped commits share pages, and the log
packs them with zstd; the research round under [Order](#order) measures what
remains.

**Positions.** Each file counts its own commits. Nothing orders two files: no
write is atomic across files on the primary, so no copy promises it either.

**The log.** The primary keeps each file's recent commits in its directory,
segments a file (`log/<file>/`, a draft), until every follower and the
archive hold them, within a bound of bytes and of age and the disk the store
keeps free. A follower that falls behind the log starts again from a snapshot.

**Integrity.** An entry carries its pages' checksums and a checksum of the
file after it, as LTX's post-apply checksum does. A follower that disagrees
stops and starts again from a snapshot rather than serve a file that is not
the primary's.

## Applying it

A follower applies a commit's pages in one write transaction through SQLite's
`sqlite_dbpage` table, so its readers see whole commits and only SQLite's
pager writes the file. SQLite 3.53.2 writes pages through it and truncates the
file by `INSERT INTO sqlite_dbpage(pgno, data) VALUES (n, NULL)`. It needs
`SQLITE_ENABLE_DBPAGE_VTAB` added to `LIBSQLITE3_FLAGS` in
`.cargo/config.toml`, and no new `unsafe`.

A follower's engines open no writer and run none of their background work: no
expiry, packing, merges, jobs, `once`, migrations or instruments of their own.
Its readers are the same `query_only` readers. What follows a file — a jobs
watch, a records tail, a config's values — goes to the primary in the first
version; later a follower wakes it after it applies a commit of that file.

A follower judges a key's time to live by its own clock, as the primary does;
the two clocks agree within the skew the engines accept already.

## The first copy

A follower applies later pages on top of its first copy, so the copy must be
the primary's file page for page at a known commit. Backup's `vacuum into`
rebuilds a file and renumbers its pages: it cannot start a follower. The
snapshot reads every page through `sqlite_dbpage` inside one read transaction
and records the commit it stands at.

Backup's orchestration carries over: each engine's moment while it keeps
working, blobs' bytes linked while removals are held, and the manifest of
sizes and SHA-256 ([api/backup.md](api/backup.md)).

## Blobs

An object in `objects/` is never written again after its commit, so it ships
before the commit of `blobs.db` that names it, and a follower fetches an
object it lacks before it applies that commit. A removal applies after the
commit that drops the object's last name. Contents small enough to live inline
in `blobs.db` travel with its pages.

## Where the stream goes

| Sink                  | What it is for                                                                               | Step |
|-----------------------|----------------------------------------------------------------------------------------------|------|
| object storage        | the archive: restore to any commit or second, segments compacted by time as Litestream's are | 3    |
| a standby server      | a copy ready to be promoted                                                                  | 4    |
| a read replica        | reads taken off the primary                                                                  | 5    |
| a copy in the program | reads without the network                                                                    | 5    |
| another cell          | a database moved to another machine                                                          | 7    |

Object storage means the S3 protocol, which AWS, Cloudflare R2, Backblaze B2,
Hetzner, Yandex Object Storage, Selectel and MinIO speak: one client reaches
them all.

## A backup in a cloud, before the stream

What a small project wants first is its zip somewhere else, in one line. It
stands on the backup that is built.

```ts
await store.backup('s3://my-bucket/app/2026-10-14.zip')                                   // a draft
const store = await open('./data', { backup: { to: 's3://my-bucket/app', every: '1h' } }) // a draft
```

```sh
tinystore backup ./data s3://my-bucket/app/2026-10-14.zip    # a draft
tinystore restore s3://my-bucket/app/2026-10-14.zip ./data   # a draft
```

- **Bun.** Bun has an S3 client built in, and the SDK needs Bun 1.4 already.
  `store.backup` gets the zip in pieces and writes a `.part` it renames when
  whole (`sdk/js/src/backup.ts`); for a cloud the pieces go to
  `s3.file(key).writer()`, a multipart upload that becomes an object only when
  it ends, as the `.part` and its rename do. Credentials come from `S3_*` or
  `AWS_*`, the endpoint from `S3_ENDPOINT`. Node has no client built in: the
  SDK hands it the zip as a stream, as Rust's `write_to` does.
- **The command line.** `crates/cli` has `hmac` and `sha2`, which sign S3's
  requests; it lacks an HTTP client and root certificates, a dependency to
  decide.
- **Keeping old backups** is the bucket's lifecycle rule; nothing in
  TinyStore.
- **Clouds without S3.** Google Drive, Dropbox and OneDrive sign in through a
  browser, each with an app its provider verifies; TinyStore does not build
  them in. rclone reaches them all after one sign-in and works today:
  `store.backup` to a file, then `rclone copyto`. Two small additions make it
  one line: `tinystore backup <dir> -`, the zip on stdout for `rclone rcat`,
  which the zip's writer allows since it never seeks; and a backup through a
  running server, which phase 4 lists already. A restore needs a file, since
  the reader seeks to the zip's central directory.
- **Encryption.** The zip is not encrypted ([api/backup.md](api/backup.md)).
  One that leaves the machine should be: TinyStore encrypts it with
  `chacha20poly1305`, a dependency already, under a name that does not repeat
  the trap the newcomer check found in `encryptionKey`; or rclone's `crypt`
  does.
- **Never sync the live directory** with Google Drive's or Dropbox's client:
  it copies SQLite's files in the middle of a write. Sync the zip.

## A standby

```sh
tinystore serve ./data --replica-of tls://a:7443    # on b, beside how a serves today; a draft
tinystore promote ./data                            # on b, once a is gone; a draft
```

- The standby starts from a snapshot, then follows each file's log; after a
  lost connection it asks for each file from the commit it applied last.
- It connects with a token of a capability of its own beside admin, data and
  read, or with an admin's.
- `promote` stops following, keeps what was applied, opens the writers,
  starts the background work and raises the epoch. A person runs it after
  making sure the old primary is gone; step 6 does it without one.

**Async by default.** The primary answers a write after its own commit, and
the standby trails: a failover loses the commits not yet shipped, counted and
logged. **Sync as an option:** a commit's completion fires once the standby
holds it, and a grouped commit pays one round trip for the whole group. While
the standby is gone a sync write waits; whether it may fall back to async with
an alarm is open.

**What a failover keeps.** A job runs at least once already
([api/jobs.md](api/jobs.md)), so a job may run again. A `once` key may run
again after an async failover: its book says so, or `once` asks for sync.
Counters flushed every second and rate limits lose what they lose to a crash
today.

## Clients

```ts
const store = await connect(['tls://a:7443', 'tls://b:7443'], { token })   // a draft: today connect takes one address
```

- The list is where to look, not whom to trust. Each server says in its
  welcome whether it is the primary or a replica, its epoch, and the primary's
  address.
- Writes go to the primary of the highest epoch the client has seen; a
  replica refuses a write with an error that names the primary. libpq's
  several hosts with `target_session_attrs=read-write` work the same way.
- Reads go to the primary unless a handle was opened to read a copy, an
  option of its builder (a draft).
- **Reading one's own writes.** A write answers its file's commit number; the
  session keeps the highest a file and sends it with a read to a copy, which
  waits until it has applied that commit, within a bound, or the read goes to
  the primary.

## A copy in the program

```ts
const store = await open('./copy', { replicaOf: 'tls://a:7443', token })   // a draft
```

A follower inside the program reads at the speed of a folder; its writes go
to the primary and are answered once the copy has applied their commit, so
the program reads what it wrote. The network stays in the SDK or the command
line: the core applies the pages it is handed and keeps no client of its own.
Turso's embedded replicas have this shape.

## Failover without a person

Two servers cannot tell a dead peer from a cut cable, so a third party decides
who writes.

- **A lease in object storage.** The primary holds a lease object and renews
  it by a conditional write (`If-Match`); a standby takes it only once it has
  expired. Litestream 0.5 holds its lease this way where LiteFS used Consul.
  Without object storage a small witness process holds the lease; without
  either, promotion stays manual.
- **Epochs fence.** Each promotion raises the epoch. A primary that cannot
  renew stops accepting writes before its lease can be taken, by a margin
  wider than the clocks' drift; a follower refuses pages of an older epoch; a
  client refuses a primary older than one it has seen.
- **The old primary comes back as a replica:** it reads the lease, drops what
  it had not shipped, counted and logged, and follows the new primary.
- The margins are chosen by measurement, not here.

## Cells

- **A tenant is a database.** `store.database(tenant)` keeps the tenant's
  tables, and `db.bucket` and `db.queue` its keys and jobs, in one file. This
  is built, and it is the advice to give users now: it makes a tenant movable
  later.
- A cell is a store on a machine. A directory maps each tenant to its cell,
  and the SDK routes by the tenant.
- **Moving a tenant** streams its database to the new cell, cuts over at a
  commit and redirects; a tenant's blob set moves by its objects and its rows.
- Nothing spans two cells in one transaction.

## The exit

| Engine  | Out to                                                  | Status                     |
|---------|---------------------------------------------------------|----------------------------|
| sql     | any SQLite tool, and Postgres through one               | the files are plain SQLite |
| kv      | JSONL or MessagePack: key, value, time to live, version | not built                  |
| jobs    | the pending jobs, drained or exported                   | not built                  |
| blobs   | the files and a manifest, or object storage             | not built                  |
| records | JSONL or OTLP                                           | not built                  |
| metrics | OpenMetrics, where its meaning holds                    | not built                  |

Records and metrics are packed with TinyStore's own codecs: without an
exporter no other tool reads them. An export is not lossless — time to live,
versions and a queue's guarantees differ from system to system — and each
exporter says what it drops.

## Order

Nothing here blocks `v0.1.0`: a copy takes the files as they are, and the
protocol gains messages without changing one.

Before `v0.1.0`:

- [ ] A research round: what taking a commit's frames costs a commit; the
  bytes a commit ships for kv, jobs, sql, records and metrics, raw and through
  zstd; applying them through `sqlite_dbpage`.
- [ ] Beside phase 4's backup: `s3://` in the Bun SDK, `tinystore backup <dir>
  -`, a guide to rclone; the command line's S3 once its HTTP client is chosen.

After `v0.1.0`, proposed:

| Phase              | Its work                                                                         | Its gate                                                                                                           |
|--------------------|----------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------|
| 7. Off the machine | the log, the archive in object storage, restore to a commit or a second          | a store restored at commit N is the primary's files at N, byte for byte                                            |
| 8. A standby       | `--replica-of`, the snapshot, `promote`, `connect` with a list, roles in welcome | a standby's files equal the primary's at each commit; a promotion loses only the unshipped commits, counted        |
| 9. Reads           | replicas, the copy in the program, commit numbers for one's own writes           | a read after a write sees it on any copy; no reader sees half a commit                                             |
| 10. Failover       | the lease, epochs, sync                                                          | no cut of a simulated network leaves two primaries that both accept writes; an old primary comes back as a replica |
| 11. Cells          | the directory, routing, moving a database                                        | a database moved while written loses no commit and serves from its new cell after the cut-over                     |
| any time           | the exporters                                                                    | each takes its engine's test data out and names what it lost                                                       |

## Research checkpoint — 15 October 2026

The [sustained WAL capture round](https://github.com/tinyshed/research/blob/main/tinystore/reports/scaling-wal-load-2026-10-15.md)
measures product seams at `31b10e4` on `experiment/wal-load`, with a separate
research crate. Its three 60-second mixed-load passes use 64 writers, eight
readers and periodic pinned snapshots under SQLite FULL. The bounded/owned
candidate retains 89.5–91.8% of uncaptured write throughput and improves
19.3–21.6% over the instrumented legacy capture. Sampled RSS is 52–54 MiB
instead of 373–384 MiB. A 180-second soak confirms 2,358,498 ACKed writes;
whole-process RSS still drifts slowly upward, so native allocation accounting
remains open.

The experiment admits a declared WAL reserve before BEGIN, refuses safely
when a reader pins the watermark, and syncs the covered feed before checkpoint.
Exceeding the declared reserve is discovered after native COMMIT and returns
OutcomeUnknown until recovery. A strict arbitrary-SQL byte cap needs an
enforceable transaction contract or VFS admission. The cache budget is per file;
the feed disk budget eventually stops writes until retention exists.

Status remains research. Owned encoding reduces codec work and batched tree
updates reduce hash work, while their separate throughput effects vary. Syncing
the feed every group costs another 16.8–26.7% in the controls without changing
native FULL. The report contains raw traces, ACK/root/integrity checks,
16 abrupt workers and the unresolved Linux SDK events CI teardown failure.
The next implementation gates are global/native budgets, atomic snapshot plus
stream position, segmented retention and a persistent sender with bounded
shared frames and explicit durable positions, followed by cascade/140-child
and WAN measurements. The ladder's product statuses above are unchanged.

## Open

1. When: the log and the archive right after `v0.1.0`, before channels and
   indexes?
2. Async by default; whether a sync write falls back when the standby is
   gone; what `once` promises after a failover.
3. Where the network lives: the command line and the SDKs, with the library
   writing the log and applying pages (proposed), or an object-storage client
   in the library behind a feature.
4. The command line's HTTP client and root certificates.
5. A backup that leaves the machine: encrypted by default, and the option's
   name.
6. The names: `--replica-of`, `replicaOf`, `promote`, the read option, the
   log's directory.
7. The log's format, TinyStore's own or LTX; a checksum of the whole file
   every commit or now and then.

## Prior art

- [Litestream](https://fly.io/blog/litestream-revamped/): the WAL to object
  storage, LTX files compacted in levels, a lease through conditional writes,
  and [read replicas through a VFS](https://litestream.io/how-it-works/vfs/).
- LiteFS: a FUSE file system and a lease in Consul.
- rqlite: Raft, and reads at a consistency the caller picks.
- [Turso](https://docs.turso.tech/features/embedded-replicas): embedded
  replicas by pages; Sync by logical changes, for writes on both sides.
- [S3's conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html),
  and [a leader elected with them](https://www.morling.dev/blog/leader-election-with-s3-conditional-writes/).
- libpq: several hosts and `target_session_attrs`.
