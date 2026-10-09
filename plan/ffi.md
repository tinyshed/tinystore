# The byte pipe

The FFI knows nothing of engines. It carries the same frames a socket does,
whole: `HELLO` and `WELCOME`, `REQUEST`, `RESPONSE` and `DATA`, `CANCEL`,
`CREDIT`, `PING` and `GOAWAY`, as [docs/wire.md](../docs/wire.md) defines them.
An SDK speaks the protocol and changes only where it writes the bytes: into a
socket for a sidecar or a server, into the pipe for the core in its own
process.

So the pipe never changes. A new call, a new engine, channels, indexes: each
is new messages in the protocol, negotiated in `HELLO` like a server's
version, and the five functions below stay as they are.

## The functions

```c
typedef struct tinystore_conn tinystore_conn;
typedef struct { uint8_t *ptr; size_t len; } tinystore_bytes;

/* Opens the store in dir, or joins it when this process has it open. */
int32_t tinystore_open(const uint8_t *dir, size_t dir_len,
                       const uint8_t *options, size_t options_len,   /* MessagePack */
                       void (*wake)(void *ctx), void *ctx,
                       tinystore_conn **conn, tinystore_bytes *error);

/* Takes frames; returns the frames ready at once, perhaps none. */
tinystore_bytes tinystore_send(tinystore_conn *conn, const uint8_t *frames, size_t len);

/* Returns the frames ready since, waiting up to wait_ms for the first; 0 does not wait. */
tinystore_bytes tinystore_recv(tinystore_conn *conn, uint32_t wait_ms);

/* Ends the connection; the store closes with its last connection. */
void tinystore_close(tinystore_conn *conn);

/* Gives back what send, recv or open returned. */
void tinystore_free(tinystore_bytes bytes);
```

The napi-rs and PyO3 bindings expose `open`, `send`, `recv` and `close`; their
runtimes own the memory, so they need no `free`.

- **A connection is local.** Its capability is `admin`, with no token and no
  proof: the process opened the directory itself.
- **One store a directory a process.** Opening a directory this process
  already has open joins its store with a new connection. When another
  process holds the directory, `open` fails with `in use`, and the SDK
  connects to that process's server, as a sidecar client finds one today.
- **`send` never waits for the disk.** It parses the frames and dispatches
  them; what is ready at once comes back from the call, everything else
  through `recv`. A write joins the group commit and its answer arrives after
  the commit. A point read may answer inline once phase 1 has measured that it
  pays; the functions do not change either way.
- **`wake` is edge-triggered.** A core thread calls it once when frames become
  ready after an empty `recv`, and not again until the host has called `recv`.
  It must return quickly and must not call into the core. A host that reads
  with a blocking `recv`, as Go does from a goroutine, passes no `wake`.
- **A panic never crosses.** Every function catches unwinding; a panic in the
  core ends the connection with `GOAWAY` and the code `internal`.
- **A connection is safe from any thread**, so Go may call `send` from many
  goroutines; frames from one call stay together.
- **Credit is smaller than a socket's.** A stream's credit on the pipe is a
  fraction of the 2 MiB a socket gets, so a slow consumer holds little memory;
  phase 1 picks the number.

## Each language

| Language | Embedded                                            | Loads                              | Wake                                                | Sidecar and remote         |
|----------|-----------------------------------------------------|------------------------------------|-----------------------------------------------------|----------------------------|
| Rust     | the engines directly, no pipe                       | the crates                         | —                                                   | a client crate             |
| Go       | cgo and a static library                            | `libtinystore.a`, built in Actions | none: a goroutine blocks in `recv`                  | the same SDK over a socket |
| Bun      | bun:ffi                                             | the dynamic library                | a threadsafe `JSCallback`                           | the same SDK over a socket |
| Node     | napi-rs                                             | `tinystore.node`                   | a threadsafe function                               | the same SDK over a socket |
| Python   | PyO3, one abi3 wheel a platform for every CPython 3 | the extension module               | `loop.call_soon_threadsafe`; GIL released in `recv` | the same SDK over a socket |

bun:ffi is experimental by Bun's own documentation; the owner chose it to try
it (decision 10). If it fails a gate, Bun loads the napi-rs addon instead,
which Bun supports, and nothing above the binding changes.

Go with cgo turned off cannot link the library; its SDK then reaches the core
only as a sidecar.

## What ships

Actions builds everything for seven targets: Linux x64 and arm64 with glibc
and with musl, macOS arm64 and x64, Windows x64. Applications never compile
Rust or SQLite.

| Channel   | What it holds                                                                                            |
|-----------|----------------------------------------------------------------------------------------------------------|
| npm       | the SDK, and one optional package a platform with the dynamic library, the Node addon and the executable |
| PyPI      | one abi3 wheel a platform with the extension module and the executable                                   |
| Go        | the SDK module, and one module a platform with the static library, chosen by build tags                  |
| crates.io | the crates                                                                                               |
| GitHub    | the executable a platform, beside the server image                                                       |

## What the protocol must learn

The pipe makes the protocol the whole API, and today it is less than Go's:

- an interactive transaction in sql, reading and then writing (today only
  `batch`);
- each engine's options when it opens, from any language (`RetentionOf` is
  Go-only today);
- config: the layers, `explain`, `update` and watching, now written again in
  every SDK;
- the logger: a record in, its console line back;
- metrics instruments, buffered in the SDK and flushed as one message;
- every other Go-only call, which each engine's API book lists.

## What phase 1 measures

Against Go `e81a050` on the same machine, in the same session:

- kv get and set in every mode: Rust, the pipe from Go, Bun, Node and Python,
  and a sidecar;
- 25,000 rows through the pipe into Bun against native Rust, Prisma's case;
- an idle store with kv, sql and jobs open, in each host;
- the time from a write's commit to its promise resolving in Bun and Node;
- whether a point read answered inline in `send` beats the store's threads.

The estimate before measuring is 1–2 µs a call for MessagePack and the
session, against 40–200 µs through a sidecar.
