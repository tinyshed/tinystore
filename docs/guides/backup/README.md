# Backup

A backup is the whole store as one zip: every engine's files, each copied as
one moment of itself while the store keeps working, with a manifest of each
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

- A backup holds one moment of each engine while the store keeps working
  (contract BAK-01).
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

## Reference

[Bun and Node](../../reference/bun/backup.md) ·
[Python](../../reference/python/backup.md) ·
[Go](../../reference/go/backup.md) ·
[Rust](../../reference/rust/backup.md)
