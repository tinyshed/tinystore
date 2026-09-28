---
name: wire-change
description: Use when adding or changing anything in TinyStore's wire protocol - a message, a field, a method, an error code, a frame rule, the handshake, SERVE's format - or the vectors every SDK is tested against (server/wire, docs/wire.md, server/wire/testdata/vectors.json).
---

# Changing the wire protocol

The protocol is the one contract three languages share, so a change is only
done when every side of it changed together.

## One commit carries all of it

- `server/wire`: the message's `Append` and `Decode`, and its checks.
- The server: the handler in `server/<engine>.go`, the method in
  `(*Server).<engine>Methods`.
- `server/internal/client`, when the Go client the tests use needs it.
- `docs/wire.md`: the message's table, the text around it, and an example of
  its bytes when it is a new kind of message.
- `server/wire/testdata/vectors.json` and `server/wire/vectors_test.go`.
- The seeds of `FuzzMessages` (`msgpack_test.go`) and `FuzzFrames`
  (`frame_test.go`) when a new rule refuses something.
- `sdk/bun` and `sdk/python`, once they exist.
- The AGENTS.md gates table, for each promise the change makes.

Nothing is released yet, so a field may still be repurposed, as `HELLO`'s field
6 went from the instance to the challenge; after a release a message never
changes meaning and a new field is added instead.

## The profile's rules

- A body is a MessagePack map with small integer keys, written canonically:
  the shortest integer, keys as the struct declares them; a decoder takes any
  order and skips keys it does not know.
- `str` is UTF-8 and anything else is `bin`; a field is refused when its type
  is not the one the table gives, and `nil` only where a field says so.
- A field at its zero value is left out, unless the table says otherwise.
- A new method takes the next number in its engine's range: kv `0x01xx`, jobs
  `0x02xx`, blobs `0x03xx`, sql `0x04xx`, records `0x05xx`, metrics `0x06xx`.
- Every answer is checked against the agreed body before it leaves; one that
  can pass it comes in `DATA` pieces within the stream's credit, as a long
  metrics series does.

## Bytes you can trust

- Write a vector's bytes with Go, `wire.AppendFrame` of the message's
  `Append`, never by hand; check arithmetic and cryptography with a second,
  independent implementation, as the proof's vector was checked with Python's
  `hmac`.
- Scripts that write hex or backslashes: see `platform-traps`, since the
  editing tools may turn escapes into characters.

## Tests to add

- A round trip in the message's `_test.go`.
- An accepted vector for each new frame, checked by
  `TestTheExamplesAreWhatTheMessagesWrite`, and a refused vector for each new
  rule.
- A test over the wire in `server/*_test.go`, through `internal/client` or raw
  frames, that the server does what the table promises, a refusal included.

## Checks

`go -C server test ./...`, which runs the fuzzers' seeds; lint for Windows and
`GOOS=linux`; the race suite in the container. The `verify` skill has the
commands.
