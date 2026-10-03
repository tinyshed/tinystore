---
name: sdk
description: Use when building or changing a TinyStore client SDK (sdk/js, sdk/python) or any code that talks to `tinystore serve` as a client - finding or starting the sidecar, the handshake and its proof, streams, credit, errors, value mapping, reconnecting, and packaging the server binary with the SDK.
---

# Building a TinyStore SDK

A client is the other half of a contract the server already keeps. Get the
contract from the documents, not from the Go client in
`server/internal/client`, which is a test's and a measurement's.

## Read first

- `docs/wire.md`: frames, `HELLO` and `WELCOME`, *Finding a local server*,
  *Versions*, every engine's methods, the error codes.
- Research's design/server.md, the design both SDKs were built to: *What a
  connection may do*, *Across the wire*, *SDKs*. It is kept as it was; where it
  and the code disagree, the code and its tests decide.
- `server/wire/testdata/vectors.json`: the bytes every SDK is tested against;
  `messages.json` beside it, every message by its fields' names, with the
  methods' numbers and the errors' codes.

One commit changes the protocol, the server, both SDKs and the vectors. The
SDKs live in `sdk/js` (Bun and Node) and `sdk/python`, and `sdk/go.mod`
keeps them out of the Go module's zip. What they look like to an application,
beside Go, is the guides in `docs/`, three languages a page; the rules every
call follows, and any change to what a program writes, are the `api-change`
skill's, since it reaches all three.

## Reaching the server, in this order

1. **A private child** (`open(dir, { private: true })`): spawn
   `tinystore serve --dir <dir> --stdio`. Frames go on stdin and stdout; read
   stderr to its end, since a pipe nobody drains stops the server once it
   fills. Closing stdin is the shutdown; the child also leaves on a signal.
2. **The directory's sidecar** (the default):
   - read `<dir>/server/SERVE`: `protocol`, `server`, `pid`, `instance`,
     `secret` (32 bytes, base64url without padding) and `endpoints`; open,
     read and close it at once, since Windows refuses to replace a file a
     reader holds;
   - connect to `endpoints[0]` and send `HELLO` with a fresh `challenge` of 16
     random bytes;
   - before any `REQUEST`, check that `WELCOME.proof` is the HMAC-SHA256 of
     the challenge keyed with the decoded secret, compared in constant time.
     A process that took a dead server's endpoint cannot answer it; the
     `proofs` vector is the test;
   - on any failure (no file, a refused connection, a proof that does not
     check), run `tinystore serve --dir <dir> --local --log
     <dir>/server/serve.log`, detached and with no pipes, since nothing will
     read them and it outlives this process. Exit code 3 means
     another process holds the directory: read `SERVE` again until the winner
     answers, five seconds at most, and start once more when none does, since
     a sidecar leaving for idleness removes `SERVE` first and holds `LOCK`
     until its store has closed; any other code, or no answer, is an error
     quoting the log's last lines;
   - never trust `pid`: pids are reused, and `LOCK` is the truth.
3. **A remote server**: `tls://` verifies the server's certificate before the
   token leaves in `HELLO`; `tcp://` sends the token in the clear.

Endpoints: `pipe:tinystore-<16 hex>` on Windows (`\\.\pipe\...`: Bun through
`node:net`, Python through asyncio's Proactor `create_pipe_connection`;
CPython on Windows has no `AF_UNIX`), `unix://<path>` elsewhere, which may lie
under `$XDG_RUNTIME_DIR` when the store's path is long.

## On the wire

- Every call is a stream of its own: a `u32` number, never 0, never one still
  in use. Answers come in any order; calls return promises.
- Frames asked for in one microtask (JavaScript) or one loop turn (asyncio)
  leave in one write.
- Credit: the connection's (8 MiB from `WELCOME`) bounds `REQUEST` and `DATA`
  bytes the client sends until the server grants more with `CREDIT` on stream
  0; a stream's credit bounds its `DATA` both ways, and the client grants a
  download's with `CREDIT` on the stream. A client past its credit loses the
  connection.
- Answer `PING` with `PONG` carrying the same eight bytes.
- After `GOAWAY` open no stream; a `REQUEST` that crossed it is answered
  `unavailable` without running and may be sent again.
- Errors are their code's class (`ConflictError`, `LimitError` and the rest)
  and carry `what`. A lost connection rejects writes in flight with
  `outcome_unknown`; a read may be sent again. Connect again, open the handles
  again, and resume a work loop.

## Values

- kv values are what `kv.Raw` holds: nothing, an int64, or bytes. Map bytes
  and strings to bytes, integers and booleans to an integer, floats to their
  eight bytes big-endian, anything else to JSON, as Go's `codecFor` does, so
  that Go and every SDK read each other's buckets.
- A 64-bit integer past 2^53 is a `BigInt` in JavaScript: record times in
  nanoseconds always are.
- Text whose bytes are not UTF-8 travels as `bin`.
- The vocabulary is Go's: a type is given once, where a bucket or queue
  opens; the daily calls are plain verbs; engine choices go into open's
  options, never into a call.

## Tests and packaging

- Every SDK decodes and encodes every vector: `values`, `refused`, `frames`,
  `refused frames`, `proofs`, and each of `messages.json` through its own
  message codecs, whose field names are wire.md's (`if absent` is `ifAbsent`
  in JavaScript and `if_absent` in Python), so the loop needs no table.
- Every SDK's logger writes the console lines of
  `records/testdata/console.json` byte for byte, as Go's handler does; `go
  test ./records -run TestConsoleLinesAreTheVectors -update` writes them from
  Go's, and a change to the format changes all three in one commit.
- Integration tests run against a real `tinystore serve` built from
  `cmd/tinystore`, over every transport the platform has.
- The binary is pure Go (`CGO_ENABLED=0`), so every target builds from one
  machine: per-platform optional dependencies on npm, per-platform wheels on
  PyPI.

## What building them taught

- Each SDK's layers carry the same names: `wire/` (codec, frames, messages),
  a session without I/O, a runtime file (Bun's and Node's; asyncio's), a connection
  with its `Link` that dials again, a store, an engine a file. A message's
  fields are wire.md's names, camelCase or snake_case, so the vector tests
  are one loop.
- A handle opens with its first call and again on each connection; `open`,
  `connect` and `sql`, which migrates, are awaited.
- Batches record calls and send them as the block ends: a callback in
  JavaScript, `async with` in Python; each call's promise or future settles
  with the batch.
- Python calls the `cancelled` code `CallCancelledError`, apart from
  asyncio's; a cancelled task cancels its stream.
- Bun 1.4's `expect(p).rejects` does not run the loop's I/O, so a call
  answered by the server never settles in it: tests take the error with a
  `caught()` helper.
- `records.lines` reach a read after the server's next flush, a second.
- The JS SDK touches Bun or Node only in `src/runtime/bun.ts` and
  `src/runtime/node.ts`, which `store.ts` picks as it loads; both load under
  Node, so `bun.ts` reads `Bun` only inside a call. Bun imports `src`; Node
  the `dist` that `bun run build` compiles and a release packs, since Node
  strips no types in `node_modules`. `test/under-node.ts` runs the SDK under
  Node through its own runtime (`node --test`, Node 22.18 or later); the Bun
  suite stays Bun's. A package's `exports` conditions are tried in order:
  `bun`, then `types`, then `default`, last.
- Both packages install the binary as the `tinystore` command:
  `sdk/js/bin/tinystore.js`, plain JavaScript that Bun and Node both run, and
  `tinystore._cli` as a console script. Neither looks on PATH, where it may
  find itself.
- `close` waits for a private child until it exits, in both SDKs: until then
  it holds the directory, and asyncio warns of its pipes.
- `task sdk` checks and tests both, building `tinystore` from this
  repository (`test/binary.ts`, `tests/conftest.py`) unless `TINYSTORE_BIN`
  names one; it is part of `task check`, and CI runs it on Linux, Windows and
  macOS.
- Formatting is the tools', never by hand: `bun run check:write` in `sdk/js`
  (biome, then `tsc`), and `uv run ruff format .` and `uv run ruff check --fix
  .` in `sdk/python`. ruff formats the Python blocks of `README.md` too, so
  run it after editing the README.
- `tsc` runs with `exactOptionalPropertyTypes`: an option a caller may pass as
  `undefined`, as spreading another query does, is declared
  `after?: string | undefined`.
- A Bun test file opens one private store in `beforeAll`, so each of its tests
  names a bucket or queue of its own; Python's `store` fixture opens one a
  test, in `tmp_path`. Both are private children: a test of the sidecar opens
  one itself, with a short `idle`.
- Windows hides two things only Linux shows: pyright narrows a platform only
  by `sys.platform == "win32"` written at the test, not through a constant;
  and a refused socket is `ECONNREFUSED` at once, where a Windows client
  waits, so `dial` turns it into `ClosedError`. `task sdk:linux` runs both
  suites in a Linux container (`sdk/test.Dockerfile`), the repository copied
  in so `node_modules` and `.venv` stay the host's.

Platform behaviour behind several of these rules is in the `platform-traps`
skill; a protocol change is the `wire-change` skill.
