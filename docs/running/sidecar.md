# The sidecar

In Bun, Node and Python, the store runs in a sidecar: a `tinystore serve`
process that the SDK starts the first time a program opens a directory. Every
process that opens the same directory shares it, and it exits on its own when
nobody uses it. This page explains how it starts, how programs find it, and how
to see and stop it.

## What happens on open

```ts
await using store = await open('./data')
```

```python
async with tinystore.open("./data") as store:
    ...
```

```text
1. read data/server/SERVE           → the sidecar's address, if one runs
2. connect and check its proof      → it really serves this directory
3. if anything fails, start one:    tinystore serve --dir ./data --local --log ./data/server/serve.log
4. connect to the new sidecar
```

If two programs start a sidecar at the same moment, only one of them gets the
directory's lock. The other exits at once, and its program connects to the
winner. Programs never trust a process id from the file, because ids are
reused. The lock on the directory decides who serves it.

The SDK checks a proof before it sends the first call. `SERVE` contains a
secret that only the directory's owner can read, and the sidecar must prove
it knows the secret. A process that takes over the address of a sidecar that
exited can't answer, so your program never sends data to it.

## Look at it

```sh
tinystore status ./data
```

```text
● ./data  served by v0.1.0 · pid 37420 · up 12m

  jobs         60.0 KiB
  kv           36.0 KiB
  records      24.0 KiB
  ──────────────────────
  total       120.0 KiB
```

`status` reads the directory without opening the store, so it works next to a
running sidecar. It shows each engine's file with its size, and who serves the
directory. `--json` prints the same as JSON for scripts.

The sidecar writes its own log, including the error it exits with, to
`data/server/serve.log`.

## Idle and stop

A sidecar exits once it has had no connections for 30 seconds. When your
program starts the sidecar, set this with `idle`, or 0 to keep it running:

```ts
await using store = await open('./data', { idle: '10m' })
```

```python
async with tinystore.open("./data", idle="10m") as store:
    ...
```

To stop a sidecar now:

```sh
tinystore stop ./data
```

`stop` lets the calls that are running finish, then stops the sidecar. The
next `open` starts a new one. You don't need `stop` after an SDK update. The
new SDK replaces an older sidecar by itself, as
[Update the SDK](../languages.md#update-the-sdk) explains.

## Run it in the foreground

```sh
tinystore serve ./data
```

```text
● serving ./data · Ctrl+C to stop
  apps find it through   data/server/SERVE
  endpoint               pipe:tinystore-015ef87b270a1914
```

`serve` with a directory runs the sidecar in your terminal until you press
Ctrl+C, with its log on the screen. Programs that open the directory connect
to it as usual. This is useful while you develop, or under a process manager
such as systemd.

## Where it listens

| Platform | Endpoint |
|---|---|
| Linux, macOS | a Unix socket, `data/server/tinystore.sock`; when that path is too long for a socket, in `$XDG_RUNTIME_DIR` or the temporary directory |
| Windows | a named pipe, `\\.\pipe\tinystore-<hash>`, that only its owner can open |

A local connection needs no token: the permissions of the directory decide
who may connect, and a local connection may do everything, including
migrations.

## Private servers

`open(dir, { private: true })` starts a server as a child of your process,
connected through its stdin and stdout. It writes nothing to `data/server/`,
and it exits with your program. Use it in tests and scripts. See
[Go, Bun and Python](../languages.md#a-private-server-for-tests-and-scripts).

## See also

- [A remote server](server.md): serve a store to other machines.
- [The wire protocol](../wire.md#finding-a-local-server): the exact discovery
  rules, for client authors.
