# Connecting

One core serves every language. A Rust program runs it in its own process. A
program in TypeScript, Python or Go reaches the same core one of four ways,
and every call after `open` is the same whichever it is.

```ts
const shared = await open('./data') // the directory's sidecar, shared with every process here
const mine = await open('./data', { private: true }) // a child of this process alone
const inside = await open('./data', { embedded: true }) // the core in this process
const remote = await connect('tls://db.internal:7443', { token: process.env.TINYSTORE_TOKEN! })
```

```python
shared = await tinystore.open("./data")
mine = await tinystore.open("./data", private=True)
inside = await tinystore.open("./data", embedded=True)
remote = await tinystore.connect("tls://db.internal:7443", token=os.environ["TINYSTORE_TOKEN"])
```

```go
shared, err := tinystore.Open(ctx, "./data")
mine, err := tinystore.Open(ctx, "./data", tinystore.Private())
inside, err := tinystore.Open(ctx, "./data", tinystore.Embedded())
remote, err := tinystore.Connect(ctx, "tls://db.internal:7443", os.Getenv("TINYSTORE_TOKEN"))
```

```rust
let store = Store::open("./data", Options::default())?; // always the core in this process
```

## Which one

| Way                  | What runs                                                                              | Choose it for                                     |
|----------------------|----------------------------------------------------------------------------------------|---------------------------------------------------|
| sidecar, the default | one `tinystore serve` beside the directory, started by the first process that opens it | a web server and its workers in several processes |
| `private`            | a child on this process's stdin and stdout, ending with it                             | tests, scripts, one process alone; a test's clock |
| `embedded`           | the core inside this process: no child, no socket                                      | one process that wants the shortest path          |
| `connect`            | a server on another machine, over TLS with a token                                     | programs on several machines                      |

- **One directory, one holder.** A store's directory is held by one process,
  through its `LOCK`. Several processes on one machine share it through the
  sidecar, which the first starts and the others find through
  `<dir>/server/SERVE`. A second `embedded` or `private` open of a directory
  another process holds fails `in use`.
- **The sidecar stays** 30 seconds after its last connection goes, unless `idle`
  says otherwise, so a quick restart finds it running. To open its directory
  `embedded` sooner, stop it first with `tinystore stop <dir>`; the `in use`
  error says so too.
- **Embedded** loads the core's library: under Bun the C library, under Node
  its addon, in Python the extension module, in Go the core's library. The
  host's event loop never blocks: every call answers through a completion.
- **A private child** can run on a test's clock: `clock` sets it, and
  `store.clock.advance('1h')` moves time instead of the test waiting.

## A remote server

```sh
tinystore token new data >> tokens.txt
tinystore serve ./data --listen tls://0.0.0.0:7443 --tls-cert cert.pem --tls-key key.pem --tokens tokens.txt
```

The client checks the server's certificate before the token leaves. A
connection that drops is made again by the SDK: a subscription comes back
with `onMissed`, and a worker's jobs return to the queue as a dead worker's
would. `tcp://` sends the token in the clear, for a network its operator
trusts. See [serve](cli/serve.md).

## What every way shares

The calls, their errors and their limits are the same in every way, since
they are the same core speaking the same protocol. What differs is cost: an
embedded call skips a socket, and a remote one crosses a network. A program
moves from `private` in its tests to the sidecar in development to `connect`
in production by changing one line.

## See also

- [All engines](README.md).
- [One core](../handbook/inside/one-core.md) in the Handbook: frames,
  credit, the pipe and the sidecar's proof.
- [The server](../handbook/nodes/server.md) in the Handbook.
