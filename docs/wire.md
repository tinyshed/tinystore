# The wire protocol, version 1

The bytes a TinyStore server and its clients exchange over any byte stream: a
child's stdin and stdout, a Unix socket, a Windows named pipe, TCP or TLS.
Read it to write a client in a language TinyStore has no SDK for, or to see
what an SDK does on the wire; the Bun and Python SDKs are built to this page.

Frames, the profile, the handshake, the errors and every engine's methods, kv,
jobs, blobs, sql, records and metrics, are built in `server/wire`, and why the
server behaves as it does is
[its design](https://github.com/tinyshed/research/blob/main/tinystore/design/server.md).
Every example on this page is a vector of
[testdata/wire/vectors.json](../testdata/wire/vectors.json),
which the server and every SDK are tested against: a value in a typed
notation, the bytes it is, and the bytes a decoder refuses, each named after
the rule it breaks. Every message is also a vector of
[messages.json](../testdata/wire/messages.json), its bytes and its
fields by the names the tables below give them, with the methods' numbers and
the errors' codes, so that an SDK checks each field of each message in one
loop; the Go types write it, through a schema of each message's keys that
`TestMessageVectors` holds.

## Frames

Everything is little-endian. A frame is a twelve-byte header and a body:

```text
0             4      5       6          8             12
│ body length │ kind │ flags │ method   │ stream      │ body …
  u32           u8     u8      u16        u32
```

- **The length is the body's.** A frame is 12 + body length bytes; a body
  past the agreed maximum is refused before a byte of it is read.
- **What is reserved is zero.** An unknown kind, a flag this kind does not
  define, a method outside a `REQUEST` or a stream where the kind names none
  is a protocol error, and the connection ends.

| kind |            | stream                              | method        | body                                    | flags      |
|------|------------|-------------------------------------|---------------|-----------------------------------------|------------|
| 1    | `HELLO`    | 0                                   | 0             | a message                               | —          |
| 2    | `WELCOME`  | 0                                   | 0             | a message                               | —          |
| 3    | `REQUEST`  | the new stream                      | the operation | a message                               | END        |
| 4    | `RESPONSE` | its stream                          | 0             | a message, or an error                  | END, ERROR |
| 5    | `DATA`     | its stream                          | 0             | bytes, or a message, as the method says | END, ERROR |
| 6    | `CANCEL`   | its stream                          | 0             | empty                                   | —          |
| 7    | `CREDIT`   | its stream, or 0 for the connection | 0             | u32: bytes granted                      | —          |
| 8    | `PING`     | 0                                   | 0             | 8 bytes                                 | —          |
| 9    | `PONG`     | 0                                   | 0             | the `PING`'s 8 bytes                    | —          |
| 10   | `GOAWAY`   | 0                                   | 0             | a message                               | —          |

END is 1: the sender's last frame on its stream. ERROR is 2, only beside END:
the body is [an error](#errors).

A method's high byte is its engine and its low byte the operation, so a new
engine takes a new high byte and the frame stays as it is; [Methods](#methods)
has every engine's byte and every method's number.

A `HELLO` from Bun:

```text
16 00 00 00  01  00  00 00  00 00 00 00                                   a body of 22 bytes, HELLO
82                                                                        a map of two
   01 01                                                                  protocol: 1
   02 b1 74 69 6e 79 73 74 6f 72 65 2d 62 75 6e 2f 30 2e 31               client: "tinystore-bun/0.1"
```

## A connection

1. The client sends `HELLO`, and the server answers `WELCOME`, or `GOAWAY`
   and closes. Nothing comes before them, and the server waits five seconds
   for `HELLO`. The connection speaks the older of the client's newest
   protocol and the server's, which `WELCOME` names: a client newer than its
   server speaks the server's protocol, or closes when it no longer does, and
   only a client older than the oldest protocol the server speaks is refused.
2. The client opens streams with `REQUEST`, as many at once as `WELCOME`
   allows.
3. Either side may send `PING`; the other answers `PONG` with the same eight
   bytes.
4. The server ends the connection with `GOAWAY`. The client opens no stream
   after it; a `REQUEST` that crossed it on the way is answered `unavailable`
   without being run, so it may be sent again elsewhere; the streams already
   running finish, and the server closes. Stream numbers are reused, so no
   number could say which requests the server saw: each answer says it.

`HELLO`:

| key | field         | type |                                                                                             |
|-----|---------------|------|---------------------------------------------------------------------------------------------|
| 1   | protocol      | uint | the newest the client speaks, 1                                                             |
| 2   | client        | str  | a name and version, for logs                                                                |
| 3   | token         | str  | required on TCP                                                                             |
| 4   | max body      | uint | the largest body the client takes; the server's when absent                                 |
| 5   | stream credit | uint | the `DATA` the server may send on a stream before the client grants more; 2 MiB when absent |
| 6   | challenge     | bin  | 16 random bytes, when the client found the server through `SERVE`                           |

A body must fit the credit it is sent under, so the max body both sides agree
is the smallest of the server's, the client's and the client's stream
credit: a client granting 64 KiB a stream takes bodies of 64 KiB at most.

`WELCOME`:

| key | field             | type         |                                                                                                                                        |
|-----|-------------------|--------------|----------------------------------------------------------------------------------------------------------------------------------------|
| 1   | protocol          | uint         | the one this connection speaks                                                                                                         |
| 2   | server            | str          | its version                                                                                                                            |
| 3   | instance          | bin          | 16 random bytes a start                                                                                                                |
| 4   | capability        | str          | `admin`, `data` or `read`                                                                                                              |
| 5   | max body          | uint         | the largest body either side sends                                                                                                     |
| 6   | in flight         | uint         | the streams a client may have open at once                                                                                             |
| 7   | connection credit | uint         | the bytes of `REQUEST` and `DATA` bodies a client may send before credit comes back; at least the max body                             |
| 8   | stream credit     | uint         | the bytes of `DATA` a client may send on a stream before credit comes back                                                             |
| 9   | engines           | array of str | what this server serves                                                                                                                |
| 10  | now               | int          | the store's clock, unix milliseconds                                                                                                   |
| 11  | proof             | bin          | on a local connection whose `HELLO` carried a challenge: the HMAC-SHA256 of the challenge, keyed with the 32 bytes of `SERVE`'s secret |
| 12  | durability        | str          | `full` or `os`, when the store was opened with one: how far its files' commits go before they return                                   |

A client that found the server through `SERVE` sends no `REQUEST` before the
proof checks, compared in constant time: only the user the server runs as can
read `SERVE`, so a process that took the endpoint of a server gone cannot
answer, and a client whose proof fails reads `SERVE` again, as a stale one.
`HELLO` carries nothing a stranger could use, since a local connection takes
no token. A challenge of another length is a `HELLO` the server cannot take;
a remote connection's challenge gets no proof, its server being the one its
TLS certificate names. The vectors hold a proof, `proofs`, and a `HELLO` with
a challenge.

A server that does not take the connection at all, past its connections or
closing, still reads the `HELLO` and answers `GOAWAY` where the `WELCOME`
belongs, with `limit` or `unavailable`.

`GOAWAY`:

| key | field   | type |                          |
|-----|---------|------|--------------------------|
| 1   | code    | str  | as [an error's](#errors) |
| 2   | message | str  |                          |

## Finding a local server

A store directory has at most one server, since its `LOCK` lets one process
open it. That server publishes how to reach it in `<dir>/server/`, which only
the user the server runs as may enter, whatever the store's directory lets
others do: mode 0700, and `SERVE` 0600, on Linux and macOS. On Windows, which
lets anyone traverse a directory to a file whose path they know, `server\`
and `SERVE` each have a protected DACL that grants that user every right and
nobody else any, so that neither inherits the store directory's, and what
else is made in `server\` inherits its DACL alone. `SERVE` is created with
its DACL rather than given it after. An owner may change a DACL whatever it
says, so a `server\` another user made first, as a folder at a drive's root
lets every user, is refused, and so is a link in its place: the server does
not start, and its error names the folder. On Linux and macOS such a folder
refuses its new mode. `SERVE` there is a hint; `LOCK` is the truth.

```json
{"protocol": 1, "server": "0.4.0", "pid": 4212, "instance": "q1q0l3yZ3JvYw8g7oM2sYA",
 "secret": "Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE",
 "endpoints": ["unix:///srv/app/data/server/tinystore.sock"], "sidecar": true}
```

| field     |                                                                                                                                                                                                                                                        |
|-----------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| protocol  | the newest protocol the server speaks                                                                                                                                                                                                                  |
| server    | its version                                                                                                                                                                                                                                            |
| pid       | its process, for a person reading the file; no client trusts it, since a pid is reused                                                                                                                                                                 |
| instance  | 16 random bytes a start, base64url without padding, which `WELCOME` repeats                                                                                                                                                                            |
| secret    | 32 random bytes a start, base64url without padding, the key of the proof                                                                                                                                                                               |
| endpoints | where it listens: `unix://` and a socket's path, or `pipe:` and a Windows named pipe's name                                                                                                                                                            |
| sidecar   | `true` when the server is a sidecar, which `tinystore serve --local` starts for its clients. A client of a newer release may replace a sidecar. The field is absent for a Go program's own server and for `tinystore serve <dir>`, which a person runs |

- **Written whole, under the lock.** Only the store holding `LOCK` writes
  `server/`. A server removes a `SERVE` left behind before it listens, since
  the endpoint that file names may be another process's by now, writes its
  own once it listens, as a temporary file renamed into place, and removes it
  when it leaves, before it lets go of `LOCK`.
- **Find or start.** A client reads `SERVE`, connects to an endpoint and
  checks the proof. Any failure, no file, a refused connection or a proof that
  does not check, starts `tinystore serve --dir <dir> --local --log
  <dir>/server/serve.log`. That child takes `LOCK`, which means the old server
  is gone, and publishes itself; or it finds `LOCK` held and exits with code 3,
  and the client reads `SERVE` again until the winner answers, five seconds at
  most, starting one again when none does.
- **The proof.** A client that found its server through `SERVE` sends a
  `HELLO` with 16 random bytes as its challenge, and sends no `REQUEST` until
  `WELCOME`'s proof equals the HMAC-SHA256 of the challenge keyed with the
  secret, compared in constant time. An endpoint's name is anyone's to take
  once its server is gone, a Windows pipe's above all, and only the user the
  server runs as can read the secret. A client whose proof fails reads
  `SERVE` again, as a stale one.
- **A reader delays a change and never fails it.** On Windows a client
  holding `SERVE` open refuses its rename or removal, and so may a scanner;
  the server tries again eight times, from 1 to 64 ms apart.

How a client knows its server depends on the transport:

| transport                                           | how the client knows its server                                              |
|-----------------------------------------------------|------------------------------------------------------------------------------|
| stdio                                               | it started the server itself, as a private child                             |
| a Unix socket or a named pipe found through `SERVE` | the proof                                                                    |
| TCP with TLS                                        | the certificate, checked before its token leaves                             |
| TCP                                                 | not at all: the token travels in the clear, on a network its operator trusts |

Where a local server listens:

- **A Unix socket**, `<dir>/server/tinystore.sock`, when that absolute path
  fits a `sockaddr_un`, 104 bytes on macOS and the BSDs and 108 on Linux;
  past it, the same name in a directory of the owner's own,
  `$XDG_RUNTIME_DIR/tinystore-<hash>`, or in the temporary directory without
  one. `<hash>` is the first eight bytes of the SHA-256 of the store's absolute
  path, in hex: eight, so that a socket under macOS's temporary directory
  still fits.
- **A named pipe on Windows**, `pipe:tinystore-<hash>`, which refuses remote
  clients. Named pipes share one namespace across every user, so it is
  created as the first instance of its name, and a process that took the name
  first is found by the server rather than by its clients. Every instance has
  a DACL that grants the user the server runs as alone, so that no other user
  connects to it or makes an instance of its own. No local endpoint takes a
  token: its permission is the file system's, and a local connection is
  `admin`.

A remote server's tokens file gives each token one capability: `admin`
may do anything, `data` reads and writes, and `read` reads only. A `read`
connection may call only the methods the schema marks `read`, every other
one being `permission`; its SQL runs on a reader and is a query, a
statement that writes, attaches a file or sets a pragma being `permission`
too; and its `sql.open` applies no migration. Its opens may still name a
bucket, a queue or files the store did not have, as any first open does,
but choose nothing the writers live with: a bucket's `idle`, under which a
read renews a key, counters' `flushEvery`, a database's `durability` and a
stream's `keep` are `permission`. Its reads renew no key.

## Streams

A client names a stream with any number from 1 not in use. A number is in use
from its `REQUEST` to the server's final frame on it, and may be named again
after. A `REQUEST` on a number in use, or past the streams in flight, ends the
connection.

| shape      | the client sends               | the server sends                |
|------------|--------------------------------|---------------------------------|
| a call     | `REQUEST`·END                  | `RESPONSE`·END                  |
| an upload  | `REQUEST`, `DATA`…, `DATA`·END | `RESPONSE`·END                  |
| a download | `REQUEST`·END                  | `RESPONSE`, `DATA`…, `DATA`·END |
| both ways  | `REQUEST`, `DATA`…, `DATA`·END | `RESPONSE`, `DATA`…, `DATA`·END |

- **The server ends every stream once**: `RESPONSE`·END or `DATA`·END, either
  with ERROR, a cancelled stream too. It takes the number out of use before
  that frame leaves, so a client may name it again the moment it arrives.
- **A download's `RESPONSE` is its header**, a message of the method's, `{}`
  when it has nothing to say; its items follow as `DATA`, and its last
  `DATA`·END is its trailer, a message saying where the next page begins.
- **`DATA` carries bytes or items.** A blob's bytes are raw; rows, jobs and
  series are a message each. The method says which. A `DATA` carries at least
  one byte, but the one that ends its stream.
- **A transfer shares the connection.** A sender splits bytes into frames of
  64 KiB at most, so that no stream holds the connection longer than one such
  frame takes to send.
- **`CANCEL` asks the server to stop.** It answers with the final frame it
  would have sent, or with the error `cancelled`; a client may drop what
  arrives on a stream it cancelled. A client ends an upload early with
  `CANCEL`: ERROR is the server's alone. A read under way stops at once,
  its statement interrupted, and answers `cancelled`, as do the reads of a
  connection that ends; a call that writes runs to its end.
- **A frame for a stream no longer in use is dropped**: a `DATA` the client
  sent before the final frame reached it, a `CANCEL` or a `CREDIT` that
  crossed it. The dropped `DATA` still gives its connection credit back.
- **A body that is not its method's message fails its stream** with
  `invalid`, naming the rule it broke; a frame that is not a frame ends the
  connection.

## Credit

A sender never sends more than it was granted, and a receiver grants only room
it has, so a connection holds no more than its grants and the server's reader
never waits.

- **Client to server.** Every `REQUEST` and `DATA` body counts against the
  connection's credit, and a `DATA` body against its stream's too. The server
  gives credit back with `CREDIT` as it lets go of bodies: on stream 0 for the
  connection, on a stream's number for that stream.
- **Server to client.** `DATA` bodies count against the stream credit
  `HELLO` stated, and the client grants more with `CREDIT` on the stream as it
  consumes them. A `RESPONSE` body is not counted: it is one message within
  the agreed body, and every collection travels as `DATA` instead, so a client
  holds at most its streams in flight times the agreed body in answers. Nor is
  the `DATA`·END·ERROR that fails a download, so that a stream always has
  room for its end. A client grants nothing for the connection: its `CREDIT`
  names a stream.
- **A grant waits for half the window.** A receiver grants what it let go of
  once half its window has gone, and not a frame for every frame it takes.
- **A body past its credit** ends the connection with `GOAWAY` and `protocol`,
  the connection's last frame.
- **A client that does not read** is read no further. A call's stream ends,
  and its credit comes back, once its answer is queued rather than read, and
  a `PONG` counts against no credit; so a connection that owes its client
  more than 8 MiB reads no frame more until the client takes some, and the
  server stops reading its socket, where the client's sends wait. The
  requests of one write are all read before any of them is answered, so a
  call also checks when a worker starts it. Past 8 MiB, it waits until the
  client has read all but 1 MiB. A connection then owes at most 8 MiB, plus
  one answer for each of the 16 workers, the answers of writes already
  queued for a commit, and the `DATA` that its streams send within the
  credit that the client granted.

64 KiB of credit on stream 5:

```text
04 00 00 00  07  00  00 00  05 00 00 00                                   a body of 4 bytes, CREDIT, stream 5
00 00 01 00                                                               65,536
```

## Messages: the MessagePack profile

A message is one [MessagePack](https://github.com/msgpack/msgpack/blob/master/spec.md)
value: a map from small unsigned integer keys to values, under these rules.

- **Types.** Nil, bool, integers, float 64, str, bin, array and map. A float
  32, an extension type, the timestamp among them, or any other type byte is
  invalid.
- **Integers.** Each field says signed or unsigned and its range. A decoder
  takes any encoding of a value that fits, `ce 00 00 01 2c` for 300 as well as
  `cd 01 2c`, and refuses one that does not.
- **A float keeps its value; bits that must survive are bin.** A float field
  is a float 64: `cb 80 00 00 00 00 00 00 00` is -0. The server keeps its bits,
  so `cb 7f f8 00 00 00 00 00 01`, a NaN whose payload is 1, comes back as it
  went; an SDK keeps the value, NaN, the infinities and -0 included, as far as
  its language does, and JavaScript promises no NaN's payload. A float whose
  bits are the data therefore travels as bin: a kv float value, a series'
  samples, an aggregate's values. A float field also takes an integer that a
  float 64 holds exactly, since a JavaScript encoder writes 3.0 as 3.
- **Text is str, bytes are bin.** A str is valid UTF-8, and neither stands for
  the other.
- **Keys.** A message's keys are unsigned integers; a field that is a map of
  names, an error's `what` or a series' labels, says so and has str keys. A
  key twice is invalid. A key the reader does not know is read past, its value
  still well formed and within bounds, and then it depends on the reader. A
  server refuses a request carrying one, `unimplemented` naming the field,
  since a field it skipped could change what the client asked, and the answer
  would be to another question. A client reading an answer, and a server
  reading `HELLO`, skip it. So a client leaves out a field at its zero value,
  and an older server takes everything a newer client sends that it would
  have sent anyway; and a server adds an answer field only where a client
  that skips it still reads the answer right, or else answers it only to a
  client that asked for it in a field of its request. An absent key is the
  field's default, and nil stands only where a field allows it.
- **Bounds come before allocation.** A length or a count past the bytes left
  in the body is invalid, since every element takes at least a byte; nesting
  deeper than 8 is invalid; a body is at most the agreed maximum. The server
  holds a request's lists in at most 64 times the body's bytes, 64 KiB at
  least: a list past it, as one of a million empty maps, is `limit` before
  it is read.
- **Time is an integer**: unix milliseconds, unless a field says nanoseconds.
  Records keep nanoseconds, past what a JavaScript number holds, so an SDK
  reads such a field as a `BigInt`:

  ```text
  1,790,000,000,123,456,789 ns → cf 18 d7 5b 84 2b 4e cd 15
  ```

  A duration is an integer of milliseconds.
- **Columns of numbers are bin**: a series' times and values, little-endian
  int64 and float64 one after another.
- **Encoders are canonical, decoders are not.** An encoder writes the
  shortest form of every integer, length and count, keys in ascending order,
  every float in eight bytes, so that a vector compares byte for byte; a
  decoder takes any valid form in any order.
- **An SDK's codec is its own**: a general library writes plain maps and
  reads 64-bit integers without rounding when told, but takes a float 32, an
  extension, a key twice or a value nested nine deep, which this profile
  refuses. The SDKs' codecs refuse every `refused` vector.

## Errors

An error is the final frame of its stream, with ERROR set:

| key | field   | type           |                                                                                                |
|-----|---------|----------------|------------------------------------------------------------------------------------------------|
| 1   | code    | str            | below                                                                                          |
| 2   | message | str            | what failed, as the engine's error says it                                                     |
| 3   | what    | a map of names | the item it names: a bucket and key, a queue and key, a series' labels, a table and constraint |

| code                 | means                                                                                                                               | sent again                  |
|----------------------|-------------------------------------------------------------------------------------------------------------------------------------|-----------------------------|
| `invalid`            | the request cannot be done as asked                                                                                                 | no                          |
| `limit`              | a bound: memory, size or count; `what` names the `limit`, what the call `wanted` of it and the `bound`, where the engine knows them | later, or smaller           |
| `closed`             | the store or the handle closed                                                                                                      | after opening again         |
| `in_use`             | a name is taken, or a lock's key stayed held past a hold's wait                                                                     | later                       |
| `conflict`           | a condition or a version no longer holds                                                                                            | after reading again         |
| `corrupt`            | stored bytes no longer read                                                                                                         | no                          |
| `too_old`, `too_new` | a time outside its engine's window                                                                                                  | no                          |
| `suspended`          | a series in quarantine                                                                                                              | after its repair            |
| `outcome_unknown`    | a commit whose result is unknown, or a write that ran and whose answer, as the values a take removed, is past the agreed body       | after reading what it wrote |
| `committed_locally`  | a synchronous commit its home made and no copy it waits for held in time: it stands, and losing that home's disk would lose it      | no                          |
| `lost`               | a lease ran out or was revoked at its maxHold: another holder may have run beside it                                                | no                          |
| `permission`         | the connection's capability does not allow it, as a `read` connection opening an encrypted bucket                                   | no                          |
| `unimplemented`      | a method, or a field of a request, this server does not have: `what` names the field, the message the server's version              | no                          |
| `cancelled`          | the client cancelled it                                                                                                             | —                           |
| `unavailable`        | the server is closing                                                                                                               | on another connection       |
| `internal`           | a fault of the server's                                                                                                             | —                           |

The first nine are the root's sentinels, so that an SDK's error classes mean
what `errors.Is` means in Go. A frame or a `HELLO` the server cannot take is a
`GOAWAY` with `protocol` or `unauthenticated`, and the connection ends. A
name's value in `what` is str, or bin where its bytes are not UTF-8, as a kv
key of bytes may be.

A conflict on stream 7:

```text
2c 00 00 00  04  03  00 00  07 00 00 00                                   a body of 44 bytes, RESPONSE, END and ERROR, stream 7
83                                                                        a map of three
   01 a8 63 6f 6e 66 6c 69 63 74                                          code: "conflict"
   02 ad 73 74 61 74 65 20 63 68 61 6e 67 65 64                           message: "state changed"
   03 81 a6 62 75 63 6b 65 74 a8 73 65 73 73 69 6f 6e 73                  what: {"bucket": "sessions"}
```

## Versions

A server and its clients upgrade apart: a remote server deployed once while
the applications' SDKs move on, or a sidecar still serving a directory after
its application's SDK was updated. So each side promises what it does with an
older or a newer one:

- **A connection speaks the older protocol of its two sides.** `HELLO` says
  the newest the client speaks and `WELCOME` the one the connection speaks; a
  server refuses only a client older than the oldest protocol it still speaks.
  A protocol moves only for what a field cannot carry: frames, the handshake,
  credit.
- **Within a protocol a message grows by fields, and a request is understood
  whole or refused.** A field the server does not know is `unimplemented`,
  naming the field and the server's version, so a newer client using
  something new learns which server to upgrade rather than reading an answer
  to another question. A client leaves a field out at its zero value, so one
  using nothing new talks to any server of its protocol.
- **An answer grows only by what a client may skip.** A field whose meaning a
  client must know is answered only to a client that asked for it with a
  field of its request, which an older server refuses.
- **A method is added, never repurposed.** An older server answers a newer
  method `unimplemented`, naming its version. From `v0.1.0` on, a message
  never changes meaning and a field's number is never reused.

A client that finds a sidecar of an older release than its own replaces it:

1. The client sends `server.stop`. The sidecar answers, lets its running
   streams finish, and gives back `SERVE` and `LOCK`.
2. The client starts its own binary, as it does when no sidecar runs, and
   prints one line about it on stderr.
3. Every other client of the old sidecar gets `GOAWAY` and connects to the new
   sidecar when it connects again.

A client doesn't replace a server that `SERVE` doesn't call a sidecar, such as
a Go program's own server or one that a person runs. It also keeps a sidecar
that refuses to stop. In both cases, it prints a warning once on stderr.

## Methods

Every method is declared in `protocol/*.wire`, a file an engine, in the
language `protocol/README.md` defines: its number, who
may call it, the message it takes, and its answer's shape and messages. Each
message's fields are declared there too, their keys, their types and a
comment after a field saying what it holds, and `testdata/wire/protocol.json`
holds a vector of every message, which every SDK reads. The tables below are
written from those files by `just protocol`, and CI checks them, so a number
here is the number the server answers.

A method's high byte is its engine and its low byte the operation:

<!-- written from protocol/*.wire by `just protocol`: engines -->
| high byte | engine   |
|-----------|----------|
| `0x00`    | server   |
| `0x01`    | kv       |
| `0x02`    | jobs     |
| `0x03`    | sql      |
| `0x04`    | blobs    |
| `0x05`    | records  |
| `0x06`    | metrics  |
| `0x07`    | config   |
| `0x08`    | backup   |
| `0x09`    | locks    |
| `0x0a`    | channels |
| `0x0b`    | search   |
<!-- end of what `just protocol` writes -->

`may` is who may call it: a `read` connection calls the methods marked
`read` alone, and an `admin` method is an admin connection's. An answer
`T` is a call's one message; the others are the shapes of
[Streams](#streams):

| answer                 | the stream                                                                                                                                       |
|------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| `T`                    | a call: a `T` as its `RESPONSE`·END                                                                                                              |
| download `T` until `U` | after its `RESPONSE`, any number of `T` as `DATA`, then a `U` with END; an answer that fits one message may be a `U` as the `RESPONSE`·END alone |
| handover `T`           | a `T` as the `RESPONSE`·END; or the run handed to the client, which sends the `T` to keep as its last `DATA`                                     |
| exchange `T` for `U`   | after its `RESPONSE`, `T` from the server and `U` from the client as `DATA`, until each side ends its own                                        |

The values they carry:

- a kv value is nil, an integer or bin, as its row keeps it, a bin's first
  byte the kind that wrote it (1 text, 2 bytes, 3 JSON, 4 a float 64's eight
  bytes, 5 an unsigned 64, 6 a float 32), and a version is an opaque bin
  compared only for equality;
- a job's value is its JSON as str;
- an SQL value is one of SQLite's five: nil, an integer, a float, str or bin;
- an event's fields and context are a JSON object as str;
- a series' samples are two bin columns, an int64 time and a float64 value a
  sample, 8 bytes each, little-endian, so that every value's bits come back;
- a search's document is its JSON as str, and a vector bin, four bytes a
  little-endian float a dimension.

### the server

The server's own methods take the range below the engines', `0x00xx`.

<!-- written from protocol/*.wire by `just protocol`: methods of server -->
| method   | name         | may   | request        | answer         |
|----------|--------------|-------|----------------|----------------|
| `0x0001` | server.stop  | admin | `Empty`        | `Empty`        |
| `0x0002` | server.clock | admin | `server.Clock` | `server.Clock` |
<!-- end of what `just protocol` writes -->

A stop is an admin connection's alone, `permission` to a data connection, and
a server whose program said nothing of stopping refuses it with `permission`
too: a program serving its own store decides when that store's server stops.
`tinystore serve` stops at one, as it does at Ctrl+C, which is how
`tinystore stop` ends a sidecar and how an SDK newer than the sidecar it found
replaces it with its own binary, as [Versions](#versions) says.

Only a private server started with `tinystore serve --stdio --clock <time>`
has a clock that moves, and the store runs on it instead of the system's
clock. An SDK starts such a server when you open a store with `private` and
`clock`. Only an admin connection can move the clock, and only forward. The
server answers `invalid` to a time before the clock's, both fields at once,
or a clock call to a server that runs on the system's clock.

### kv

Each open, of a bucket, counters, a rate limit, a quota or once's answers,
answers a handle, which every call on it carries. A handle holds kv's rows as
they are, so the server reads what any bucket wrote.

<!-- written from protocol/*.wire by `just protocol`: methods of kv -->
| method   | name               | may   | request            | answer               |
|----------|--------------------|-------|--------------------|----------------------|
| `0x0101` | kv.bucket.open     | read  | `kv.BucketOpen`    | `Handle`             |
| `0x0102` | kv.get             | read  | `kv.Call`          | `kv.Entry`           |
| `0x0103` | kv.has             | read  | `kv.Call`          | `kv.Found`           |
| `0x0104` | kv.set             | write | `kv.Call`          | `kv.Written`         |
| `0x0105` | kv.create          | write | `kv.Call`          | `kv.Written`         |
| `0x0106` | kv.take            | write | `kv.Call`          | `kv.Entry`           |
| `0x0107` | kv.delete          | write | `kv.Call`          | `kv.Found`           |
| `0x0108` | kv.expire          | write | `kv.Call`          | `kv.Found`           |
| `0x0109` | kv.clear           | write | `kv.Branch`        | `Empty`              |
| `0x010a` | kv.list            | read  | `kv.List`          | `kv.Page`            |
| `0x0110` | kv.counters.open   | read  | `kv.CountersOpen`  | `Handle`             |
| `0x0111` | kv.counters.add    | write | `kv.Call`          | `kv.Count`           |
| `0x0112` | kv.counters.get    | read  | `kv.Call`          | `kv.Count`           |
| `0x0113` | kv.counters.delete | write | `kv.Call`          | `kv.Found`           |
| `0x0114` | kv.counters.clear  | write | `kv.Branch`        | `Empty`              |
| `0x0120` | kv.rateLimit.open  | read  | `kv.RateLimitOpen` | `Handle`             |
| `0x0121` | kv.quota.open      | read  | `kv.QuotaOpen`     | `Handle`             |
| `0x0122` | kv.allow           | write | `kv.Call`          | `kv.Allowance`       |
| `0x0123` | kv.peek            | read  | `kv.Call`          | `kv.Allowance`       |
| `0x0124` | kv.reset           | write | `kv.Call`          | `Empty`              |
| `0x0125` | kv.refund          | write | `kv.Call`          | `Empty`              |
| `0x0130` | kv.once.open       | read  | `kv.OnceOpen`      | `Handle`             |
| `0x0131` | kv.once.run        | write | `kv.Call`          | handover `kv.Answer` |
| `0x0132` | kv.once.get        | read  | `kv.Call`          | `kv.Answer`          |
| `0x0133` | kv.once.delete     | write | `kv.Call`          | `kv.Found`           |
| `0x0140` | kv.tx              | write | `kv.Tx`            | `kv.TxResults`       |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/kv.wire`. A `kv.tx` checks every read it
carries and applies every write, all or none; one that fails names its place
in `what`, as `check` or `write` and its index. `kv.once.run` lets one run of
a key go at a time, every client's: another run of the key waits until that
run ends, then is answered what it kept, or is handed the run when it kept
nothing. A client that is handed a run, an answer not found as its
`RESPONSE`, runs its function and sends a `kv.Answer` as its last `DATA`:
found with the value to keep, or not found when the function failed, which
keeps nothing. A client that cancels or leaves keeps nothing either, and the
next run of the key is handed over again.

`kv.get` of `token` under the owner `7`, on handle 1, and its answer:

```text
0e 00 00 00  03  01  02 01  01 00 00 00                                   a body of 14 bytes, REQUEST, END, 0x0102, stream 1
83                                                                        a map of three
   01 01                                                                  handle: 1
   02 91 a1 37                                                            under: ["7"]
   03 a5 74 6f 6b 65 6e                                                   key: "token"

1a 00 00 00  04  01  00 00  01 00 00 00                                   a body of 26 bytes, RESPONSE, END, stream 1
84                                                                        a map of four
   01 c3                                                                  found: true
   02 c4 06 01 68 65 6c 6c 6f                                             value: bin 01 "hello", the text "hello"
   03 c4 01 31                                                            version: bin "1"
   04 cf 00 00 01 a0 c4 50 6c 00                                          expiresAt: 1,790,000,000,000
```

### jobs

`jobs.queue.open` answers a handle on a queue of JSON values, and
`jobs.schedule.open` one on a schedule, the queue of one repeating job under
its name; every other call carries it.

<!-- written from protocol/*.wire by `just protocol`: methods of jobs -->
| method   | name               | may   | request             | answer                                 |
|----------|--------------------|-------|---------------------|----------------------------------------|
| `0x0201` | jobs.queue.open    | read  | `jobs.QueueOpen`    | `Handle`                               |
| `0x0202` | jobs.schedule.open | write | `jobs.ScheduleOpen` | `Handle`                               |
| `0x0203` | jobs.add           | write | `jobs.Call`         | `jobs.Changed`                         |
| `0x0204` | jobs.set           | write | `jobs.Call`         | `Empty`                                |
| `0x0205` | jobs.update        | write | `jobs.Call`         | `jobs.Changed`                         |
| `0x0206` | jobs.cancel        | write | `jobs.Id`           | `jobs.Changed`                         |
| `0x0207` | jobs.get           | read  | `jobs.Id`           | `jobs.Job`                             |
| `0x0208` | jobs.list          | read  | `jobs.List`         | `jobs.Page`                            |
| `0x0209` | jobs.work          | write | `jobs.Work`         | exchange `jobs.Held` for `jobs.Answer` |
| `0x020a` | jobs.step          | write | `jobs.Step`         | `jobs.Kept`                            |
| `0x020b` | jobs.keep          | write | `jobs.Step`         | `Empty`                                |
| `0x020c` | jobs.watch         | read  | `jobs.Id`           | download `jobs.Job` until `Empty`      |
| `0x020d` | jobs.tx            | write | `jobs.Tx`           | `jobs.TxResults`                       |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/jobs.wire`. A `jobs.work` stream hands the
client's worker each job as a `jobs.Held`, and the client answers it with a
`jobs.Answer` on the same stream: how its run ended, how far it got, which
settles nothing, or a stop, which takes no job more. The client ends its side
with `DATA`·END once it takes no more. When that side ends, or the client
cancels or leaves, the jobs in its hands fail that attempt, as a worker's
whose process died, and the server's `DATA`·END, `{}`, follows once that is
written. `jobs.watch` sends the job as it is now, then again as it changes,
and ends after the job ends. `jobs.step` asks for the answer a step of a held
job's run kept, and `jobs.keep` keeps one, so that the attempt after a retry
or a lost worker gets it back without running the step again.

### sql

`sql.open` answers a handle on a database by its name, its migrations applied
and checked; a read connection's applies none and chooses no durability, and
opens the file as it is.

<!-- written from protocol/*.wire by `just protocol`: methods of sql -->
| method   | name        | may   | request         | answer                                     |
|----------|-------------|-------|-----------------|--------------------------------------------|
| `0x0301` | sql.open    | read  | `sql.Open`      | `Handle`                                   |
| `0x0302` | sql.query   | read  | `sql.Query`     | download `sql.Rows` until `sql.Rows`       |
| `0x0303` | sql.exec    | write | `sql.Statement` | `sql.Done`                                 |
| `0x0304` | sql.batch   | write | `sql.Batch`     | `sql.Batched`                              |
| `0x0305` | sql.tx      | write | `sql.TxOpen`    | exchange `sql.TxAnswer` for `sql.TxCall`   |
| `0x0306` | sql.columns | read  | `sql.Statement` | download `sql.Columns` until `sql.Columns` |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/sql.wire`. A query's rows come in its
`RESPONSE` when they fit one message, and otherwise as `DATA` parts within
the client's credit. A row a query downloads past the agreed body is `limit`.
A batch's results travel in one message, and past the agreed body they are
`outcome_unknown`, since the batch committed. A query holds its rows in the
server's memory, 64 MiB of them at most, before the first leaves. A
`sql.tx` holds its database's writer while the client's calls come as `DATA`,
and a transaction whose client stalls, cancels or leaves is rolled back.

### blobs

`blobs.open` answers a handle on a bucket of files, which every other call
carries.

<!-- written from protocol/*.wire by `just protocol`: methods of blobs -->
| method   | name         | may   | request        | answer                                     |
|----------|--------------|-------|----------------|--------------------------------------------|
| `0x0401` | blobs.open   | read  | `blobs.Open`   | `Handle`                                   |
| `0x0402` | blobs.put    | write | `blobs.Write`  | `blobs.Written`                            |
| `0x0403` | blobs.upload | write | `blobs.Write`  | exchange `blobs.Written` for `blobs.Piece` |
| `0x0404` | blobs.get    | read  | `blobs.At`     | `blobs.Got`                                |
| `0x0405` | blobs.head   | read  | `blobs.At`     | `blobs.Head`                               |
| `0x0406` | blobs.delete | write | `blobs.Delete` | `Empty`                                    |
| `0x0407` | blobs.copy   | write | `blobs.Move`   | `blobs.Info`                               |
| `0x0408` | blobs.rename | write | `blobs.Move`   | `blobs.Info`                               |
| `0x0409` | blobs.expire | write | `blobs.Expire` | `blobs.Found`                              |
| `0x040a` | blobs.list   | read  | `blobs.List`   | `blobs.Page`                               |
| `0x040b` | blobs.usage  | read  | `blobs.Folder` | `blobs.Usage`                              |
| `0x040c` | blobs.clear  | write | `blobs.Folder` | `Empty`                                    |
| `0x040d` | blobs.read   | read  | `blobs.Read`   | download `blobs.Piece` until `Empty`       |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/blobs.wire`. `blobs.upload` takes a file's
bytes as `DATA` pieces, its last publishing the file, and `blobs.read` sends
them back, the first piece in its `RESPONSE` and the rest as `DATA` within the
client's credit.

### records

`records.open` answers a handle on a stream of events, which every other call
carries.

<!-- written from protocol/*.wire by `just protocol`: methods of records -->
| method   | name               | may   | request                   | answer                                         |
|----------|--------------------|-------|---------------------------|------------------------------------------------|
| `0x0501` | records.open       | read  | `records.Open`            | `Handle`                                       |
| `0x0502` | records.append     | write | `records.Append`          | `Empty`                                        |
| `0x0503` | records.list       | read  | `records.Query`           | `records.Page`                                 |
| `0x0504` | records.count      | read  | `records.Query`           | `records.Count`                                |
| `0x0505` | records.top        | read  | `records.TopAsked`        | `records.Tops`                                 |
| `0x0506` | records.aggregate  | read  | `records.AggregateAsked`  | `records.Rows`                                 |
| `0x0507` | records.streams    | read  | `Empty`                   | `records.Streams`                              |
| `0x0508` | records.fieldNames | read  | `records.FieldNamesAsked` | `records.Names`                                |
| `0x0509` | records.poll       | read  | `records.PollAsked`       | `records.Batch`                                |
| `0x050a` | records.tail       | read  | `records.Query`           | download `records.TailBatch` until `Empty`     |
| `0x050b` | records.lines      | write | `Handle`                  | exchange `records.Dropped` for `records.Piece` |
| `0x050c` | records.damaged    | read  | `Empty`                   | `records.Damages`                              |
| `0x050d` | records.drop       | write | `records.Damage`          | `Empty`                                        |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/records.wire`. `records.tail` sends a batch
each time events arrive that its query finds. `records.lines` takes another
program's output as `DATA`, cut anywhere, and its last answers with what the
writer dropped.

### metrics

Metrics are one store of series, each named by its labels, so none opens a
handle.

<!-- written from protocol/*.wire by `just protocol`: methods of metrics -->
| method   | name                     | may   | request                  | answer                                                   |
|----------|--------------------------|-------|--------------------------|----------------------------------------------------------|
| `0x0601` | metrics.read             | read  | `metrics.Query`          | download `metrics.Read` until `metrics.Read`             |
| `0x0602` | metrics.latest           | read  | `metrics.Query`          | download `metrics.Latest` until `metrics.Latest`         |
| `0x0603` | metrics.aggregate        | read  | `metrics.AggregateAsked` | download `metrics.Aggregates` until `metrics.Aggregates` |
| `0x0604` | metrics.explain          | read  | `metrics.AggregateAsked` | `metrics.Plan`                                           |
| `0x0605` | metrics.names            | read  | `metrics.PageAsked`      | `metrics.NamePage`                                       |
| `0x0606` | metrics.labelNames       | read  | `metrics.PageAsked`      | `metrics.TextPage`                                       |
| `0x0607` | metrics.labelValues      | read  | `metrics.PageAsked`      | `metrics.TextPage`                                       |
| `0x0608` | metrics.description      | read  | `metrics.NameAsked`      | `metrics.Description`                                    |
| `0x0609` | metrics.damaged          | read  | `Empty`                  | `metrics.Damages`                                        |
| `0x060a` | metrics.ingest           | write | `metrics.Ingest`         | `Empty`                                                  |
| `0x060b` | metrics.describe         | write | `metrics.Description`    | `Empty`                                                  |
| `0x060c` | metrics.delete           | write | `metrics.SeriesNamed`    | `metrics.Deleted`                                        |
| `0x060d` | metrics.flush            | write | `Empty`                  | `Empty`                                                  |
| `0x060e` | metrics.instruments      | write | `metrics.Session`        | exchange `metrics.Ack` for `metrics.Batch`               |
| `0x060f` | metrics.ingestPrometheus | write | `metrics.Exposition`     | `Empty`                                                  |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/metrics.wire`. A read, the newest samples and an
aggregate answer in parts within the client's credit, a series longer than a
part cut across parts. `metrics.instruments` takes a client's batches on a
stream of their own, applied in turn, each answered with the last batch of
the session the store applied.

### config

<!-- written from protocol/*.wire by `just protocol`: methods of config -->
| method   | name          | may   | request         | answer                                |
|----------|---------------|-------|-----------------|---------------------------------------|
| `0x0701` | config.open   | read  | `config.Open`   | `config.Value`                        |
| `0x0702` | config.follow | read  | `Handle`        | download `config.Value` until `Empty` |
| `0x0703` | config.update | write | `config.Update` | `config.Value`                        |
| `0x0704` | config.reset  | write | `config.Reset`  | `config.Value`                        |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/config.wire`. `config.follow` sends the
config's value now and again each time it changes, until the client cancels
it.

### backup

<!-- written from protocol/*.wire by `just protocol`: methods of backup -->
| method   | name         | may   | request        | answer                                         |
|----------|--------------|-------|----------------|------------------------------------------------|
| `0x0801` | backup.write | admin | `backup.Write` | download `backup.Piece` until `backup.Written` |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/backup.wire`. Only an admin connection can ask
for a backup, since it holds every byte of the store. The server sends the zip
of every engine's files in pieces, as it writes them, within the client's
credit, then what it holds. `include` names files of the application's by
their paths inside the store's directory, which the zip holds beside the
engines'.

### locks

<!-- written from protocol/*.wire by `just protocol`: methods of locks -->
| method   | name          | may   | request       | answer       |
|----------|---------------|-------|---------------|--------------|
| `0x0901` | locks.hold    | write | `locks.Hold`  | `locks.Held` |
| `0x0902` | locks.renew   | write | `locks.Renew` | `Empty`      |
| `0x0903` | locks.release | write | `locks.Lease` | `Empty`      |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/locks.wire`. A hold is answered once its key is
given, from whichever thread lets it go, and a client that cancels it before
is answered `cancelled`.

### channels

<!-- written from protocol/*.wire by `just protocol`: methods of channels -->
| method   | name               | may   | request              | answer                                  |
|----------|--------------------|-------|----------------------|-----------------------------------------|
| `0x0a01` | channels.publish   | write | `channels.Publish`   | `Empty`                                 |
| `0x0a02` | channels.subscribe | read  | `channels.Subscribe` | download `channels.Batch` until `Empty` |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/channels.wire`. A subscription answers once the
store has it, then sends its batches within the client's credit. A client
that grants nothing falls behind and misses messages, and the next batch says
how many it missed: the log is bounded, and neither the publisher nor the
server waits for it.

### search

An index opens a handle, as a bucket does; its documents are JSON as str, and
a vector is bin, its floats little-endian, four bytes a dimension.

<!-- written from protocol/*.wire by `just protocol`: methods of search -->
| method   | name             | may   | request          | answer           |
|----------|------------------|-------|------------------|------------------|
| `0x0b01` | search.open      | read  | `search.Open`    | `Handle`         |
| `0x0b02` | search.set       | write | `search.Set`     | `Empty`          |
| `0x0b03` | search.setMany   | write | `search.SetMany` | `Empty`          |
| `0x0b04` | search.delete    | write | `search.Key`     | `search.Deleted` |
| `0x0b05` | search.get       | read  | `search.Key`     | `search.Got`     |
| `0x0b06` | search.search    | read  | `search.Query`   | `search.Found`   |
| `0x0b07` | search.status    | read  | `Handle`         | `search.Status`  |
| `0x0b08` | search.dropIndex | write | `search.Name`    | `search.Dropped` |
| `0x0b09` | search.list      | read  | `Empty`          | `search.Listed`  |
<!-- end of what `just protocol` writes -->

Its messages are in `protocol/search.wire`. A set on an index with vectors
gives `vector`, or `noVector: true` for a document only its words find; one
that says neither is `invalid`. A read connection opens an index only as it
is.

A table's index opens with `database`, a handle `sql.open` gave, and
`table`, with `key` and `vectorColumn` where it needs them: its fields and
filters are columns of the table. Its search answers `columns` once and
each hit's `row`, a cell a column, as `sql.Rows` carries rows, and `value`
null. A `set`, `setMany`, `delete` or `get` on it is `invalid`, since the
table owns the rows, and `dropIndex` names the database beside the name.

#### search's where

A search's `where` is a JSON list of conditions, every one of which a
document passes, as the Bun SDK writes its `where` object:

| condition                                       | passes a document that                                |
|-------------------------------------------------|-------------------------------------------------------|
| `{"eq": [path, value]}`                         | holds the value, or a list holding it                 |
| `{"in": [path, [values]]}`                      | holds one of them; an empty list passes none          |
| `{"missing": path}`                             | holds no value there: no field, null or an empty list |
| `{"range": [path, {"gte": 10, "lt": 100}]}`     | holds a number within it: `gt`, `gte`, `lt` and `lte` |
| `{"or": [conditions]}`, `{"and": [conditions]}` | passes any, or every, of them                         |

A value is text, a number or a boolean: the text "50" never passes for the
number 50, and an integer past 2⁵³ is written as its digits and matched
exactly. A path the index was not opened with is `invalid`.
