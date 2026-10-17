# Scaling

How TinyStore lets an application grow past one process and one machine
without rewriting it, and how the application leaves when it outgrows
TinyStore. Written on 14 October 2026 from a conversation with the owner;
the model of nodes and homes was accepted on 17 October
([decisions.md](decisions.md), entry 20) and is built in phase 7
([phases.md](phases.md)). The calls still open are under [Open](#open).

## The promise

TinyStore does not become a distributed database that merges writes. It lets
an application go as far as copies of its files and homes spread over
machines carry it, without infrastructure the application did not ask for,
and it never traps it: every step has a way out.

- Between steps the application changes an address or an option, never its
  own logic.
- A file has one writer at a time, on one machine: its home. Scaling out is
  copies that read where they run, and files whose homes are on different
  machines; never two writers of a file.
- Each step says what it costs where it is offered: the round trip a write
  pays to its home, the commits a failover can lose, how far a copy lags.

## The ladder

| Step                   | What it gives                                                                                   | What the code changes                    | Status                                                    |
|------------------------|-------------------------------------------------------------------------------------------------|------------------------------------------|-----------------------------------------------------------|
| 1. One folder          | the store inside the program, no server                                                         | `open('./data')`                         | built                                                     |
| 2. A server            | many processes on one store over TCP and TLS, tokens that admit, write or only read             | `connect('tls://db:7443', { token })`    | built                                                     |
| 3. Copies              | a node anywhere, in a server or inside the program, holding the files it asks for, read locally | `from` and `serve` where the store opens | links over TCP; kv's and sql's writes passed on           |
| 4. Homes on many nodes | each file written on the node nearest its writers, a tenant's database in its region            | where a database's home is               | not built                                                 |
| 5. A home that moves   | a file's home moved by a command, later by itself when its node dies                            | nothing                                  | not built                                                 |
| 6. Off the machine     | a backup in a cloud; then every commit in object storage, restored to any second                | an address where the store opens         | the zip is built; the cloud and the stream are not        |
| The exit               | the data out to Postgres, object storage, ClickHouse or Prometheus, from any step               | a command                                | sql's files are plain SQLite; the exporters are not built |

## What it will not be

- No consensus for data, no Raft: one home writes a file, and the data
  follows as pages.
- No two homes of a file, no merge of conflicting writes, no writes on a copy
  cut off from its home. Writes on both sides while offline are another
  product (Turso's Sync is one) and another promise.
- No transaction across files, machines or cells: none exists across files on
  one machine today.

## Nodes

Every TinyStore is a node, and every node runs the same code. There is no
relay, standby or replica in the code: what a node is follows from which files
it holds, which of them it writes, and who takes them from it.

```ts
// a draft: the names are the owner's to settle
const store = await open('./data', {
  from: ['tls://eu:7443', 'tls://us:7443'],   // where this node takes its files, in order
  serve: 'tls://0.0.0.0:7443',                // where other nodes take them from this one
})
```

- **A file has one home**: the node that writes it, at an epoch. Every other
  node holding it holds a copy, which only readers and the applier open.
- **A node takes only the files it asks for.** A program that needs one
  database of 20 MB does not copy the network's 500 GB.
- **A node serves whatever it holds**, its own files and its copies alike,
  the bytes and positions of a copy as it received them. A copy taken from a
  copy is the same copy: twenty nodes inside programs can take their files
  from one node in their data centre rather than from a home an ocean away.
- **`from` is whom the node is connected to**, a list tried in order. Pages
  come from there and writes go there; a node passes a write for a file it
  does not write on toward that file's home.
- **The network is the core's.** The replication engine holds the links on
  the store's threads, with no async runtime, so that a store inside a Bun or
  Python program is the same node as a server. The command line watches and
  steers the links: their lag, `promote`, a move, a fresh copy.

Three jobs stay apart, because they need different guarantees:

| Job                | What it is                                           | What decides it                                                     |
|--------------------|------------------------------------------------------|---------------------------------------------------------------------|
| Copying            | each file's commits, as pages, to the nodes that ask | the file's home, which alone writes its log                         |
| Routing            | where a write for a file goes                        | the catalog: a file naming each file's home, copied like any other  |
| The right to write | which node is a file's home now                      | a person in the first version; later a lease held outside the nodes |

The catalog routes and never grants the right to write: during a cut of the
network each side holds its own copy of it, and both would name themselves.

**The catalog** (the owner, 18 October) is `catalog.db`, written by one node
a person names its home and copied by the others like any file; its home is
not the node without `from`, since in a mesh every node may write files of
its own and take from others. A new file's home is the node that makes it,
once the catalog registered it:

```ts
// a program on the European node
const db = await store.database('users').open()
```

1. Europe asks its copy of the catalog who writes `sql/users.db`: no one.
2. It claims the name for itself, a write that goes to the catalog's home.
3. The catalog's home commits the first claim of the name; Europe makes the
   file and writes it, and the other nodes read its home in their copies.

Of two nodes claiming one name in the same moment, the catalog's home keeps
one claim, and the other node copies the file from the one that won. While the
catalog's home is out of reach, the routes the catalog holds go on and no new
file is made. In the first version a person moves the catalog's home, once its
old home is surely stopped; moving a file's home takes epochs and fencing, and
later a lease or a judge, never the catalog alone.

## Trying it

What phase 7 built so far runs as below; the names are drafts, as
[Open](#open) says.

```sh
tinystore serve ./us --node-listen 0.0.0.0:7444 --node-token-file token   # America, the home
tinystore serve ./eu --from us:7444 --node-listen 0.0.0.0:7444 --node-token-file token
tinystore serve ./ru --from eu:7444 --from us:7444 --node-token-file token
```

```ts
// a program in Russia, its store inside it, copying Europe's files
const store = await open('./data', { embedded: true, node: { from: ['eu:7444'], token } })
await store.bucket('sessions').get(id)          // read here
await store.bucket('sessions').set(id, session) // committed in America, answered once here
```

- A node copies every file the node it takes from serves but its own
  `records.db` and `metrics.db`, or those `--node-files` names. With a
  catalog, `--node-catalog` on one node, a node writes each file it made once
  its claim won, and needs no list; without one, a file its home never made,
  the home makes when a copy waits for it. `--node-address` says where other
  nodes reach a node that listens on every interface; a node takes a file
  whose home its `from` does not serve straight from that home, at the
  address the catalog names.
- kv's buckets and counters, encrypted ones sealed on the copy, sql's
  statements, search's documents, jobs' adds and schedules, blobs' files,
  rate limits, quotas and `once` pass from a copy to their home; a
  transaction on a copy is refused: run it at the home.
- Links are TCP with a token, or TLS with the `replication-tls` feature:
  `--node-tls-cert` and `--node-tls-key` where a node serves, and
  `--node-tls-roots` where one takes.
- `--node-synchronous` makes the home answer a write once a node copying
  its file holds it.
- When America is gone for good: stop Europe, `tinystore promote ./eu`, start
  it without `--from`; Russia goes on from it. With a catalog, the promoted
  file's next epoch moves its route there when Europe starts. America comes back with
  `tinystore demote ./us` and `--from eu:7444`.

## An example: America, Europe, Russia

The application's backend runs in America, where it is deployed and where its
migrations run. A node in Europe copies every file so that European servers
read near them; a node in Russia copies its files from Europe.

```
America (home, migrations) ──pages──▶ Europe (copy) ──pages──▶ Russia (copy)
America ◀──────writes────── Europe ◀──────writes────── Russia
```

- **Reads and searches** are local on every node.
- **A write from Europe** goes to America, America commits it, its pages come
  back, and the call answers once Europe's copy holds them: one round trip
  across the ocean, about 100 ms. From Russia it is two hops, or one if Russia
  is told to send writes to America directly.
- **Migrations run at the home alone** and reach the copies as pages. A
  release goes to the home first; a copy whose code expects a newer schema
  waits for the migration to arrive.
- **If America dies**, Europe, the freshest copy, becomes the home; Russia
  takes its files from Europe as before. Nodes that were connected to America
  try the next address of their `from`, and Europe answers that it is the home
  now.

## Writes

A write that two machines must agree on pays the round trip between them, in
any database: light in fibre needs about 60 ms from Europe to America and
back. TinyStore makes that cost visible and puts it only where it cannot be
avoided.

| Write                                             | Where it lands                                          | What it costs                   |
|---------------------------------------------------|---------------------------------------------------------|---------------------------------|
| A read                                            | the local copy                                          | nothing                         |
| A counter's add, an appended event                | a local queue, seen at once, sent to the home           | nothing; no conflict            |
| A set                                             | the home; answered once the local copy holds its commit | the round trip to the home      |
| A decision: a unique name, the last item, a quota | the home alone                                          | the round trip to the home      |
| A tenant's or a region's own data                 | a database whose home is in that region                 | nothing for that region's nodes |

**Read your writes.** A set answers once its home committed it and the
node's own copy applied that commit, so a `get` right after it sees it. A copy
that cannot apply it within a bound answers with what it knows: the write
committed, the copy behind.

**Operations that commute go through a local queue.** Two adds of one counter
give the same total in either order, so a node writes `+1` into a queue of its
own on its disk, a file no one copies, and answers at once. `get` answers the
copy's value with what the queue still holds. The queue sends each operation
to the home with an id; the home commits the id with the effect, so that an
operation sent twice counts once; the operation leaves the queue when its id
arrives back in the pages. If the home dies meanwhile, the queue sends to the
next one.

**A set through the queue** is an option for values whose last writer may
win: it answers at once and says, where it is offered, that one of two
concurrent sets is lost.

**Decisions go to the home alone.** Selling the last item from two continents
without asking each other sells it twice.

**Where to put data.** A tenant's tables, keys, jobs and index belong in its
own database, `sql/<tenant>.db` with `db.bucket`, `db.queue` and
`db.searchIndex`, which are built: it can then have its home in its region
and move whole.

**A node's logs and metrics are its own**: records' and metrics' files stay on
the node that writes them and are copied only when asked for.

**Locks and channels** live in the memory of the process that holds the home
of kv.db; from other nodes a hold or a subscription goes there. When that home
moves, leases are lost and subscriptions are made again, as after a lost
connection today.

## When something breaks

| What breaks                 | What happens                                                                                                           |
|-----------------------------|------------------------------------------------------------------------------------------------------------------------|
| A copy dies                 | nothing else notices; it catches up from its last position when it returns, or from a fresh copy if the log moved on   |
| The link to the home is cut | reads go on from the copy, older by the minute; writes try again for a bound, then fail `unavailable`, naming the home |
| A home dies                 | its files read everywhere and take no writes until a copy becomes their home                                           |
| A home's disk is lost       | a copy becomes the home; the commits not yet shipped are lost, counted and logged, or none in the synchronous mode     |
| The home of one file dies   | files with homes elsewhere take writes as before                                                                       |

A lag and an unreachable home are said once in the log and shown by the
command line; a write never waits without a bound.

**The synchronous mode.** By default a home answers a write after its own
commit and its copies trail. As an option, a home answers only once a chosen
copy holds the commit, so that losing the home loses nothing it answered; a
grouped commit pays one round trip for the whole group.

## Changing a file's home

**Moving a home that is alive** loses nothing: the old home stops taking
writes, ships its last commits, and the new home opens its writer at the next
epoch. `replication::move_home`, called on the node that takes the file over,
does it through the catalog, the old home becoming the new one's copy once
the catalog names it.

**When a home dies**, no node can tell a dead home from a cut cable: both are
silence. A copy that guessed wrong and opened a writer while the old home
lived would make a second home, and the file would split for good.

- **In the first version a person decides**: `tinystore promote` on the
  freshest copy, once the old home is known to be stopped.
- **Later, a judge outside the nodes decides**: a lease in object storage
  renewed by a conditional write (`If-Match`), as Litestream 0.5 holds its, or
  a small witness process. A home that cannot renew stops writing before its
  lease can be taken, by a margin wider than the clocks' drift; the freshest
  copy allowed to take it does so once it expired.
- **Epochs fence.** Each change of home raises the file's epoch; a node
  refuses pages of an older epoch, and a client a home older than one it has
  seen.
- **The news travels along the links**: the new home tells the nodes
  connected to it, nodes cut off from the old one try the next address of
  their `from`, and the judge, where there is one, has the last word.
- **The old home comes back as a copy**: it drops what it had not shipped,
  counted and logged, and follows the new home before it opens any writer.
- The margins are chosen by measurement, not here.

## One stream a file

Every step after the second stands on one mechanism: each file's commits, as
the pages they wrote, in order.

**Capture.** A file has one writer, and every commit passes through it
([architecture.md](architecture.md), the writer). After a commit, while the
writer is still held, the adapter reads the frames the commit appended to the
WAL and hands them, with the file's commit number, to the file's log. The
three commits of the adapter, a grouped commit, a transaction of its own and
a begun transaction, already run channels' hooks at that point, the writer
still held. Litestream and LiteFS work from outside the program, through a
read transaction held open or a FUSE file system; the writer is inside and
knows each commit as it ends.

The adapter takes over checkpoints for this: `wal_autocheckpoint` off, and a
checkpoint of its own once a commit's frames are in the log, so that no
checkpoint resets the WAL before its frames are read. Today SQLite checkpoints
by itself, and only `close` runs `wal_checkpoint(truncate)` (`sqlite/file.rs`).
A commit whose capture fails after SQLite committed it answers
`OutcomeUnknown`, and the file takes no write until it recovers from the WAL.

**Pages, not operations.** A copy that applies pages is the home's file
page for page, so:

- every engine and the application's own SQL replicate alike, migrations
  included, and a `now()` or `random()` in a statement changes nothing;
- what writes by itself — kv's expiry, records' packing and merges, metrics'
  packing and expiry, jobs' scheduler, `once` — runs at the home alone and
  reaches a copy as pages;
- a copy proves by a checksum that it is the home's file at a commit. The
  checksum leaves out the eight bytes of page 1 that hold SQLite's change
  counters, which the applier's own commits move.

A small change carries a whole page. Grouped commits share pages, and the log
packs them with zstd.

**Positions.** Each file counts its own commits, at its epoch. Nothing orders
two files: no write is atomic across files at a home, so no copy promises it
either. An entry is named by its file, epoch, number and checksum, and names
the checksum before it, so a node applies an entry once whichever neighbour
it came from, and a conflicting entry of the same number reveals two homes.

**The log.** A home keeps each file's recent commits in its directory,
segments a file, within a bound of bytes and of age and the disk the store
keeps free. A node that falls behind the log starts again from a fresh copy;
no slow node pins it.

## Applying it

A copy applies a commit's pages in one write transaction through SQLite's
`sqlite_dbpage` table, so its readers see whole commits and only SQLite's
pager writes the file. SQLite 3.53.2 writes pages through it and truncates the
file by `INSERT INTO sqlite_dbpage(pgno, data) VALUES (n, NULL)`, all in one
statement. It needs `SQLITE_ENABLE_DBPAGE_VTAB` added to `LIBSQLITE3_FLAGS` in
`.cargo/config.toml`, and no new `unsafe`.

On a copy, a file's engines open readers and no writer, and run none of their
background work: no expiry, packing, merges, jobs, `once`, migrations or
instruments of their own. Background work and the writer follow the file, not
the store, since a node writes its own files and copies others at once. An
engine that keeps what it read of its file in memory reads it again after a
commit applies.

A copy judges a key's time to live by its own clock, as its home does; the
two clocks agree within the skew the engines accept already.

## The first copy

A copy applies later pages on top of its first copy, so that copy must be the
home's file page for page at a known commit. Backup's `vacuum into` rebuilds
a file and renumbers its pages: it cannot start a copy. The snapshot reads
pages in short read transactions between two positions taken under the
writer, then applies the commits between them, so that no long reader grows
the WAL.

Backup's orchestration carries over: each engine's moment while it keeps
working, blobs' bytes linked while removals are held, and the manifest of
sizes and SHA-256 ([api/backup.md](api/backup.md)).

## Blobs

An object in `objects/` is never written again after its commit, so it ships
before the commit of `blobs.db` that names it, and a copy fetches an object it
lacks before it applies that commit. A removal applies after the commit that
drops the object's last name. Contents small enough to live inline in
`blobs.db` travel with its pages.

## A backup in a cloud

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

Later the same `s3://` address takes every commit: object storage is a node
without a process, holding the log's segments and snapshots in the format
the links carry, restored to any commit or second.

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

## What the research measured

Research's rounds of 15 October, on a managed Linux container (kernel
6.18.44, AMD EPYC 9V74, a quota of 2 CPUs, 8 GiB), SQLite 3.53.2 with FULL
durability, against product branches `experiment/wal-capture` (`690cdf3`) and
`experiment/wal-load` (`31b10e4`), which are not merged:

- capture at the writer kept 89.5–91.8 % of the uncaptured write rate under 64
  writers and eight readers, and a bounded history kept the process at 52–54
  MiB against 373–384 MiB
  ([scaling-wal-load](https://github.com/tinyshed/research/blob/main/tinystore/reports/scaling-wal-load-2026-10-15.md));
- 33 scenarios of a process killed mid-write recovered every acknowledged
  commit
  ([scaling-wal](https://github.com/tinyshed/research/blob/main/tinystore/reports/scaling-wal-2026-10-15.md));
- a node that forwards what it received unchanged, 7 × 20 on loopback, cut
  the root's response bytes 20× and added 5 % to the network's
  ([scaling-cascade](https://github.com/tinyshed/research/blob/main/tinystore/reports/scaling-cascade-2026-10-15.md)).

None of it is a WAN measurement, a power cut or a failover time.

## Order

Phase 7 builds the first version now, before phase 4's command line and the
SDKs, which wait for the system's full bounds (the owner, 17 October):

| Slice               | Its work                                                                                          | Its gate                                                                                       |
|---------------------|---------------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------|
| 1. Capture          | the after-commit seam in the three commits, checkpoints taken over, the log in segments, recovery | every commit of every engine is in its file's log, in order, through a process killed anywhere |
| 2. Apply            | a copy's pages applied whole; engines on a copy without writer or background work                 | a copy equals its home at each commit; no reader sees half a commit                            |
| 3. The first copy   | a page-exact snapshot in short reads; blobs' objects before their commit                          | a copy started while the home is written converges on it                                       |
| 4. Links            | `from` and `serve` in the core, files chosen, copies of copies, reconnects, a fresh copy          | a node three links away holds the home's files; a cut link resumes from its position           |
| 5. Writes           | writes passed to the home, read your writes, the queue of operations that commute, `unavailable`  | a get after a set sees it on any node; an operation sent twice counts once                     |
| 6. Several homes    | the catalog, files homed on different nodes, routing, a move                                      | two nodes write their own files and copy each other's; a moved database loses no commit        |
| 7. Promote          | epochs, `tinystore promote`, an old home back as a copy, the command line's view of links         | a promotion loses only the unshipped commits, counted; an old home cannot write again          |
| 8. Synchronous mode | a home answering once a chosen copy holds the commit                                              | losing the home's directory loses no write it answered                                         |

After it: the judge and a home that moves by itself, whose gate is that no
cut of a simulated network leaves two homes taking writes; the log in object
storage with a restore to a commit or a second; the exporters.

## Open

1. The names: `from`, `serve`, `promote`, the move, the queue's set, the
   synchronous mode, the log's directory.
2. The judge: a lease in object storage, a witness process, or both.
3. TLS is built behind `replication-tls`, off by default, rustls with
   ring as `crates/cli` uses it: whether it is on by default, and an object
   storage client in the library, are open.
4. A file's identity across nodes: its name in the catalog, its home a node's
   id made once and kept in `log/node`; two homes claiming one name are
   refused, and a file two nodes wrote before their network kept a catalog is
   logged as two homes' for a person to settle.
5. Collecting nodes' logs and metrics in one place: a node copying others'
   records and metrics, and under what names.
6. What `once` promises after an asynchronous failover. A synchronous write
   whose chosen copy is gone falls back and answers, counted and said once an
   outage, as the owner left to the builder on 18 October.
7. The log's format, TinyStore's own or LTX; a checksum of the whole file
   every commit or now and then.
8. A backup that leaves the machine: encrypted by default, and the option's
   name.

## Prior art

- [Litestream](https://fly.io/blog/litestream-revamped/): the WAL to object
  storage, LTX files compacted in levels, a lease through conditional writes,
  and [read replicas through a VFS](https://litestream.io/how-it-works/vfs/).
- LiteFS: a FUSE file system, a lease in Consul, writes forwarded to the
  primary.
- rqlite: Raft, and reads at a consistency the caller picks.
- [Turso](https://docs.turso.tech/features/embedded-replicas): embedded
  replicas by pages; Sync by logical changes, for writes on both sides.
- [S3's conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html),
  and [a leader elected with them](https://www.morling.dev/blog/leader-election-with-s3-conditional-writes/).
- libpq: several hosts and `target_session_attrs`.
