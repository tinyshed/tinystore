# Backups

Back up the whole store, every engine and every file, into one zip while your
program keeps running. A restore checks every byte against the checksums in
the backup before it creates a single file.

## Back up a running store

```ts
await store.backup('backup-2026-10-03.zip') // the store keeps working while it copies
```

```python
await store.backup("backup-2026-10-03.zip")  # the store keeps working while it copies
```

```go
f, err := os.Create("backup-2026-10-03.zip")
if err != nil {
	return err
}
defer f.Close()

err = backup.Write(ctx, store, f) // the store keeps working while it copies
```

The backup copies each engine's database with SQLite's `VACUUM INTO`, which
doesn't block writers. Then it writes the copies into a zip, together with a
manifest of their sizes and SHA-256 checksums. Blob files go into the zip as
they are, without compression, because media doesn't compress.

Each engine's copy is a consistent moment of that engine. Two engines are
copied at two slightly different moments. That is safe, because no write
spans two engines.

In Bun and Python, the server writes the backup and sends the zip over the
connection. The SDK writes the zip next to the name you give, and renames it
when the zip is complete. If the backup fails, it leaves no zip behind. On a
remote server, only an `admin` token can ask for a backup.

In Go, `backup.Write` copies the engines that your program has opened. The
`backup` package is separate from the store, so programs that never back up
don't include `archive/zip` in their binary.

## Back up from the command line

```sh for=bun
bunx tinystore backup ./data backup-2026-10-03.zip
```

```sh for=python
uvx --from tinyshed-tinystore tinystore backup ./data backup-2026-10-03.zip
```

```sh for=go
tinystore backup ./data backup-2026-10-03.zip
```

`tinystore backup` works while your program runs. It asks the server of the
directory for the backup, and starts the sidecar if no server runs.

The backup contains every engine whose file is in the directory, even one
that no program has used since the server started. The server copies such an
SQL database without opening it. If your program brings a new migration for
that database, the migration still runs when your program opens it.

A Go program that keeps its store to itself holds the directory, so the
command can't reach the store. Share the store with
[`server.Share`](../languages.md#share-a-go-programs-store), or use
`backup.Write` in the program.

## Restore

```sh for=bun
bunx tinystore restore backup-2026-10-03.zip ./data
```

```sh for=python
uvx --from tinyshed-tinystore tinystore restore backup-2026-10-03.zip ./data
```

```go
archive, err := os.Open("backup-2026-10-03.zip")
info, err := archive.Stat()

err = backup.Restore(ctx, "./data", archive, info.Size()) // before tinystore.Open
```

A restore writes into an empty directory, before anything opens the store. It
checks the size and checksum of every file. If one byte changed, the restore
fails and leaves nothing in the directory.

## Blobs in a backup

A backup of blobs doesn't copy the files first: the snapshot hard-links each
file, which takes no time and no space, so even a store of large videos is
snapshotted instantly. The zip then streams each file. A file is never changed
after it is stored, so tools such as rsync or restic copy each file only once
across many backups.

## Snapshots

`store.Snapshot` creates the same consistent copy of every engine in a
directory inside the store's own directory, on the same disk, without a zip.
`backup.Write` is built on it.

## See also

- [Integrity](../blobs/integrity.md): what a backup repairs.
- [backup/backup.go](../../backup/backup.go): the backup package.
