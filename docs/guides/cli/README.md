# The command line

`tinystore` is the one command a person types. It serves a store to other
processes and machines, shows what a store holds and logs while its program
runs, backs it up, and steers the nodes of a network. The npm package and the
Python wheel install it; for Go and Rust it comes from the release's archive
or the `ghcr.io/tinyshed/tinystore` image.

```sh
tinystore status ./data                        # what the store holds, who serves it
tinystore logs ./data -f --level warn          # the application's warnings as they arrive
tinystore kv ./data get sessions 42            # one value
tinystore sql ./data chat "select count(*) from messages"
tinystore backup ./data backups/now.zip        # the whole store, while it runs
claude mcp add tinystore -- tinystore mcp ./data   # let an AI agent read the store
```

## Every command

| Group   | Command                              | Does                                                              |
|---------|--------------------------------------|-------------------------------------------------------------------|
| Look    | `status [dir]`                       | what the store holds, who serves it, how its node stands          |
|         | `logs [dir]`                         | the application's logs; new ones as they arrive with `-f`         |
|         | `kv [dir] get <bucket> <key>`        | one value; `list <bucket> [branch]` for a bucket's keys           |
|         | `sql [dir] <db> "<query>"`           | a query's rows; reads only unless `--write`                       |
|         | `search list [dir]`                  | the store's search indexes; `drop <dir> <name>` drops one         |
| Run     | `serve <dir>`                        | share the store with programs here and elsewhere                  |
|         | `stop <dir>`                         | stop the server of a directory, its work finished                 |
|         | `backup <dir> <zip>`                 | the whole store in one checked zip, while it runs                 |
|         | `restore <zip> <dir>`                | a backup into an empty directory                                  |
|         | `mcp [dir]`                          | let an AI agent read the store                                    |
|         | `token new [admin\|data\|read]`      | a token line for `serve`'s `--tokens`                             |
| Nodes   | `node status [dir]`                  | the node's files, its links and how far behind its copies are     |
|         | `node promote <dir> [file]`          | make a copy the home, once its old home is gone                   |
|         | `node demote <dir> [file]`           | make an old home's files copies again                             |
|         | `witness <dir> --listen <host:port>` | a draft: decide which node writes a file when its home goes quiet |
| Develop | `migrate <dir> <db> <migrations>`    | apply a database's migration files                                |
|         | `schema <dir> [db]`                  | a database's tables, columns and indexes                          |

`tinystore` alone prints this list; `tinystore help <command>` or
`<command> --help` prints one command with its flags and an example.

## How it behaves

- **Reading never stops the store.** `status`, `logs`, `kv`, `sql`, `backup`
  and `mcp` read a directory beside the process that serves it, without its
  lock and without writing a byte (contract CLI-01).
- **A terminal gets colour and words, a pipe gets plain lines, a script asks
  `--json`.** Colour only on a terminal, never under `NO_COLOR`.
- **A mistake names the next command.** A misspelt command or flag is told
  the nearest one, `did you mean backup?` (contract CLI-02), and exits 2; a
  failure while running exits 1.
- **A dangerous step refuses when unsure.** `node promote` refuses while a
  node it took its files from still serves them, unless `--force`.

## In these guides

| Page                                   | Covers                                                 |
|----------------------------------------|--------------------------------------------------------|
| [serve](serve.md)                      | local, remote with TLS and tokens, nodes, limits, logs |
| [status and logs](status-and-logs.md)  | what a store holds, following and filtering its logs   |
| [kv, sql and schema](kv-sql-schema.md) | reading values and rows, `--write`, migrations         |
| [Backup and restore](backup.md)        | the zip, the key, a restore that checks every byte     |
| [Nodes](nodes.md)                      | `node status`, `promote`, `demote`, the witness        |
| [AI agents](mcp.md)                    | `tinystore mcp` and its read-only tools                |

In the Handbook, [Your first store](../../handbook/start/first-store.md#look-inside)
looks inside a store, and [Running it](../../handbook/production/running-it.md)
is the operator's page.
