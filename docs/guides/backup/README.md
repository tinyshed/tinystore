# Backup

A backup is the whole store as one zip: every engine's files, all copied as
one moment of the store while it keeps working, with a manifest of each
file's size and SHA-256. A restore puts it back into an empty directory,
checking every byte, or leaves nothing.

```ts
const backup = await store.backup('backups/chat-2026-10-19.zip') // { files, bytes, keyLeftOut, excluded }
```

```python
backup = await store.backup("backups/chat-2026-10-19.zip")  # Backup(files, bytes, key_left_out, excluded)
```

```go
backup, err := store.Backup(ctx, "backups/chat-2026-10-19.zip") // tinystore.Backup{Files, Bytes, KeyLeftOut, Excluded}
```

```rust
let backup = store.backup().write("backups/chat-2026-10-19.zip")?;
```

```sh
tinystore backup ./data backups/chat-2026-10-19.zip
tinystore restore backups/chat-2026-10-19.zip ./restored
```

## What it guarantees

- A backup holds one moment of the whole store, every engine's file at the
  same instant, while the store keeps working (contract BAK-01). A row that
  names what another file holds, such as a cursor saying how far a series'
  samples go, is restored with what it names.
- A restore checks every byte against the manifest and the zip, writes only
  into an empty directory, and leaves nothing when it fails (contract
  BAK-02). If its process dies before it ends, the directory does not open:
  remove it and restore again.
- The store's encryption key and the program's own files are left out unless
  asked for, and a backup without the key says so (contract BAK-03).
- A database opened with `backup: false` is in no backup, its file nor a
  byte of its files, and the backup names it in `excluded` (contract
  BAK-05). See [A database left out of backups](#a-database-left-out-of-backups).
- A backup that fails leaves no zip and no snapshot (contract BAK-04). If
  its process dies instead, its snapshot, a copy of the store's files in a
  `.snapshot-…` folder of the store's directory, stays until the store next
  opens there, which removes it. The zip it was writing stays too, beside
  the zip's name with `.part` at its end.

## A database left out of backups

Some files live minutes: an upload waiting for its owner to confirm it, a
one-time link. Open their database with `backup: false`, and no backup holds
them:

```ts
const uploads = await store.database('uploads', { backup: false })
const pending = uploads.files('pending', { ttl: '15m' })
```

```python
uploads = await store.database("uploads", backup=False)
pending = uploads.files("pending", ttl="15m")
```

```go
uploads, err := sqldb.Open(ctx, store, "uploads", sqldb.Backup(false))
pending, err := blobs.OpenFiles(uploads, "pending", blobs.TTL(15*time.Minute))
```

```rust
let uploads = store.database("uploads").backup(false).open()?;
let pending = uploads.files("pending").ttl(Duration::from_secs(15 * 60)).open()?;
```

Its file and its files' bytes lie in `local/uploads.db` and
`local/uploads.blobs/` instead of `sql/`. Where they lie is the whole rule:
a backup takes `sql/` and the engines' own files, so `local/` stays out
whether the database is open or not, from this process or from `tinystore
backup` in another, and an older release's backup leaves it out too. The
backup's answer names it:

```ts
await store.backup('backups/nightly.zip') // { files: 8, bytes: 412_380, keyLeftOut: false, excluded: ['uploads'] }
```

A restore of that zip has no `uploads`; opening it makes it empty. Buckets
and queues opened from such a database are left out with it, and an
encrypted bucket in it still needs the store's key, as any other does.

**It is not copied either.** On a network of nodes, such a database is each
node's own, as its own records and metrics are: no node copies it, and each
node that opens it writes its own. A file uploaded on one node is not on
another, and a node that dies takes it along.

## Through the protocol

A backup that TypeScript, Python or Go asks for, of the core in its own
process or of a server, comes back as the zip in pieces, each sent once the
SDK has room for it. A
client that reads slowly but steadily, as one writing to a slow disk, gets
the whole zip however long it takes. A client that takes nothing for a
minute gets no zip: the backup fails with `cancelled`, saying its reader
took nothing for 60s, and the server removes the backup's snapshot and goes
on with its other work.

## In these guides

| Page                                    | Covers                                        |
|-----------------------------------------|-----------------------------------------------|
| [Restore](restore.md)                   | `restore`, `verify`, Rust's too, their checks |
| [The encryption key](encryption-key.md) | `includeEncryptionKey`, a key kept apart      |

New to backups? [Keeping it](../../handbook/chat/keeping-it.md) in the
Handbook backs up every night with a schedule and restores on another
machine.

## Async Rust

In Rust, the `async` feature adds `tinystore::aio::backup`, whose backup you
await. See [Rust with async](../../handbook/start/rust-async.md).

```rust
use tinystore::Options;
use tinystore::aio::Store;

let store = Store::open("data", Options::default())?;
let backup = store.backup().include("secret.key").write("backups/chat-2026-10-19.zip").await?;
let sent = store.backup().write_to(upload).await?; // upload: any Write + Send + 'static
```

| Blocking Rust                                                 | Async Rust                                           |
|---------------------------------------------------------------|------------------------------------------------------|
| `store.backup().write("backups/chat.zip")?`                   | `store.backup().write("backups/chat.zip").await?`    |
| `store.backup().write_to(upload)?`                            | `store.backup().write_to(upload).await?`             |
| `tinystore::backup::restore("backups/chat.zip", "restored")?` | the same blocking call: it runs before a store opens |
| `tinystore::backup::verify("backups/chat.zip")?`              | the same blocking call: it reads a zip, not a store  |

The backup runs on the store's own threads, and `write_to` writes `upload` on
that thread. On a large store a backup takes minutes, and every engine keeps
working meanwhile. Once polled, a backup runs to its end even if you drop its
future, so await it to know whether it wrote its zip.

## Reference

[Bun and Node](../../reference/bun/backup.md) ·
[Python](../../reference/python/backup.md) ·
[Go](../../reference/go/backup.md) ·
[Rust](../../reference/rust/backup.md)
