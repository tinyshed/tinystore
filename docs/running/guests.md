# A second process

In Go, a second process can open a store that another process holds, as a
guest, and change its SQL databases. Use it for a command that runs beside
your server, such as resetting a password or backing up, also with
`docker exec`. Bun and Python don't need it, because they reach a running
store through its server.

## Open a store as a guest

```go
store, err := tinystore.Open(ctx, "/data", tinystore.Options{Guest: true})
if err != nil {
	return err
}
defer store.Close(ctx)

db, err := sqldb.Open(ctx, store, "app", migrations, data.Schema)
if errors.Is(err, sqldb.ErrPending) {
	return fmt.Errorf("update and restart the server first: %w", err)
}
if err != nil {
	return err
}

err = db.Batch(ctx, func(b *sqldb.Batch) error {
	b.Exec(`update users set password_hash = ? where login = ?`, hash, login)
	b.Exec(`delete from sessions where user_id = (select id from users where login = ?)`, login)
	return nil
})
```

The server sees the new password with its next read, and the batch is one
commit: the password changes and the old sessions end together. A guest takes
no lock on the directory, so it opens while the server runs, and the two
processes share each database's writer through SQLite's own locks. A guest's
write waits up to 5 seconds for the server's, and then fails.

## What a guest can't do

| A guest can't                                    | Because                                                                           |
|--------------------------------------------------|-----------------------------------------------------------------------------------|
| open KV, jobs, records, metrics or blobs         | the server keeps their state in memory                                            |
| apply a migration                                | the server applies migrations when it starts; a newer one fails with `ErrPending` |
| create, change or drop a table, index or trigger | the server checked the schema when it opened the database                         |
| write a table named `_tinystore_…`               | jobs and KV inside a database keep their state there                              |
| open a database that isn't there                 | only the server creates one                                                       |

Each of these fails with an invalid error (`ErrInvalid`) that says what a
guest may do. Nothing runs in the background of a guest store.

## Back up beside the server

```go
store, err := tinystore.Open(ctx, "/data", tinystore.Options{Guest: true})
f, err := os.Create("backup-2026-10-04.zip")
err = backup.Write(ctx, store, f, backup.File("secret.key"))
```

A guest's backup copies every engine's file, the SQL databases, KV, jobs,
records and metrics, each from a consistent read of its own, without opening
the engines. Blobs back up only in the process that holds the store, so a
guest's backup of a store with blobs fails with an invalid error. See
[Backups](backups.md).

## See also

- [Backups](backups.md): what a backup holds and how to restore it.
- [The command line](cli.md): `tinystore` reaches a running store through its
  server.
- [sqldb/README.md](../../sqldb/README.md): the full contract of a database.
