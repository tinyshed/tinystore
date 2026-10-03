# The command line

The `tinystore` command shows what a store contains, follows its logs while
your program runs, runs and stops servers, and writes migrations for Go
schemas. The Bun and Python packages install it, and Go builds it.

## Install

```sh for=bun
bunx tinystore status ./data
```

```sh for=python
uvx --from tinyshed-tinystore tinystore status ./data
```

```sh for=go
go install github.com/tinyshed/tinystore/cmd/tinystore@latest
tinystore status ./data
```

In a project that uses the Bun or Python SDK, the `tinystore` command is
already installed with the package. `bunx` and `uvx` run it without
installing anything.

## Commands

```text
tinystore v0.1.0 · kv, jobs, blobs, SQL, logs and metrics in one directory

Look
  status  [dir]       what the store holds and who serves it
  logs    [dir] -f    the application's logs, as they arrive

Run
  serve   <dir>       share the store with other processes, until Ctrl+C
  stop    [dir]       stop the server of a directory, its work finished
  mcp     [dir]       let an AI agent read the store
  backup  <dir> <zip> every engine of the store in one checked zip, while it runs
  restore <zip> <dir> a backup into an empty directory

Develop
  migrate [name]      compare a schema with its migrations
  schema  [name]      print a schema's SQL
```

The directory can come before or after the flags, or with `--dir`. Without
one, the command uses the current directory. `serve` requires a directory, so
that it never creates a store by accident. `backup` and `restore` take a
directory and a zip, in the order that the help shows.

## Follow the logs

```sh
tinystore logs ./data -f --level warn
```

```text
11:02:14.502 WARN  api  slow request  requestId=7f3a ms=1200
11:02:15.911 ERROR api  payment failed  orderId=981 reason="card declined"
```

`logs` prints the last records, oldest first, the way your logger prints them
on a terminal, and `-f` keeps printing new ones as they arrive. It reads
through the directory's server, and starts a sidecar if none runs, but only if
the directory already contains a store.

| Flag | Meaning |
|---|---|
| `-f` | follow new records |
| `-n 100` | how many of the last records to print first |
| `--level warn` | only this level and above |
| `--since 1h` | only records from the last hour |
| `--grep text` | only records that contain the text |
| `--stream api` | only one stream |
| `--json` | one JSON object per line |

For `logs` to reach the store of a running Go program, the program must share
its store with `server.Share`. See
[Share a Go program's store](../languages.md#share-a-go-programs-store).

## Status for scripts

```sh
tinystore status ./data --json
```

`status` reads the directory without opening the store, so it works next to a
running server. It never prints the server's secret.

## Back up and restore

```sh
tinystore backup ./data backup-2026-10-03.zip   # the program keeps running
tinystore restore backup-2026-10-03.zip ./data2 # into an empty directory
```

`backup` asks the server of the directory for a zip of the whole store. If no
server runs, it starts the sidecar first. `restore` writes a backup into an
empty directory. See [Backups](backups.md).

## Migrations (Go)

```sh
go tool tinystore migrate                 # what differs, for every database
go tool tinystore migrate app new add_tags # write the next migration of "app"
go tool tinystore schema app               # print the schema's SQL
```

These commands work with the schemas declared in your Go code. Add the tool to
your module with `go get -tool github.com/tinyshed/tinystore/cmd/tinystore`.
See [Schema in Go](../sql/schema.md).

## See also

- [AI agents](agents.md): `tinystore mcp`.
- [The sidecar](sidecar.md): `serve` and `stop`.
- [Backups](backups.md): `backup` and `restore`.
