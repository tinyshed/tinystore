# Restore

A restore puts a backup back into an empty directory, checking every byte
against the manifest and the zip, and leaves nothing if any byte is wrong. It
runs before a store opens there, so it is a command, and in Rust a function.

```sh
tinystore restore backups/chat-2026-10-19.zip ./data
```

```text
● ./data  restored from backups/chat-2026-10-19.zip · 3 files, 52 KiB
```

```rust
tinystore::backup::restore("backups/chat-2026-10-19.zip", "data")?; // before Store::open
let store = Store::open("data", Options::default())?;
```

A program in TypeScript, Python or Go restores through the command line: a
restore runs before a store opens, when an SDK has nothing yet to call.

```ts
await Bun.$`tinystore restore ${zip} ./data`
const store = await open('./data')
```

```python
subprocess.run(["tinystore", "restore", zip_path, "./data"], check=True)
store = await tinystore.open("./data")
```

```go
if err := exec.CommandContext(ctx, "tinystore", "restore", zip, "./data").Run(); err != nil {
	return err
}
store, err := tinystore.Open(ctx, "./data")
```

## What a restore checks

| Step                                       | If it fails                                 |
|--------------------------------------------|---------------------------------------------|
| the directory is empty or absent           | `invalid`: a restore never mixes two stores |
| no store holds the directory               | `in use`                                    |
| the manifest reads, of format `1`          | `invalid`, naming both formats              |
| each file's CRC-32, size and SHA-256 match | `corrupt`, naming the file                  |

- The restore holds the directory's `LOCK` meanwhile, so no store opens there
  before it ends.
- Each file is written beside its name, checked, synced, and only then
  named. A file that does not match fails the restore and leaves the
  directory empty (contract BAK-02).
- A restore of a backup without the encryption key says so: its encrypted
  buckets need the key put back first. See [The encryption
  key](encryption-key.md).

```text
tinystore restore: restore full.zip into ./data: the directory is not empty: a restore writes into an empty one: invalid
```

## Test your restores

A backup nobody restored is a hope. Restore last night's into a scratch
directory on a schedule, open it, and read something you know is there:

```ts
await Bun.$`tinystore restore ${zip} ${scratch}`
const restored = await open(scratch, { private: true })
const count = await (await restored.database('chat')).scalar<number>`select count(*) from messages`
if (count === 0) log.error('last night\'s backup restored empty', { zip })
await restored.close()
```

```python
subprocess.run(["tinystore", "restore", zip_path, scratch], check=True)
async with await tinystore.open(scratch, private=True) as restored:
    db = await restored.database("chat")
    if await db.scalar(int, "select count(*) from messages") == 0:
        log.error("last night's backup restored empty", zip=zip_path)
```

```go
if err := exec.CommandContext(ctx, "tinystore", "restore", zip, scratch).Run(); err != nil {
	return err
}
restored, err := tinystore.Open(ctx, scratch, tinystore.Private())
db, err := sqldb.Open(ctx, restored, "chat")
count, err := sqldb.Scalar[int64](ctx, db, `select count(*) from messages`)
```

```rust
tinystore::backup::restore(&zip, &scratch)?;
let restored = Store::open(&scratch, Options::default())?;
let count: i64 = restored.database("chat").open()?.scalar("select count(*) from messages")?;
```

The restore itself checks every byte; reading a row checks that the backup
holds what you think it does.

## What a backup holds

A backup is a plain zip, its files stored without compression, which any tool
that opens a zip opens:

```text
manifest.json         { format: 1, created, files: [{ engine, name, size, sha256 }], keyLeftOut }
kv.db                 each engine's file, one moment of it
jobs.db
sql/chat.db
blobs/blobs.db        and every file blobs keeps
app.json              a file of the program's, when included
```

Each engine's SQLite file is copied through `vacuum into` while the store
keeps working, so it holds every row and no free page. Two engines are two
moments, which is safe, since no write spans two files. A file blobs keeps is
linked into the snapshot rather than copied, so a large file costs the
snapshot a directory entry until the zip is written.

## See also

- [Backup](README.md): the engine's overview.
- [The encryption key](encryption-key.md).
- [Keeping it](../../handbook/chat/keeping-it.md) in the Handbook: a nightly
  backup and a checked restore.
