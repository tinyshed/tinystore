# The encryption key

A store that keeps [encrypted buckets](../kv/encrypted.md) seals their values
with its encryption key. A backup leaves the key out unless asked: a zip
without it shows nothing of the sealed values, and a zip with it opens them
all. Which one you want depends on where the backup goes.

```ts
const plain = await store.backup('backups/nightly.zip') // { files, bytes, keyLeftOut: true }
const full = await store.backup('backups/full.zip', { include: ['app.json'], includeEncryptionKey: true })
```

```python
plain = await store.backup("backups/nightly.zip")  # key_left_out=True
full = await store.backup("backups/full.zip", include=["app.json"], include_encryption_key=True)
```

```go
plain, err := store.Backup(ctx, "backups/nightly.zip") // KeyLeftOut: true
full, err := store.Backup(ctx, "backups/full.zip", tinystore.Include("app.json"), tinystore.IncludeEncryptionKey())
```

```rust
let plain = store.backup().write("backups/nightly.zip")?;
let full = store.backup().include("app.json").include_encryption_key().write("backups/full.zip")?;
```

```sh
tinystore backup ./data backups/full.zip --include app.json --include-encryption-key
```

## Which to choose

| The backup goes to                                   | Choose                                            |
|------------------------------------------------------|---------------------------------------------------|
| object storage, another team, a place you do not own | without the key; keep the key in a secret manager |
| a disk you trust as much as the store's own          | with the key, so one file restores everything     |

- A store that seals values says so of a backup without the key:
  `keyLeftOut` in the answer and in the manifest. A store that seals nothing
  answers `false`, since nothing is missing.
- The zip itself is not encrypted. What the engines keep in the clear, rows,
  events, samples, files, is in the clear in it: keep a backup as safely as the
  store's directory, or encrypt it where you keep it.
- `includeEncryptionKey` puts the key in the zip as it is. It does not
  encrypt the zip with it.
- While values are sealed, `includeEncryptionKey` fails with `io`, and writes
  no zip, when the key is not where the store reads it. That happens to
  `tinystore backup` beside a server that keeps its key apart with
  `--encryption-key-file`, since a backup beside a server reads the key from
  the store's directory alone.

## Restoring without the key

A backup without the key restores like any other, and says what is missing:

```text
● ./data  restored from backups/nightly.zip · 1 files, 52 KiB
  its encrypted buckets open with the store's key, which the backup left out: put it back first
```

Put the key back as `encryption.key` in the restored directory, or open the
store with `encryptionKeyFile` naming where you keep it. Until then, every
other bucket, table and file reads; an encrypted bucket's reads fail and say
the key is missing.

> [!CAUTION]
> **A lost key loses the values**
> Nothing recovers a sealed value without its key. Keep a copy of the key
> apart from the backups that leave it out, and test a restore with it.

## The program's own files

`include` takes a file of the program's by its path inside the store's
directory, `/` between its parts, and a restore puts it back where it was. A
path outside the directory, or one of the store's own files, an engine's
file, `LOCK`, the key, is `invalid`; the key goes only through
`includeEncryptionKey`.

```text
backup: "encryption.key": the store's encryption key, which include_encryption_key() takes: invalid
```

## See also

- [Backup](README.md): the engine's overview.
- [Encrypted buckets](../kv/encrypted.md): what the key seals.
- [Restore](restore.md).
