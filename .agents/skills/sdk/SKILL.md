---
name: sdk
description: Use when building or changing a TinyStore client SDK (sdk/bun, sdk/python) or any code that talks to `tinystore serve` as a client - finding or starting the sidecar, the handshake and its proof, streams, credit, errors, value mapping, reconnecting, and packaging the server binary with the SDK.
---

# Building a TinyStore SDK

A client is the other half of a contract the server already keeps. Get the
contract from the documents, not from the Go client in
`server/internal/client`, which is a test's and a measurement's.

## Read first

- `docs/server.md`: *Starting and finding a server*, *SERVE*, *What a
  connection may do*, *Across the wire*, *SDKs*.
- `docs/wire.md`: frames, `HELLO` and `WELCOME`, every engine's methods, the
  error codes.
- `server/wire/testdata/vectors.json`: the bytes every SDK is tested against.

One commit changes the protocol, the server, both SDKs and the vectors. The
SDKs live in `sdk/bun` and `sdk/python`, and `sdk/go.mod` keeps them out of the
Go module's zip.

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
     check), run `tinystore serve --dir <dir> --local`. Exit code 3 means
     another process holds the directory: read `SERVE` again until the winner
     answers, five seconds at most, and start once more when none does, since
     a sidecar leaving for idleness removes `SERVE` first and holds `LOCK`
     until its store has closed;
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
  `refused frames`, `proofs`.
- Integration tests run against a real `tinystore serve` built from
  `cmd/tinystore`, over every transport the platform has.
- The binary is pure Go (`CGO_ENABLED=0`), so every target builds from one
  machine: per-platform optional dependencies on npm, per-platform wheels on
  PyPI.

Platform behaviour behind several of these rules is in the `platform-traps`
skill; a protocol change is the `wire-change` skill.
