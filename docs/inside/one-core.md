# One core, four languages

TinyStore's engines are written once, in Rust. A Rust program calls them
directly. Bun, Node, Python and Go programs reach the same core through one
wire protocol, whether the core runs inside their process, beside it as a
sidecar, or on another machine. This chapter explains the protocol, the four
C functions that carry it inside a process, and how a program finds and
trusts its sidecar.

## The shape

```text
Rust program                   Bun · Node · Python · Go program
    │                          SDK: one typed API per language
    │                               │  frames of the wire protocol
    │                ┌──────────────┼──────────────────┐
    │            the pipe        a sidecar           a server
    │            (in process)    (socket, named pipe) (TCP and TLS, tokens)
    │                └──────────────┼──────────────────┘
    │                    session: decodes frames, answers them
    └──────────────▶ engines · store · SQLite
```

The engines know nothing of the protocol, and the protocol knows nothing of
the transport. An SDK writes the same bytes into whichever it was given.

## Frames

Everything on the wire is a frame: a header of twelve bytes, then a body.

```text
0             4      5       6          8             12
│ body length │ kind │ flags │ method   │ stream      │ body …
```

A connection opens with `HELLO` and `WELCOME`, which agree on the protocol's
version and the largest body. A call is a `REQUEST` and its `RESPONSE` on a
stream number the client picked; many streams share one connection, so a slow
download never blocks a quick call. A download, such as a page of events or a
file's bytes, is a `RESPONSE` followed by `DATA` frames.

A body is one MessagePack value: a map from small integer keys to values, the
same keys in every language, generated from one schema per engine in
`protocol/*.wire`. The schema writes the codec of every language and the test
vectors each of them must read and write byte for byte, so the four SDKs
cannot drift apart. The [wire protocol](../wire.md) page has every rule.

## Credit

A receiver grants credit, and a sender never sends more than it was granted.
A program that reads a tail of events slowly holds the server's sending, not
the server's memory: the server stops at the credit the client gave, and a
slow subscriber of a channel is told what it missed instead of growing a
buffer. A connection holds at most its streams times the agreed body.

## The pipe: four functions

Inside a process, the frames go through four C functions:

```c
size_t tinystore_open(dir, options, wake, ctx, &conn, error, error_room);
size_t tinystore_send(conn, frames, len, into, room);
size_t tinystore_recv(conn, wait_ms, into, room);
void   tinystore_close(conn);
```

`send` takes frames and writes back the ones ready at once. A write's answer
is ready only after its commit, so it arrives later through `recv`. When
frames become ready, the core calls `wake` once, and the host reads them:

| Language | Loads the core through                       | Learns of an answer by                           |
|----------|----------------------------------------------|--------------------------------------------------|
| Bun      | `bun:ffi` and the dynamic library            | a threadsafe `JSCallback`                        |
| Node     | the napi-rs addon, `tinystore.node`          | a threadsafe function                            |
| Python   | the PyO3 module, `tinystore._core`           | a thread waiting in `recv` with the GIL released |
| Go       | the core's library: cgo, or a DLL on Windows | a goroutine waiting in `recv`                    |

No memory of the core's crosses the boundary: the host gives the buffers, the
core reads one and writes the other before it returns, and nothing is left to
free. A panic in the core never crosses either: it ends the connection with
`GOAWAY` and the code `internal`.

Every new call of every engine is a new message in the protocol. The four
functions do not change when a feature is added.

## The sidecar and its proof

A store's directory has one owner, the process that holds its `LOCK`. When a
Bun, Python or Go program opens a directory without `embedded`, its SDK
looks for the owner:

1. It reads `server/SERVE` in the directory: the server's endpoints, a Unix
   socket or a Windows named pipe, and a secret.
2. It connects and sends `HELLO` with 16 random bytes as a challenge.
3. The server's `WELCOME` carries a proof: the HMAC-SHA256 of the challenge,
   keyed with the secret. The client checks it before it sends any request.
4. If any step fails, the SDK starts `tinystore serve --local` for the
   directory. That process takes `LOCK` and publishes a new `SERVE`, or finds
   `LOCK` held and exits, and the client tries again.

The `server/` folder is readable by the directory's owner alone. A name such
as a Windows pipe can be taken by anyone once its server is gone, so the
proof, not the name, says that the server really serves this directory.

The sidecar exits 30 seconds after its last client leaves. Four web processes
and a cron script that open the same directory all join one sidecar.

## A remote server

`tinystore serve ./data --listen tls://0.0.0.0:7443` serves the same protocol
over TLS, to clients that connect with a token. A token gives `admin`,
`data`, which reads and writes, or `read`, which can only call methods the
schema marks as reads. A local connection, through the pipe or a sidecar,
needs no token: its permission is the file system's.

## Why this design?

**Why the protocol is the boundary, even in one process.** The first versions
had a Go engine and re-implemented parts of it in each SDK. A C API with a
function per call would have needed a new function in four bindings for
every feature. With the protocol as the boundary, a feature is a message, its
dispatch and its typed call in each SDK, and the core's behaviour is the same
for every language because it is the same code.

**Why MessagePack with integer keys.** It is compact, every language has a
fast encoder, and integer keys keep a message small without a schema at run
time. The profile is strict, one way to write each value, so that the
vectors can check bytes, not only meaning.

**Why a sidecar by default and not embedded.** An embedded store belongs to
one process. Web servers run several, plus scripts and cron jobs, and only
one process may own a directory. The sidecar gives all of them one owner. A
program that is the only one ever to open its store chooses `embedded` and
saves a hop per call.

## See also

- [The journey of a write](journey-of-a-write.md): one call through these
  layers.
- [One directory, many engines](../start/directory.md#four-ways-to-open-a-store):
  the four ways to open a store from the program's side.
- [Wire protocol](../wire.md): every frame, rule and method.
