---
name: wire-change
description: Use when adding or changing anything in TinyStore's wire protocol - a message, a field, a method, an error code, a frame rule, the handshake, store.Options, SERVE's format - or the vectors every SDK is tested against (protocol/*.wire, crates/protocol, crates/tinystore/src/wire, docs/wire.md, testdata/wire).
---

# Changing the wire protocol

The protocol is the one contract Rust, Bun and Node, Python and Go share, and
the pipe of the core in a program's own process carries the same frames as a
socket. A change is done when every side of it changed in one commit.

## Declared once, written everywhere

Every message and method is declared in `protocol/<engine>.wire`, in the
language [protocol/README.md](../../../protocol/README.md) defines;
`connection.wire` holds what no engine owns, `HELLO`, `WELCOME`, failures and
`store.Options`, and `server.wire` the server's own calls. From them
`crates/protocol` writes every codec and vector:

| Written                                      | For                                         |
|----------------------------------------------|---------------------------------------------|
| `crates/tinystore/src/wire/protocol.rs`      | the core, its session and the pipe          |
| `sdk/js/src/wire/protocol.ts`                | Bun and Node                                |
| `sdk/python/src/tinystore/_wire/protocol.py` | Python                                      |
| `sdk/go/internal/wire/protocol.go`           | Go                                          |
| `testdata/wire/protocol.json`                | a vector of every message, every SDK's test |

- `just protocol` writes them all again; `just protocol-check` fails when
  one differs from what the schema writes, and CI runs it. Never edit a
  written file: change the schema or `crates/protocol`.
- A method takes the next number of its engine's range: the server `0x00xx`,
  kv `0x01xx`, jobs `0x02xx`, sql `0x03xx`, blobs `0x04xx`, records
  `0x05xx`, metrics `0x06xx`, config `0x07xx`, backup `0x08xx`, locks
  `0x09xx`, channels `0x0axx`, search `0x0bxx`. An old number never takes a
  new meaning.
- A method's line says whether it reads or writes, which decides what a
  read-only token may call and whether a lost connection may send it again,
  and its shape: `call` for one answer, or a stream.

## One commit carries all of it

1. The schema in `protocol/`, then `just protocol`.
2. The core's handler in `crates/tinystore/src/wire/<engine>.rs`, its route
   in `session.rs` when the method is a stream, and a test in
   `<engine>_tests.rs` that the session does what the schema promises, a
   refusal included.
3. Each SDK's call: `sdk/js/src`, `sdk/python/src/tinystore`, `sdk/go`. The
   `api-change` skill when a program sees it, the `sdk` skill for how.
4. `docs/wire.md` when bytes, frames, credit, the handshake or an error code
   change, and `testdata/wire/vectors.json`, the hand-checked vectors of
   frames, values, refusals and proofs, with each rule that refuses
   something new.
5. The fuzz targets in `fuzz/` when a decoder learns a new shape.
6. GATES.md, a line for each promise the change makes.

`store.Options` reaches the core two ways and both change together: the
pipe's `open` decodes it (`asked` in `crates/tinystore/src/pipe.rs`), and an
SDK that starts a server passes the same as `tinystore serve`'s flags
(`crates/cli/src/args.rs`, `help.rs`, and plan/api/cli.md's table).

## The profile's rules

- A body is a MessagePack map with small integer keys, written canonically,
  the shortest integer, keys in the schema's order; a decoder takes any
  order and refuses a key it does not know as `unimplemented`, naming the
  field: `field 99 of kv.Call: not in tinystore 0.1.0`.
- `str` is UTF-8 and anything else `bin`; a field of the wrong type is
  refused, and `nil` only where the schema says `?`.
- A field at its zero value is left out.
- An answer is checked against the agreed body before it leaves; one past it
  comes in `DATA` pieces within the stream's credit.

Nothing is released, so a field may still be renumbered or repurposed
(AGENTS.md: no versions before `v0.1.0`). After the first release a message
never changes meaning: a new field is added, its zero what an older client
meant, and a new method takes a new number.

## Bytes you can trust

- A vector's bytes come from a codec, never from a hand; arithmetic and
  cryptography are checked with a second implementation, as the `SERVE`
  proof was with Python's `hmac`.
- A script that writes hex or backslashes: the `platform-traps` skill, since
  the editing tools may turn escapes into characters.

## Checks

`just protocol-check`, `just test`, and every SDK: `just sdk`, `just
sdk-node`, `just sdk-python`, `just sdk-go`, or `just quick`, which picks
what the files changed reach. The `verify` skill has the rest.
