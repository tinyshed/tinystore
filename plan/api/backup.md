# backup: the API book

A backup of a whole store while its program runs, as one zip, and its
restore into an empty directory before a store opens there. This book is the
API as it is built; [dx.md](../dx.md) holds the rules it follows, and the Go
release candidates at `e81a050` its promises, `backup/` and
`docs/running/backups.md`.

## What a newcomer learns

| Word                    | What it is                                                                     |
|-------------------------|--------------------------------------------------------------------------------|
| a backup                | one zip of every engine's files, each a moment of its engine, and a manifest   |
| the manifest            | `manifest.json` in the zip: its format, its time, each file's size and SHA-256 |
| a file of the program's | a file inside the store's directory that a backup takes only when named        |
| the encryption key      | the store's `encryption.key`, which a backup takes only when asked             |
| a restore               | the zip put back into an empty directory, every byte checked, or nothing left  |

## Back up

```rust
let backup = store.backup().write("backup-2026-10-13.zip")?;    // the store keeps working meanwhile
store.backup().include("secret.key").include_encryption_key().write("full.zip")?;
store.backup().write_to(&mut upload)?;                           // to any writer, as it is made
```

```ts
const backup = await store.backup('backup-2026-10-13.zip')
await store.backup('full.zip', { include: ['secret.key'], includeEncryptionKey: true })
```

```python
backup = await store.backup("backup-2026-10-13.zip")    # Backup(files, bytes, key_left_out)
await store.backup("full.zip", include=["secret.key"], include_encryption_key=True)
```

```go
backup, err := store.Backup(ctx, "backup-2026-10-13.zip") // tinystore.Backup{Files, Bytes, KeyLeftOut}
_, err = store.Backup(ctx, "full.zip", tinystore.Include("secret.key"), tinystore.IncludeEncryptionKey())
```

```sh
tinystore backup ./data backup-2026-10-13.zip --include secret.key
```

`write` and `store.backup` answer what the backup holds:
`{ files: 7, bytes: 1834221, keyLeftOut: false }`.

- Each engine copies its own files as one moment of it while its writer and
  readers keep going: an SQLite file through `vacuum into` on a connection
  of its own, which holds every row and no free page. kv writes the counters
  and renewals it holds in memory first, records its loggers' lines and
  metrics its instruments' totals, so that the copy has them.
- Two engines are two moments, which is safe: no write spans two files.
- blobs copies `blobs.db` and links beside the copy every file it names: a
  link copies no byte and keeps the bytes after the engine lets its own name
  go, so a film costs the snapshot a directory entry. No file is removed
  meanwhile. A system that links nothing gets a copy.
- An engine no handle opened is copied from its files as they lie: an SQLite
  file through SQLite's own snapshot, and `blobs/` linked whole but its
  uploads. One that opens while the backup runs is copied whole all the same.
- The copies are made in `.snapshot-…` of the store's directory, on the disk
  the data is on, and removed once the zip is written.
- `write` writes the zip beside its name and renames it once synced, so that
  a backup that fails leaves no zip; `write_to` leaves what it wrote of one
  that fails to the program.
- Each file is stored as it is, without compression, and every tool that
  opens a zip opens it; ZIP64's fields where a file passes 4 GiB.

## Files of the program's, and the key

- A backup holds the engines' files and nothing else unless told: `include`
  takes a file of the program's, by its path inside the store's directory,
  `/` between its parts, which a restore puts back where it was.
- A name outside the store's directory, spelled otherwise than as a path in
  it, or of the store's own, an engine's file, `LOCK`, `server/` or the
  manifest's, is `invalid`.
- The zip is not encrypted: what the engines keep in the clear, rows,
  events, samples and files, is in the clear in it, and a backup is kept as
  safely as the store's directory, or encrypted where it is kept.
- The store's encryption key is left out unless `include_encryption_key`
  includes it, as it is: a backup without it shows nothing of the values it
  seals, and one with it opens them all, so it is kept as safely as the key.
  A store that seals values says so of a backup without the key:
  `key_left_out`, and the manifest's `keyLeftOut`. A restored store opens
  such a bucket with the key alone, and says it is missing. `include`
  refuses the key's name, which only `include_encryption_key` takes.
- The key is the file the store's options name, or `encryption.key` of its
  directory; a backup that takes it names it `encryption.key`, where a store
  restored without options reads it.

## Restore

```rust
tinystore::backup::restore("backup-2026-10-13.zip", "data")?;   // before Store::open
```

```sh
tinystore restore backup-2026-10-13.zip ./data
```

An SDK's program restores through the command line: a restore runs before a
store opens, when an SDK has nothing yet to call.

- A restore writes into a directory that is empty or absent, holding its
  `LOCK` meanwhile, so that no store opens there before it ends.
- Each file is written beside its name, checked against its manifest's size
  and SHA-256 and its zip's CRC-32, synced, and only then named. A file that
  does not match fails the restore `corrupt`, and leaves the directory
  empty.
- A restore reads the backups of its format, `1`; a later format is
  `invalid`, naming both.

## Errors

| Kind      | When                                                                                                              |
|-----------|-------------------------------------------------------------------------------------------------------------------|
| `invalid` | a file named that is no path inside the store, or the store's own; a restore into a directory that holds anything |
| `corrupt` | a zip whose manifest, CRC-32, size or SHA-256 does not match what it holds                                        |
| `closed`  | a backup of a store that closed                                                                                   |
| `in use`  | a restore into a directory a store holds                                                                          |
| `io`      | a file named that is not there, a disk that refused                                                               |

## Bounds

|                            |              |
|----------------------------|--------------|
| A manifest a restore reads | 64 MiB       |
| A file's name              | 65,535 bytes |

## Through a server

- `backup.write` is a download of the zip as the server writes it, a piece
  of 256 KiB at most a DATA within the client's credit, then what it holds,
  for a connection that may administer: a backup holds every byte of the
  store, the key's too when asked. The backup writes on one of the session's
  workers and waits while four pieces wait for the client; a client that
  cancels or leaves ends it, and its snapshot goes.
- The SDK writes the zip beside its name and renames it once it is whole; a
  connection lost on the way makes the backup again from its start.
- `tinystore backup <dir> <zip>` backs up a directory no server holds, and
  refuses one a program or a server holds, saying that its backup goes
  through it; through a running server it comes with the command line's
  client, as `status` and `logs` do. `tinystore restore <zip> <dir>` needs no
  server.

## Slices

1. The Rust core: each engine's snapshot, the store's of every engine, the
   zip and its manifest, `store.backup()`, `restore`, the key and the files
   of the program's, GATES' backup lines in Rust.
2. The wire, the Bun SDK's `store.backup` and the command line's `backup`
   and `restore`.

Python and Go come with their bindings.

## Later

- Deflate or zstd inside the zip: `vacuum into`'s copies compress, and a
  dependency or a method fewer tools read is the owner's choice.
- Storing what repeats once across backups, as restic and borg do: research
  has the round on blobs' content-defined chunks.
- A recovery tool's `openBeside`: a store opened read-only beside a damaged
  one.
- A backup of a directory a running server holds from the command line, with
  its client.

## Newcomer check, 13 October

Three Haiku agents that had read nothing about TinyStore read the call sites
of the first draft at the highest effort: a newcomer's reading of twelve bare
calls, a free review, and a blind choice between spellings.

- All three read `encryption_key()` and `encryptionKey: true` as "encrypt the
  zip with the key", where it puts the key in the zip as it is: a backup
  that holds its own key would pass for a protected one. It is
  `include_encryption_key()`, `includeEncryptionKey` and
  `--include-encryption-key`, and the book says the zip is not encrypted.
- Two found `files` read two ways, the files a backup takes and the count it
  answers, `files: 7`, and `file` is blobs' word for a stored file. It is
  `include`, in Rust, TypeScript and the command line, beside
  `include_encryption_key`.
- Left for the owner, each named by one reader: `key_left_out` as
  `sealed_without_key`, or a backup that refuses to leave a key out without
  being told; `write_to` as `stream_to`; the answer's `files` and `bytes` as
  `entries` and `zipBytes`; a `store.setLogLevel('debug')` beside the config
  `log`, which one reader preferred to declaring the config, and the name
  `log`, which a program's own config of that name shares, each declaration
  answering for its own settings; `verify`, a backup's progress and its
  cancel from TypeScript.
- What they read right: `store.backup().write(path)` as a builder whose
  defaults are safe, `restore` as a function since no store is open yet, and
  a restore that writes into an empty directory only.

## Was, in Go

| Go at `e81a050`                                  | Now                                        |
|--------------------------------------------------|--------------------------------------------|
| `backup.Write(ctx, store, w, backup.File(name))` | `store.backup().include(name).write_to(w)` |
| `backup.Restore(ctx, dir, archive, size)`        | `backup::restore(zip, dir)`                |
| `store.Snapshot(ctx)` public                     | the snapshot is the backup's own           |
| database files deflated, blobs stored            | every file stored                          |
| a guest's backup beside the server               | not yet: guests are not built              |
