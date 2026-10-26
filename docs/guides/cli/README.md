# The command line

`tinystore` is the one command a person types. It serves a store to other
processes and machines, shows what a store holds and logs while its program
runs, backs it up, and steers the nodes of a network. The npm package and the
Python wheel install it. A Go program's author installs it with
`go install tinyshed.org/tinystore/cmd/tinystore`, which fetches the
release's binary at its first run, as a store's first open does. For Rust it
comes from the release's archive or the `ghcr.io/tinyshed/tinystore` image.

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
|         | `jobs [dir] list <queue>`            | a queue's jobs that have ids, with their state                    |
|         | `files [dir] list <set>`             | a set's files; `get <set> <path> <file>` writes one to a path     |
|         | `sql [dir] <db> "<query>"`           | a query's rows; reads only unless `--write`                       |
|         | `search list [dir]`                  | the store's search indexes; `drop <dir> <name>` drops one         |
|         | `metrics [dir] read <name>`          | a metric's samples; `names` and `aggregate` beside it             |
|         | `events [dir] list <stream>`         | a stream's events                                                 |
| Write   | `kv <dir> set <bucket> <key> <v>`    | set a key's value; `delete` deletes one                           |
|         | `jobs <dir> add <queue> <json>`      | add a job; `cancel <queue> <id>` cancels one                      |
|         | `files <dir> put <set> <path> <f>`   | write a local file; `delete <set> <path>` deletes one             |
|         | `channels <dir> publish <ch> <json>` | hand a message to whoever listens; `subscribe <ch>` listens       |
|         | `metrics <dir> ingest`               | the Prometheus text on stdin, or a stream with `-f`               |
|         | `events <dir> append <stream>`       | an event, or each line of a program's output                      |
| Run     | `serve <dir>`                        | share the store with programs here and elsewhere                  |
|         | `stop <dir>`                         | stop the server of a directory, its work finished                 |
|         | `backup <dir> <zip>`                 | the whole store in one checked zip, while it runs                 |
|         | `restore <zip> <dir>`                | a backup into an empty directory                                  |
|         | `mcp [dir]`                          | let an AI agent read the store                                    |
|         | `token new [admin\|data\|read]`      | a token line for `serve`'s `--tokens`                             |
|         | `completion <shell>`                 | the lines that give bash, zsh, fish or PowerShell Tab completion  |
| Nodes   | `node status [dir]`                  | the node's files, its links and how far behind its copies are     |
|         | `node promote <dir> [file]`          | make a copy the home, once its old home is gone                   |
|         | `node demote <dir> [file]`           | make an old home's files copies again                             |
|         | `witness <dir> --listen <host:port>` | a draft: decide which node writes a file when its home goes quiet |
| Develop | `migrate <dir> <db> <migrations>`    | apply a database's migration files                                |
|         | `schema <dir> [db]`                  | a database's tables, columns and indexes                          |

`tinystore` alone prints this list; `tinystore help <command>` or
`<command> --help` prints one command with its flags and an example, and
`tinystore help --all` prints every command's at once, for an AI agent that
learns the command line in one call.

## Tab completion

```sh
eval "$(tinystore completion bash)"                          # in ~/.bashrc
eval "$(tinystore completion zsh)"                           # in ~/.zshrc
tinystore completion fish | source                           # in ~/.config/fish/config.fish
tinystore completion powershell | Out-String | Invoke-Expression   # in $PROFILE
```

Tab then offers commands, verbs, flags and their values, and the names a
store holds, read beside it: `kv ./data get se` and Tab gives `sessions`,
`metrics ./data read ping_rtt_ms --where t` gives `target=`. When nothing
starts with what was typed, Tab offers what it is a typo of: `sesion` gives
`sessions`, `metircs` gives `metrics`. It makes no store, writes nothing and
waits for no server (contract CLI-07).

## How it behaves

- **Reading never stops the store.** `status`, `logs`, `kv`, `sql`, `backup`
  and `mcp` read a directory beside the process that serves it, without its
  lock and without writing a byte (contract CLI-01).
- **A terminal gets colour and words, a pipe gets plain lines, a script asks
  `--json`.** Colour only on a terminal, never under `NO_COLOR`.
- **A mistake names the next command.** A misspelt command or flag is told
  the nearest one, `did you mean backup?` (contract CLI-02), and exits 2; a
  failure while running exits 1.
- **A write goes through a server.** `kv set`, `metrics ingest` and every
  other write go through the server that holds the directory, or a sidecar
  the command starts, and are refused where a program holds the directory
  inside its own process (contract CLI-05).
- **A pipe may close early.** When the program reading the output stops, as
  `| head -5` does once it has its lines, the command stops too, quietly, and
  exits 0 (contract CLI-04).
- **A dangerous step refuses when unsure.** `node promote` refuses while a
  node it took its files from still serves them, unless `--force`.

## In these guides

| Page                                   | Covers                                                   |
|----------------------------------------|----------------------------------------------------------|
| [serve](serve.md)                      | local, remote with TLS and tokens, nodes, limits, logs   |
| [status and logs](status-and-logs.md)  | what a store holds, following and filtering its logs     |
| [kv, sql and schema](kv-sql-schema.md) | values read and written, rows, `--write`, migrations     |
| [jobs](jobs.md)                        | jobs added, listed and cancelled                         |
| [files](files.md)                      | files put, got, listed and deleted                       |
| [channels](channels.md)                | a message published, a channel listened to               |
| [metrics](metrics.md)                  | names, samples and aggregates, ingest and delete         |
| [events](events.md)                    | an event or a program's output appended, a stream listed |
| [Backup and restore](backup.md)        | the zip, the key, a restore that checks every byte       |
| [Nodes](nodes.md)                      | `node status`, `promote`, `demote`, the witness          |
| [AI agents](mcp.md)                    | `tinystore mcp` and its read-only tools                  |

In the Handbook, [Your first store](../../handbook/start/first-store.md#look-inside)
looks inside a store, and [Running it](../../handbook/production/running-it.md)
is the operator's page.
