# The command line

`tinystore` is the one command a person types. It serves a store to other
processes and machines, shows what a store holds and logs while its program
runs, backs it up, and steers the nodes of a network. The npm package, the
Python wheel and Go's `go install` give it, as
[Install](../../handbook/start/first-store.md#install) shows. For Rust,
`cargo install tinystore-cli` builds it, or take it from the release's archive
or the `ghcr.io/tinyshed/tinystore` image.

```sh
tinystore status ./data                        # what the store holds, who serves it
tinystore logs ./data -f --level warn          # the application's warnings as they arrive
tinystore kv ./data get sessions 42            # one value
tinystore sql ./data chat "select count(*) from messages"
tinystore backup ./data backups/now.zip        # the whole store, while it runs
claude mcp add tinystore -- tinystore mcp ./data   # let an AI agent read the store
```

## Read a command

Type a command and see what each word of it is, what it does and what a call
like it prints. Every word's meaning comes from the command line's own help.

```explain
tinystore kv ./data set flags beta on --ttl 1h
```

## Every command

| Group   | Command                               | Does                                                                     |
|---------|---------------------------------------|--------------------------------------------------------------------------|
| Look    | `status [dir]`                        | what the store holds, who serves it; `--names` counts each bucket's keys |
|         | `logs [dir]`                          | the application's logs; new ones as they arrive with `-f`                |
|         | `kv [dir] get <bucket> <key>`         | one value; `list <bucket> [branch]` for a bucket's keys                  |
|         | `jobs [dir] list <queue>`             | a queue's jobs, with their state                                         |
|         | `files [dir] list <set>`              | a set's files; `get <set> <path> <file>` writes one to a path            |
|         | `sql [dir] <db> "<query>"`            | a query's rows; reads only unless `--write`                              |
|         | `search list [dir]`                   | the store's search indexes; `drop <dir> <name>` drops one                |
|         | `metrics [dir] read <name>`           | a metric's samples; `names`, `aggregate` and `export` beside it          |
|         | `events [dir] list <stream>`          | a stream's events; `export <stream>` every one of a range, JSON lines    |
| Write   | `kv <dir> set <bucket> <key> <v>`     | set a key's value; `delete` deletes one                                  |
|         | `jobs <dir> add <queue> <json>`       | add a job; `cancel <queue> <id>` cancels one                             |
|         | `files <dir> put <set> <path> <f>`    | write a local file; `delete <set> <path>` deletes one                    |
|         | `config <dir> update <cfg> <p=v>`     | change a config's live settings; `reset`, and `explain` beside           |
|         | `channels <dir> publish <ch> <json>`  | hand a message to whoever listens; `subscribe <ch>` listens              |
|         | `metrics <dir> ingest`                | Prometheus text on stdin, a stream with `-f`; `replace` a range          |
|         | `events <dir> append <stream>`        | an event, or each line of a program's output                             |
| Run     | `serve <dir>`                         | share the store with programs here and elsewhere                         |
|         | `stop <dir>`                          | stop the server of a directory, its work finished                        |
|         | `backup <dir> <zip>`                  | the whole store in one checked zip, while it runs                        |
|         | `restore <zip> <dir>`                 | a backup into an empty directory                                         |
|         | `verify <zip>`                        | check a backup as a restore would, writing nothing                       |
|         | `mcp [dir]`                           | let an AI agent read the store, and write what `--write` names           |
|         | `token new [admin\|data\|read]`       | a token line for `serve`'s `--tokens`                                    |
|         | `lock <dir> hold <lock> <key> -- <c>` | run a command while it holds a key of a lock                             |
|         | `completion <shell>`                  | the lines that give bash, zsh, fish or PowerShell Tab completion         |
| Nodes   | `node status [dir]`                   | the node's files, its links and how far behind its copies are            |
|         | `node promote <dir> [file]`           | make a copy the home, once its old home is gone                          |
|         | `node demote <dir> [file]`            | make an old home's files copies again                                    |
|         | `witness <dir> --listen <host:port>`  | a draft: decide which node writes a file when its home goes quiet        |
| Develop | `migrate <dir> <db> <migrations>`     | apply a database's migration files                                       |
|         | `schema <dir> [db]`                   | a database's tables, columns and indexes                                 |

`tinystore` alone prints this list; `tinystore help <command>` or
`<command> --help` prints one command with its flags and an example, and
`tinystore help --all` prints every command's at once, for an AI agent that
learns the command line in one call, and `help --all --json` gives the same as
data: each command, its flags and examples, and what each word it takes
stands for.

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
  `--json`.** Colour follows the rule every logger keeps: on a terminal, or
  where `FORCE_COLOR` asks, never under `NO_COLOR` or `TERM=dumb`
  ([colours](../records/console.md#colours)).
- **A mistake names the next command.** A misspelt command or flag is told
  the nearest one, `did you mean backup?` (contract CLI-02), and exits 2; a
  failure while running exits 1.
- **A pipe may close early.** When the program reading the output stops, as
  `| head -5` does once it has its lines, the command stops too, quietly, and
  exits 0 (contract CLI-04).
- **A dangerous step refuses when unsure.** `node promote` refuses while a
  node it took its files from still serves them, unless `--force`.

## Who writes

`kv set`, `metrics ingest` and the other commands of the Write group go
through the server that holds the directory, a sidecar the command starts, or
a program that opened its store with `cli` (contract CLI-05):

| Who holds `./data`                             | What a write does                                                           |
|------------------------------------------------|-----------------------------------------------------------------------------|
| `tinystore serve` or an SDK's sidecar          | writes through it                                                           |
| nothing                                        | starts a sidecar, which stops 30 seconds after its last client, and says so |
| a program that opened it `embedded` with `cli` | writes through that program                                                 |
| a program that opened it `embedded` without it | refuses and exits 1                                                         |

```text
tinystore kv set: started a server for ./data; it stops 30 s after the last client
```

Several commands can write at once: the first one starts the sidecar, and the
others write through it. A program that opened the store `embedded` keeps it
inside its own process, so no other process can write to it:

```text
tinystore kv set: a program holds ./data inside its own process, so no other process can write to it
  open it in that program with cli or a sidecar, or let `tinystore serve ./data` hold it
```

### Writing to a store a program holds

`cli` lets the `tinystore` command line write to an embedded store while the
program runs, for an operator's one-off writes: run a job now, forget a
cached value. The store listens where a sidecar would, on a socket, or on
Windows a named pipe, that only the user the program runs as may reach
(contract SEC-07), and names it in `server/SERVE`:

```ts
const store = await open('./data', { embedded: true, cli: true })
```

```python
store = await tinystore.open("./data", embedded=True, cli=True)
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.Embedded(), tinystore.CLI())
```

```rust
// with the library's `local` feature
let store = Store::open("./data", Options { cli: true, ..Options::default() })?;
```

```sh
# while the program runs
tinystore jobs ./data add teacher-reviews '{"teacher":4353}'
tinystore status ./data
● ./data  held by a program · pid 4242 · the command line may write · durability os
```

- **Every write command works**, and so does `tinystore mcp --write`: each
  finds the program through `SERVE` and checks its proof, as it checks a
  server's.
- **The program decides when it stops.** `tinystore stop ./data` refuses:
  "a program holds ./data embedded, pid 4242; it stops when that program
  closes it".
- **A close lets a command finish.** The store tells each command `GOAWAY`,
  waits up to 2 seconds for its calls, removes `SERVE`, and only then lets
  go of the directory (contract STORE-13). A command cut off hears what it
  hears when a server goes: a write by an id goes again, one without is
  `outcome unknown`.
- **Four commands at a time.** A fifth is told to try again.
- **An SDK does not attach.** A program that opens `./data` through a
  sidecar while another holds it with `cli` gets `InUseError` at once, naming
  the program's pid.

A directory without a store is refused, and nothing is created in it. Run the
program or `tinystore serve` there once to make the store.

Some commands open the store themselves instead of reaching a server.
`sql --write` does so when no program holds the directory, and writes through
the server when one does. `migrate`, `search drop`, `node promote` and
`node demote` need a directory that no program or server holds.

| What                                  | Value                                 |
|---------------------------------------|---------------------------------------|
| a sidecar's idle time                 | 30 seconds after its last client      |
| the wait for a server                 | 15 seconds to start, 10 for an answer |
| commands a `cli` store serves at once | 4                                     |
| what a `cli` store's close waits for  | 2 seconds for the calls under way     |

## Pages

A command that lists, such as `jobs list`, `files list`, `events list` or
`metrics read`, prints one page. `--limit` sets how many a page holds, where
the command takes it. When more follow, stderr says which `--after` value
reads the next page. With `--json`, the last line is `{"next": "…"}`, whose
value `--after` takes:

```sh
next=$(tinystore metrics ./data read ping_rtt_ms --since 30d --json | tail -1 | jq -r .next)
tinystore metrics ./data read ping_rtt_ms --since 30d --json --after "$next"
```

`events export` and `metrics export` walk every page themselves.

## In these guides

| Page                                   | Covers                                                      |
|----------------------------------------|-------------------------------------------------------------|
| [serve](serve.md)                      | local, remote with TLS and tokens, nodes, limits, logs      |
| [status and logs](status-and-logs.md)  | what a store holds, following and filtering its logs        |
| [kv, sql and schema](kv-sql-schema.md) | values read and written, rows, `--write`, migrations        |
| [jobs](jobs.md)                        | jobs added, listed and cancelled                            |
| [files](files.md)                      | files put, got, listed and deleted                          |
| [channels](channels.md)                | a message published, a channel listened to                  |
| [lock](lock.md)                        | a command run while it holds a key                          |
| [config](config.md)                    | a config explained, its live settings changed               |
| [metrics](metrics.md)                  | names, samples, aggregates, export, ingest, replace, delete |
| [events](events.md)                    | an event or a program's output appended, listed, exported   |
| [Backup and restore](backup.md)        | the zip, the key, a restore that checks every byte          |
| [Nodes](nodes.md)                      | `node status`, `promote`, `demote`, the witness             |
| [AI agents](mcp.md)                    | `tinystore mcp`, its tools that read and those that write   |
| [Your own MCP server](mcp-server.md)   | a Rust program's own tools, marks, `--write`, store tools   |

In the Handbook, [Your first store](../../handbook/start/first-store.md#look-inside)
looks inside a store, and [Running it](../../handbook/production/running-it.md)
is the operator's page.
