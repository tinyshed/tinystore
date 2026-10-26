# Backup

A backup is the whole store as one zip: every engine's files, all copied as
one moment of the store while it keeps working, with a manifest of each
file's size and SHA-256. A restore puts it back into an empty directory,
checking every byte, or leaves nothing.

```ts
const backup = await store.backup('backups/chat-2026-10-19.zip') // { files, bytes, keyLeftOut }
```

```python
backup = await store.backup("backups/chat-2026-10-19.zip")  # Backup(files, bytes, key_left_out)
```

```go
backup, err := store.Backup(ctx, "backups/chat-2026-10-19.zip") // tinystore.Backup{Files, Bytes, KeyLeftOut}
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
  BAK-02).
- The store's encryption key and the program's own files are left out unless
  asked for, and a backup without the key says so (contract BAK-03).
- A backup that fails leaves no zip and no snapshot (contract BAK-04).

## In these guides

| Page                                    | Covers                                        |
|-----------------------------------------|-----------------------------------------------|
| [Restore](restore.md)                   | the command, Rust's `restore`, what it checks |
| [The encryption key](encryption-key.md) | `includeEncryptionKey`, a key kept apart      |

New to backups? [Keeping it](../../handbook/chat/keeping-it.md) in the
Handbook backs up every night with a schedule and restores on another
machine.

## Async Rust

In Rust, the `async` feature adds `tinystore::aio::backup`, whose backup you
await. This section shows only Rust, because Bun, Node and Python already
await every call, and in Go a call blocks its goroutine.

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

The backup runs on the store's own threads, never on your executor's
thread, and `write_to` writes `upload` on that thread. On a large store a
backup takes minutes, and every engine keeps working meanwhile.

Once its future is polled, a backup runs to its end, even if you drop the
future. It then writes its zip whole, or, if it fails, leaves no zip and no
snapshot (contract BAK-04). To know which, await it. `restore` stays a
blocking call, because it runs before a store opens in its directory.

## Reference

[Bun and Node](../../reference/bun/backup.md) ·
[Python](../../reference/python/backup.md) ·
[Go](../../reference/go/backup.md) ·
[Rust](../../reference/rust/backup.md)
