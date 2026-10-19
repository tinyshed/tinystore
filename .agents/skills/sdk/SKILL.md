---
name: sdk
description: Use when building or changing a TinyStore SDK (sdk/js for Bun and Node, sdk/python, sdk/go) or any code that talks to the core as a client - the core in the program's own process through bun:ffi, the Node addon, PyO3 or the C library, a private child, the sidecar and its SERVE proof, a remote server, streams, credit, errors, values, reconnecting, and what a platform's package carries.
---

# Building a TinyStore SDK

An SDK is the protocol's client and nothing more: every call is a message the
core answers, so a behaviour is the core's, and an SDK only says it in its
language. Read [plan/protocol.md](../../../plan/protocol.md),
[plan/ffi.md](../../../plan/ffi.md) and [docs/wire.md](../../../docs/wire.md)
first, and the engine's book in `plan/api/` for its call sites in every
language.

| SDK    | Where        | Its core in the process                                       |
|--------|--------------|---------------------------------------------------------------|
| Bun    | `sdk/js`     | bun:ffi over the C library (`src/runtime/pipe.ts`)            |
| Node   | `sdk/js`     | the napi-rs addon, `tinystore.node` (`src/runtime/addon.ts`)  |
| Python | `sdk/python` | PyO3's `tinystore._core`, the core built with `python`        |
| Go     | `sdk/go`     | the C library: cgo's dlopen on Unix, syscall's DLL on Windows |

The C library, the addon and Python's module are `crates/ffi` built three
ways, the same four calls: open, send, recv, close. One package a language;
Bun and Node share one, its runtime chosen as it loads (`typeof Bun`).

## Four ways to a store, one session

Every way carries the same frames to the same session, so a test runs through
all four (`test/ways.ts`, `tests/ways.py`, `internal/ways`):

1. **The core in the process** (`embedded`): the pipe. Bun's wake is a
   threadsafe `JSCallback`, Node's a threadsafe function that keeps the loop
   alive while the store is open, Python's a thread waiting in `recv` with the
   GIL let go, Go's a goroutine in `recv`. A directory another process holds
   is `in use`.
2. **A private child** (`private`): `tinystore serve --dir <dir> --stdio`,
   frames on stdin and stdout; read stderr to its end, since a pipe nobody
   drains stops the server once it fills; closing stdin is the shutdown, and
   `close` waits for the child's exit, since until then it holds the
   directory.
3. **The directory's sidecar** (the default): read `<dir>/server/SERVE`
   (`protocol`, `server`, `pid`, `instance`, `secret`, `endpoints`, and
   `sidecar`), open, read and close it at once; connect to `endpoints[0]`
   with a fresh 16-byte challenge in `HELLO`, and before any `REQUEST` check
   `WELCOME.proof`, the HMAC-SHA256 of the challenge keyed with the secret,
   in constant time. Any failure starts `tinystore serve --dir <dir> --local
   --log-file <dir>/server/serve.log`, detached; exit code 3 means another
   process holds the directory, so read `SERVE` again. A sidecar of an older
   release is stopped and replaced; any other server of an older release is
   told of once. Never trust `pid`: `LOCK` is the truth.
4. **A remote server**: `tls://` checks the certificate before the token
   leaves in `HELLO`; `tcp://` sends it in the clear.

How a store opens, durability, the encryption key's file, a node, its own
metrics, is `store.Options` to the core in the process, and the same as
`serve`'s flags to a child or a sidecar this process starts; one found
running keeps its own.

## What an SDK finds

In this order: what the program gives, the variable, then this platform's
package, then `PATH` for the executable alone:

| What           | Given     | Variable            | Packaged                                                          |
|----------------|-----------|---------------------|-------------------------------------------------------------------|
| the executable | `binary`  | `TINYSTORE_BIN`     | `@tinyshed/tinystore-<target>/bin/`, the wheel's `tinystore/bin/` |
| Bun's core     | `library` | `TINYSTORE_LIBRARY` | `@tinyshed/tinystore-<target>/lib/`                               |
| Node's core    | `library` | `TINYSTORE_ADDON`   | `@tinyshed/tinystore-<target>/lib/tinystore.node`                 |
| Python's core  |           |                     | `tinystore._core`, in the wheel                                   |
| Go's core      | `Library` | `TINYSTORE_LIBRARY` | none yet: the GitHub release's archive                            |

`<target>` is `process.platform-process.arch`, and `-musl` after it on a
Linux whose loader is musl's (`platformPackage` in
`src/runtime/shared.ts`); the `release` skill has what each package holds.

## On the wire

- Every call is a stream of its own; answers come in any order. Frames asked
  for in one microtask or one loop turn leave in one write.
- Credit bounds what each side sends; a client past it loses the connection.
  A stream's answer in parts comes within the credit the client grants.
- After `GOAWAY` open no stream; a `REQUEST` that crossed it is answered
  `unavailable` unrun and may go again.
- A lost connection fails a write in flight `outcome_unknown`; a read goes
  again on the next connection, and every handle opens again with its first
  call there.
- Errors are their kind's class and carry `what`, as the core named them.

## Values

- A bucket's values are the core's serde values, written the same in every
  language: `testdata/kv/values.json` holds what each type becomes, and
  every SDK reads and writes it.
- A 64-bit integer past 2^53 is a `bigint` in JavaScript; an event's time
  in nanoseconds always is.
- Text that is not UTF-8 travels as `bin`.

## Tests

- Every message of `testdata/wire/protocol.json` through each SDK's codec,
  every vector of `testdata/wire/vectors.json`, every line of
  `testdata/console/console.json` printed byte for byte, and
  `testdata/config/env.json` read as the core reads it.
- `just sdk` runs Bun's suites through all four ways; `just sdk-node` runs
  the same files under Node (`test/node/register.ts` answers `bun:test` with
  `node:test` and Jest's `expect`) and `test/under-node.ts`; `just
  sdk-python` and `just sdk-go` the others'. A test file of `sdk/js` writes
  only what both runtimes run: `node:timers/promises`' `setTimeout` for a
  sleep, `node:fs` for a file, `import.meta.dirname`, never the global `Bun`.
- Bun's `expect(p).rejects` does not run the loop's I/O, so a call the core
  answers never settles in it: take the error with `caught()`.

## What building them taught

- The JS SDK touches Bun or Node only in `src/runtime/`; `store.ts` picks one
  as it loads, and both load under Node, so `bun.ts` reads `Bun` only inside
  a call. Bun imports `src`; Node the `dist` that `bun run build` compiles,
  since Node strips no types in `node_modules`, and refuses what it would
  have to compile: parameter properties, enums, namespaces.
- `tsc` runs with `exactOptionalPropertyTypes`: an option a caller may pass
  as `undefined` is declared `after?: string | undefined`.
- A handle that streams, a config's watch or the instruments, is cancelled
  and its END waited for before the connection ends, or a private child or a
  sidecar waits ten seconds to drain it at close.
- Go runs with `GOWORK=off`: a local `go.work` names modules that are gone.
- Formatting is the tools': `bun run check:write` in `sdk/js`, `uv run ruff
  format` and `ruff check --fix` in `sdk/python`, `gofmt -w` in `sdk/go`.

Platform behaviour behind several of these is the `platform-traps` skill; a
protocol change is the `wire-change` skill; a name a program writes is the
`api-change` skill.
