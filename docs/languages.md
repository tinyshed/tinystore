# Go, Bun and Python

TinyStore has the same engines and the same operations in Go, Bun, Node and
Python. Go runs the engines inside your program. Bun, Node and Python connect
to a `tinystore serve` process, which the SDK starts for you. This page shows
the ways to connect to a store and what is different in each language.

## Connect to a store

```ts
import { connect, open } from '@tinyshed/tinystore'

// The directory's shared sidecar, started if none is running.
await using store = await open('./data')

// A private server that lives and dies with this process.
await using scratch = await open('./scratch', { private: true })

// A server on another machine.
await using remote = await connect('tls://db.internal:7443', { token: process.env.TINYSTORE_TOKEN! })
```

```python
import os

import tinystore

# The directory's shared sidecar, started if none is running.
async with tinystore.open("./data") as store:
    ...

# A private server that lives and dies with this process.
async with tinystore.open("./scratch", private=True) as scratch:
    ...

# A server on another machine.
async with tinystore.connect("tls://db.internal:7443", token=os.environ["TINYSTORE_TOKEN"]) as remote:
    ...
```

```go
// The store runs inside this program.
store, err := tinystore.Open(ctx, "./data", tinystore.Options{})
if err != nil {
	return err
}
defer store.Close(ctx)
```

| | Go | Bun, Node and Python |
|---|---|---|
| Where the engines run | inside your program | in `tinystore serve`, a separate process |
| A call | a function call | a round trip over a local socket, or TLS for a remote server |
| Writes that commit together | `Batch` and `Tx` | `batch` and `tx`, which the server commits as one transaction |
| The binary | not needed | included in the SDK package |

The SDKs have no transaction that stays open across calls, because a client
that stops responding would block every writer of the file. Instead, a batch
sends all of its writes at once, and the server commits them together. A KV
`tx` can read before it writes. It sends its writes in one batch, which first
checks that nothing it read has changed. If something changed, `tx` runs your
function again. See [Transactions](kv/transactions.md).

## The shared sidecar

```text
web worker 1 ──┐
web worker 2 ──┼──► tinystore serve ./data ──► data/kv.db, data/jobs.db, …
cron script  ──┘
```

The first `open` of a directory starts `tinystore serve` in the background,
and every later `open` connects to it. The sidecar writes its address to
`data/server/SERVE`, which only the owner of the directory can read. Before
the first call, the SDK checks that the server can prove it read this file.
Another process can't pretend to be the sidecar, even if it takes over the
sidecar's socket after it exits.

The sidecar exits after 30 seconds without connections. When your process
starts the sidecar, you can change this time with the `idle` option of `open`,
or set it to 0 to keep the sidecar running. The sidecar writes its own log to
`data/server/serve.log`.

```sh
tinystore status ./data   # who serves the directory, and how much each engine stores
tinystore stop ./data     # stop the sidecar once its running calls finish
```

## A private server for tests and scripts

```ts
await using store = await open(dir, { private: true })
```

```python
async with tinystore.open(directory, private=True) as store:
    ...
```

```go
store, err := tinystore.Open(ctx, t.TempDir(), tinystore.Options{})
```

With `private`, the SDK starts `tinystore serve` as a child process and talks
to it over its stdin and stdout. There is no socket, and nothing is written to
`data/server/`. The server exits when your program closes the store or exits.
Give each test its own directory, and tests can run in parallel.

Go needs no server for tests. Open a store in a temporary directory instead.

## A server on another machine

Start a server with TLS and a file of tokens:

```sh
tinystore serve /srv/data --listen tls://0.0.0.0:7443 \
  --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt
```

```text
admin 4dGQ6tR2kq0SxVZpLWn8E1yHf7cJmB3aUoN9Tz5Ki0E
data  Yq8wHn2xLz5Rb7Tc0Vm3Kj6Fd9Gs1Ap4Ue8Wo2Ni5Qt
```

Each line of `tokens.txt` gives a token its capability. An `admin` token can do
everything, including running migrations. A `data` token can read and write
data, but it can't change a schema. A token is 32 random bytes in base64url
without padding. Generate one with:

```sh for=bun
bun -e "console.log(require('node:crypto').randomBytes(32).toString('base64url'))"
```

```sh for=python
python -c "import secrets; print(secrets.token_urlsafe(32))"
```

```sh for=go
openssl rand -base64 32 | tr '+/' '-_' | tr -d '='
```

Clients connect with `connect` and a token, as shown at the top of this page.
With `tls://`, the client checks the server's certificate before it sends the
token. A remote server limits the memory its engines use to 1 GiB, unless you
set `--memory`.

## Share a Go program's store

A Go program that opens a store holds the directory's lock, so no sidecar can
start for it. To let other processes use the store while your program runs,
share it from your program:

```go
stop, err := server.Share(ctx, store, server.Options{KV: state, Jobs: queues})
if err != nil {
	return err
}
defer stop() // before store.Close
```

`server.Share` comes from the `github.com/tinyshed/tinystore/server` module.
It makes the store reachable exactly like a sidecar. A Python script, a Bun
script, the `tinystore` command and an AI agent over MCP all find it in
`data/server/SERVE`. `stop` removes `SERVE`, gives running calls 10 seconds to
finish, and closes the connections.

Pass the engines that your program has already opened in `server.Options`,
because each engine opens only once per store. If you forget one, a client
that uses that engine gets an in-use error. The error names the option to set,
for example `server.Options.KV`. The server opens any other engine itself when
a client first uses it.

The data is the same in every language. If the Go program stores a struct:

```go
type Prefs struct {
	Theme string `json:"theme"`
}

prefs, err := kv.OpenBucket[Prefs](ctx, state, "prefs")
err = prefs.Set(ctx, "42", Prefs{Theme: "dark"})
```

then Bun and Python read it while the program runs:

```ts
const prefs = store.kv.bucket<{ theme: string }>('prefs')
console.log(await prefs.get('42')) // { theme: "dark" }
```

```python
prefs = store.kv.bucket("prefs", dict)
print(await prefs.get("42"))  # {'theme': 'dark'}
```

In KV, strings, bytes, integers, booleans and floats are stored the same way
in every language, and any other value is stored as JSON. A job's value is
always JSON, and SQL values are SQLite's own types.

## Send calls together

```ts
const profiles = await Promise.all(userIds.map(id => users.get(id)))
```

```python
profiles = await asyncio.gather(*(users.get(user_id) for user_id in user_ids))
```

Each SDK call is a round trip to the server, but many calls can be in flight on
one connection at the same time. Send independent calls together instead of
one after another, and writes from many calls are committed to disk together.
In Go, calls are function calls inside your program, and goroutines run them
in parallel.

## Handle errors

Every error has a kind, and the kinds are the same in every language:

```ts
import { ConflictError } from '@tinyshed/tinystore'

try {
	await drafts.set(noteId, draft, { ifVersion: version })
} catch (err) {
	if (err instanceof ConflictError) {
		return reloadDraft() // someone else saved first
	}
	throw err
}
```

```python
from tinystore import ConflictError

try:
    await drafts.set(note_id, draft, if_version=version)
except ConflictError:
    return await reload_draft()  # someone else saved first
```

```go
err = drafts.Set(ctx, noteID, draft, kv.IfVersion(version))
if errors.Is(err, tinystore.ErrConflict) {
	return reloadDraft() // someone else saved first
}
```

| Kind | Go | Bun and Python | Meaning |
|---|---|---|---|
| invalid | `ErrInvalid` | `InvalidError` | the request is wrong, for example a value that can't be stored |
| limit | `ErrLimit` | `LimitError` | a size, count or memory limit was reached |
| conflict | `ErrConflict` | `ConflictError` | a version or a condition no longer matches |
| closed | `ErrClosed` | `ClosedError` | the store or the handle is closed |
| in use | `ErrInUse` | `InUseError` | the directory or the name is already in use |
| corrupt | `ErrCorrupt` | `CorruptError` | stored data failed its checksum |
| too old, too new | `ErrTooOld`, `ErrTooNew` | `TooOldError`, `TooNewError` | a timestamp is outside the engine's time window |
| outcome unknown | the engine's `ErrOutcomeUnknown` | `OutcomeUnknownError` | a write may or may not have been saved, so read before you retry |
| permission | | `PermissionDeniedError` | the connection's token doesn't allow the call |
| unimplemented | | `UnimplementedError` | the server is older than the SDK and doesn't know the call |

A `LimitError` also says which limit was reached, how much the call wanted and
what the limit is.

## Cancel a call

```ts
import { withSignal } from '@tinyshed/tinystore'

await withSignal(AbortSignal.timeout(2000), async () => {
	const notes = await db.all`select * from notes`
})
```

```python
async with asyncio.timeout(2):
    notes = await db.all(Note, "select * from notes")
```

```go
ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
defer cancel()
notes, err := sqldb.All[Note](ctx, db, `select * from notes`)
```

Each language cancels in its own way: a context in Go, a cancelled task in
Python, and an `AbortSignal` in Bun. In Bun, every call inside `withSignal`
uses the signal, and `work`, blob uploads and blob downloads also accept a
`signal` option. A cancelled call stops on the server too. A write that has
already started is either saved completely or not at all.

## Times and durations

| | Go | Bun | Python |
|---|---|---|---|
| A point in time | `time.Time` | `Date` | `datetime` |
| A duration | `time.Duration` | milliseconds, or text such as `'1h30m'` | `timedelta`, seconds, or text such as `"1h30m"` |

A duration written as text uses the units `w`, `d`, `h`, `m`, `s` and `ms`,
each at most once and from the largest to the smallest: `'30d'`, `'1h30m'`,
`'250ms'`. In TypeScript, a misspelled duration such as `'1hr'` doesn't
compile.

## Update the SDK

A sidecar keeps running the binary that started it until it has been idle for
30 seconds. If your program restarts sooner after an SDK update, it finds the
old sidecar still running. The new SDK then replaces the sidecar. It asks the
old sidecar to stop, starts the new binary, and prints one line about it:

```text
tinystore: the sidecar serving ./data was v0.1.0, older than this SDK's 0.2.0; it finishes its calls, and this SDK's binary serves the directory from now on
```

The old sidecar lets its running calls finish first. Other programs that use
the directory reconnect to the new sidecar by themselves.

The SDK replaces only a sidecar, which is a server that an SDK or the
`tinystore` command started in the background. A person may run an older
server with `tinystore serve ./data`, or a Go program may share its store with
`server.Share`. The SDK doesn't stop those servers. It prints a warning and
uses the server as it is.

## See also

- [The Bun and Node API](reference/bun.md) and
  [the Python API](reference/python.md): every call, by engine.
- [The wire protocol](wire.md): the bytes between an SDK and the server, for
  anyone writing a client in another language.
