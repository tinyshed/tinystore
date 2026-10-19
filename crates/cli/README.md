# tinystore

The command of [TinyStore](https://crates.io/crates/tinystore), a small
storage runtime for applications. It serves a store's directory to Bun, Node,
Python and Go programs and to remote clients, and reads a store while its
program runs.

> **Version 0.1, the first release.** Until `1.0`, a minor release may still
> change the commands.

```sh
LIBSQLITE3_FLAGS="SQLITE_DQS=0 -USQLITE_ENABLE_MEMORY_MANAGEMENT SQLITE_ENABLE_DBPAGE_VTAB" cargo install tinystore-cli
```

The flags are the SQLite build TinyStore expects; without them readers wait
on one another. The npm and PyPI packages and each GitHub release carry the
same command already built, and `ghcr.io/tinyshed/tinystore` is its image.

```sh
tinystore status ./data                            # each engine's bytes, and who serves the directory
tinystore logs ./data -f --level warn              # the application's logs, as they arrive
tinystore serve ./data                             # the directory's sidecar, until Ctrl+C
claude mcp add tinystore -- tinystore mcp ./data   # logs, kv, jobs and SQL for an agent, read only
```

[The command line's guide](https://tinyshed.org/tinystore/docs/guides/cli) has every
command. Licensed under Apache-2.0.
