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
[server/wire/testdata/vectors.json](../server/wire/testdata/vectors.json),
which the server and every SDK are tested against: a value in a typed
notation, the bytes it is, and the bytes a decoder refuses, each named after
the rule it breaks. Every message is also a vector of
[messages.json](../server/wire/testdata/messages.json), its bytes and its
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

| kind | | stream | method | body | flags |
|---|---|---|---|---|---|
| 1 | `HELLO` | 0 | 0 | a message | — |
| 2 | `WELCOME` | 0 | 0 | a message | — |
| 3 | `REQUEST` | the new stream | the operation | a message | END |
| 4 | `RESPONSE` | its stream | 0 | a message, or an error | END, ERROR |
| 5 | `DATA` | its stream | 0 | bytes, or a message, as the method says | END, ERROR |
| 6 | `CANCEL` | its stream | 0 | empty | — |
| 7 | `CREDIT` | its stream, or 0 for the connection | 0 | u32: bytes granted | — |
| 8 | `PING` | 0 | 0 | 8 bytes | — |
| 9 | `PONG` | 0 | 0 | the `PING`'s 8 bytes | — |
| 10 | `GOAWAY` | 0 | 0 | a message | — |

END is 1: the sender's last frame on its stream. ERROR is 2, only beside END:
the body is [an error](#errors).

A method's high byte is its engine and its low byte the operation, so a new
engine takes a new high byte and the frame stays as it is:

| high byte | engine |
|---|---|
| `0x01` | kv |
| `0x02` | jobs |
| `0x03` | blobs |
| `0x04` | sql |
| `0x05` | records |
| `0x06` | metrics |

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

| key | field | type | |
|---|---|---|---|
| 1 | protocol | uint | the newest the client speaks, 1 |
| 2 | client | str | a name and version, for logs |
| 3 | token | str | required on TCP |
| 4 | max body | uint | the largest body the client takes; the server's when absent |
| 5 | stream credit | uint | the `DATA` the server may send on a stream before the client grants more; 2 MiB when absent |
| 6 | challenge | bin | 16 random bytes, when the client found the server through `SERVE` |

A body must fit the credit it is sent under, so the max body both sides agree
is the smallest of the server's, the client's and the client's stream
credit: a client granting 64 KiB a stream takes bodies of 64 KiB at most.

`WELCOME`:

| key | field | type | |
|---|---|---|---|
| 1 | protocol | uint | the one this connection speaks |
| 2 | server | str | its version |
| 3 | instance | bin | 16 random bytes a start |
| 4 | capability | str | `admin` or `data` |
| 5 | max body | uint | the largest body either side sends |
| 6 | in flight | uint | the streams a client may have open at once |
| 7 | connection credit | uint | the bytes of `REQUEST` and `DATA` bodies a client may send before credit comes back; at least the max body |
| 8 | stream credit | uint | the bytes of `DATA` a client may send on a stream before credit comes back |
| 9 | engines | array of str | what this server serves |
| 10 | now | int | the store's clock, unix milliseconds |
| 11 | proof | bin | on a local connection whose `HELLO` carried a challenge: the HMAC-SHA256 of the challenge, keyed with the 32 bytes of `SERVE`'s secret |

A client that found the server through `SERVE` sends no `REQUEST` before the
proof checks, compared in constant time: only the store directory's owner can
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

| key | field | type | |
|---|---|---|---|
| 1 | code | str | as [an error's](#errors) |
| 2 | message | str | |

## Finding a local server

A store directory has at most one server, since its `LOCK` lets one process
open it. That server publishes how to reach it in `<dir>/server/`, which only
the directory's owner may enter: mode 0700 and, on Windows, which lets anyone
traverse a directory to a file whose path they know, a protected DACL naming
its owner alone, which every file made in it inherits. `SERVE` there is a
hint; `LOCK` is the truth.

```json
{"protocol": 1, "server": "0.4.0", "pid": 4212, "instance": "q1q0l3yZ3JvYw8g7oM2sYA",
 "secret": "Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE",
 "endpoints": ["unix:///srv/app/data/server/tinystore.sock"], "sidecar": true}
```

| field | |
|---|---|
| protocol | the newest protocol the server speaks |
| server | its version |
| pid | its process, for a person reading the file; no client trusts it, since a pid is reused |
| instance | 16 random bytes a start, base64url without padding, which `WELCOME` repeats |
| secret | 32 random bytes a start, base64url without padding, the key of the proof |
| endpoints | where it listens: `unix://` and a socket's path, or `pipe:` and a Windows named pipe's name |
| sidecar | `true` when the server is a sidecar, which `tinystore serve --local` starts for its clients. A client of a newer release may replace a sidecar. The field is absent for a Go program's own server and for `tinystore serve <dir>`, which a person runs |

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
  once its server is gone, a Windows pipe's above all, and only the
  directory's owner can read the secret. A client whose proof fails reads
  `SERVE` again, as a stale one.
- **A reader delays a change and never fails it.** On Windows a client
  holding `SERVE` open refuses its rename or removal, and so may a scanner;
  the server tries again eight times, from 1 to 64 ms apart.

How a client knows its server depends on the transport:

| transport | how the client knows its server |
|---|---|
| stdio | it started the server itself, as a private child |
| a Unix socket or a named pipe found through `SERVE` | the proof |
| TCP with TLS | the certificate, checked before its token leaves |
| TCP | not at all: the token travels in the clear, on a network its operator trusts |

Where a local server listens:

- **A Unix socket**, `<dir>/server/tinystore.sock`, when that absolute path
  fits a `sockaddr_un`, 104 bytes on macOS and the BSDs and 108 on Linux;
  past it, the same name in a directory of the owner's own,
  `$XDG_RUNTIME_DIR/tinystore-<hash>`, or in the temporary directory without
  one. `<hash>` is the first eight bytes of the SHA-256 of the store's absolute
  path, in hex: eight, so that a socket under macOS's temporary directory
  still fits.
- **A named pipe on Windows**, `pipe:tinystore-<hash>`, created as the first
  instance of its name with a DACL naming its owner alone, which refuses
  remote clients. No local endpoint takes a token: its permission is the file
  system's, and a local connection is `admin`.

## Streams

A client names a stream with any number from 1 not in use. A number is in use
from its `REQUEST` to the server's final frame on it, and may be named again
after. A `REQUEST` on a number in use, or past the streams in flight, ends the
connection.

| shape | the client sends | the server sends |
|---|---|---|
| a call | `REQUEST`·END | `RESPONSE`·END |
| an upload | `REQUEST`, `DATA`…, `DATA`·END | `RESPONSE`·END |
| a download | `REQUEST`·END | `RESPONSE`, `DATA`…, `DATA`·END |
| both ways | `REQUEST`, `DATA`…, `DATA`·END | `RESPONSE`, `DATA`…, `DATA`·END |

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
  `CANCEL`: ERROR is the server's alone.
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
  deeper than 8 is invalid; a body is at most the agreed maximum.
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

| key | field | type | |
|---|---|---|---|
| 1 | code | str | below |
| 2 | message | str | what failed, as the engine's error says it |
| 3 | what | a map of names | the item it names: a bucket and key, a queue and key, a series' labels, a table and constraint |

| code | means | sent again |
|---|---|---|
| `invalid` | the request cannot be done as asked | no |
| `limit` | a bound: memory, size or count; `what` names the `limit`, what the call `wanted` of it and the `bound`, where the engine knows them | later, or smaller |
| `closed` | the store or the handle closed | after opening again |
| `in_use` | a name is taken | no |
| `conflict` | a condition or a version no longer holds | after reading again |
| `corrupt` | stored bytes no longer read | no |
| `too_old`, `too_new` | a time outside its engine's window | no |
| `suspended` | a series in quarantine | after its repair |
| `outcome_unknown` | a commit whose result is unknown | after reading what it wrote |
| `permission` | the connection's capability does not allow it | no |
| `unimplemented` | a method, or a field of a request, this server does not have: `what` names the field, the message the server's version | no |
| `cancelled` | the client cancelled it | — |
| `unavailable` | the server is closing | on another connection |
| `internal` | a fault of the server's | — |

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

Each engine's messages are fixed on this page with its slice, before its
code: numbers, keys, types, shapes and vectors. A call answers with one
message; whatever returns a collection is a download, its items `DATA` under
credit and its last `DATA` saying where the next page begins. What each will
carry:

| engine | operations | shape |
|---|---|---|
| the server | stop | a call |
| kv | open, get, has, set, delete, take, touch, add, max, clear, batch, view, allow, configure | calls |
| | scan | a download, an entry a message |
| | watch | a download that does not end: a config's kept fields, again after each change |
| | run | both ways: the answer a key keeps, or the key's run handed over and its answer back |
| | usage, refund | calls on a quota, which allow uses |
| jobs | open, enqueue of many, update, cancel, get, claim, settle of many, step, keep | calls |
| | scan | a download, an entry a message |
| | watch | a download that ends with its job: its entry, again after each change |
| | work | both ways: jobs out, their outcomes back |
| blobs | open, stat, delete, copy, move, usage, clear | calls |
| | scan | a download, an object a message |
| | put | an upload |
| | get, with an offset and a length | a download; a whole read whose bytes do not match its hash ends with ERROR and `corrupt` instead of END |
| sql | open, with its migration files; exec; batch | calls |
| | query | a download: the columns, then rows a message at a time |
| records | append of many, drop | calls |
| | read, follow | a download, records a message at a time |
| | lines | an upload of another program's output |
| metrics | ingest, drop | calls |
| | read, aggregate | a download, a series a message, or several for a series longer than a body holds |

The values they carry:

- the server's own call carries nothing, `{}` each way;
- a kv value is nil, an integer or bin, as its row keeps it, and a version is
  an opaque bin compared only for equality;
- a job's value is its JSON as str;
- an SQL value is one of SQLite's five: nil, an integer, a float, str or bin;
- a record's attribute keeps its JSON spelling as str;
- a series' samples, and an aggregate's buckets and values, are bin columns.

### the server

The server's own methods take the range below the engines', `0x00xx`.

| method | | request | answer |
|---|---|---|---|
| `0x0001` | stop | `{}` | `{}`, then the server stops as its closing does: the streams running finish, every connection is told `GOAWAY`, and a sidecar gives back `SERVE` and its directory |
| `0x0002` | clock | a clock: a time to set it to, a while to move it forward by, or neither to read it | a clock: the time it reads once moved |
| `0x0003` | backup | `{}` | a download: `{}`, then the zip of the whole store in `DATA`, its bytes as they come, the last with END |

A stop is an admin connection's alone, `permission` to a data connection, and
a server whose program said nothing of stopping refuses it with `permission`
too: a program serving its own store decides when that store's server stops.
`tinystore serve` stops at one, as it does at Ctrl+C, which is how
`tinystore stop` ends a sidecar and how an SDK newer than the sidecar it found
replaces it with its own binary, as [Versions](#versions) says.

Only an admin connection can ask for a backup. The server first opens each
engine whose file is in the directory. It copies each database that no client
has opened, without opening it. Then it sends what the `backup` package
writes: a copy of every engine's file, and a manifest of their sizes and
checksums, which a restore checks.

A clock is `{1: at, 2: advance}`. `at` is a time in unix milliseconds, and
`advance` is a duration in milliseconds. Only a private server started with
`tinystore serve --stdio --clock <time>` has a clock that moves, and the store
runs on it instead of the system's clock. An SDK starts such a server when you
open a store with `private` and `clock`. Only an admin connection can move the
clock, and only forward. The server answers `invalid` to a time before the
clock's, a negative duration, both fields at once, or a clock call to a server
that runs on the system's clock.

### kv

`kv.open` answers a handle on a bucket of values, on counters, on a config, on
a limiter, on once's answers or on a quota, and every other call carries it. A handle holds `kv.Raw` values, so the server reads
what any bucket wrote.

| method | | request | answer |
|---|---|---|---|
| `0x0101` | open | a bucket | a handle |
| `0x0102` | get | a call | an entry: found, value, version, expires; on counters the value, 0 when absent |
| `0x0103` | has | a call | an entry: found |
| `0x0104` | set | a call with its value | an entry: found, version, expires; with if absent, found is false when a live key was there, and the entry is that key's |
| `0x0105` | delete | a call | `{}` |
| `0x0106` | take | a call | an entry: found, value |
| `0x0107` | touch | a call with ttl or expire at | an entry: found |
| `0x0108` | add | a call with n | an entry: the counter's value |
| `0x0109` | max | a call with n | an entry: the counter's value |
| `0x010a` | clear | a call naming a branch | `{}` |
| `0x010b` | batch | calls, in one transaction | results; a call that fails fails them all, and `what` names it as `call` |
| `0x010c` | view | calls, get and has, from one snapshot | results |
| `0x010d` | scan | a call naming a branch, after and limit | a download: `{}`, an entry a `DATA` with its key, a page |
| `0x010e` | allow | a call on a limiter or a quota: a key, and n requests or uses, 1 when absent | an allowance |
| `0x010f` | configure | a config's fields to keep and paths to forget | `{}` |
| `0x0110` | watch | a call on a config | a download that does not end: `{}`, then the kept fields a `DATA`, now and after each change |
| `0x0111` | run | a call on once's answers, whose REQUEST leaves the client's side open | the answer kept, found, which ends the stream; or not found, the run handed over: the client's last `DATA` is the entry to keep, and the server's, `{}`, follows once it is kept |
| `0x0112` | usage | a call on a quota: a key | an allowance, nothing used: ok says one more use would pass |
| `0x0113` | refund | a call on a quota: a key, and n uses, 1 when absent | `{}` |

A bucket:

| key | field | type | |
|---|---|---|---|
| 1 | name | str | `[a-z0-9][a-z0-9_-]{0,63}` |
| 2 | counters | bool | counters rather than values |
| 3 | default ttl | uint | milliseconds |
| 4 | sliding | uint | milliseconds; values alone |
| 5 | lose at most | uint | milliseconds; counters alone |
| 6 | config | bool | a config rather than values |
| 7 | rate | uint | a limiter's requests every per; a bucket with a rate is a limiter |
| 8 | per | uint | milliseconds |
| 9 | burst | uint | the requests a limiter lets through at once; rate when absent |
| 10 | once | bool | the answers `kv.run` keeps, a day unless default ttl says; get and delete read and forget one |
| 11 | windows | array of windows | a quota's, one to eight: a bucket with windows is a quota, which delete also takes |

A window is `{1: name, 2: limit, 3: per}`: a key may use up to limit every per
milliseconds from its first use, the name `[a-z][a-z0-9_]{0,31}`.

A handle is `{1: uint}`. A call:

| key | field | type | |
|---|---|---|---|
| 1 | handle | uint | |
| 2 | owners | array of keys | the branch below the bucket's root; absent for the root |
| 3 | key | a key | |
| 4 | value | nil, int or bin | nil when absent |
| 5 | ttl | uint | milliseconds |
| 6 | expire at | int | unix milliseconds |
| 7 | if version | bin | a version that an entry carried. A get or a has with it fails `conflict` if the key no longer has that version |
| 8 | if absent | bool | A get or a has with it fails `conflict` if the key holds a value |
| 9 | n | int | what add adds, and max compares |
| 10 | after | a key | the key a scan's page begins after |
| 11 | limit | uint | the keys a page returns: 100 when absent, 1000 at most |

A key is text: a str, a bin, or an integer, which is its decimal spelling, so
that `42` and `"42"` name one key. The server writes a key as str, or as bin
when it is not UTF-8. An entry:

| key | field | type | |
|---|---|---|---|
| 1 | found | bool | |
| 2 | value | nil, int or bin | |
| 3 | version | bin | |
| 4 | expires | int | unix milliseconds; absent for a key that never expires |
| 5 | key | a key | a scan's item alone |

A limiter answers `kv.allow` with an allowance, `{1: ok, 2: left, 3: retry
after}`: whether the requests pass, how many more would pass now, and how many
milliseconds until they would, rounded up, when they do not. More requests
than the burst at once are `invalid`. A quota's allowance adds `4: windows`,
each `{1: name, 2: used, 3: limit, 4: left, 5: reset at}` in the order its
open gave them, reset at in unix milliseconds and absent for a window not
started; its `kv.allow` counts in every window or in none, and more uses than
a window's limit are `invalid`.

`kv.configure` is `{1: handle, 2: set, 3: reset}`, set an array of str, a path
and its JSON each, and reset the paths to forget; the server keeps both in one
transaction, checking only that a value is JSON within the config's bounds,
since the types are the client's. `kv.watch` answers `{}`, then `{1: changes,
2: fields}` a `DATA`, fields a path and its JSON each, now and after each
change, whoever made it: a watcher behind is sent the latest fields, never a
state the config did not have. It ends when the client cancels it, when the
client's side of the connection ends, or with the server.

`kv.run` lets one run of a key go at a time, every client's: another
`kv.run` of the key waits until that run ends, then is answered what it kept,
or is handed the run when it kept nothing. The client that is handed a run,
`{}` as its RESPONSE, runs its function and sends an entry as its last
`DATA`: found with the value to keep, or not found when the function failed,
which keeps nothing. A client that cancels or leaves keeps nothing either,
and the next run of the key is handed over again.

A page, a scan's trailer, is `{1: more, 2: after}`: more says the limit or
the page's 4 MiB of values ended it before the branch did, and after is the
key the next page begins after. Calls are `{1: [call…]}`, each call's map
holding its method under key 0, and results `{1: [entry…]}`.

`kv.get` of `token` under the owner `7`, on handle 1, and its answer:

```text
0e 00 00 00  03  01  02 01  01 00 00 00                                   a body of 14 bytes, REQUEST, END, 0x0102, stream 1
83                                                                        a map of three
   01 01                                                                  handle: 1
   02 91 a1 37                                                            owners: ["7"]
   03 a5 74 6f 6b 65 6e                                                   key: "token"

19 00 00 00  04  01  00 00  01 00 00 00                                   a body of 25 bytes, RESPONSE, END, stream 1
84                                                                        a map of four
   01 c3                                                                  found: true
   02 c4 05 68 65 6c 6c 6f                                                value: bin "hello"
   03 c4 01 31                                                            version: bin "1"
   04 cf 00 00 01 a0 c4 50 6c 00                                          expires: 1,790,000,000,000
```

### jobs

`jobs.open` answers a handle on a queue of JSON values, or on a schedule,
the queue of one repeating job under its name; every other call carries it.
A job's value is its JSON as str, checked before it is kept; a schedule's is
`{}`.

| method | | request | answer |
|---|---|---|---|
| `0x0201` | open | a queue | a handle |
| `0x0202` | enqueue | a batch: jobs one transaction adds, all or none | `{}`; `what` names a refused job as `call` |
| `0x0203` | update | a change: a job by its key, a new value, time or repeat | `{}` |
| `0x0204` | cancel | a key | an entry: found says there was a job, which a running one's handler is told |
| `0x0205` | get | a key | an entry |
| `0x0206` | claim | a lease | a held job, found false when none was due |
| `0x0207` | settle | outcomes of claimed jobs, written in one group | settled: nil or an error each |
| `0x0208` | scan | a query | a download: `{}`, an entry a `DATA`, a page |
| `0x0209` | work | workers | both ways: `{}`, then held jobs out and outcomes back, `DATA`·END each side |
| `0x020a` | watch | a key | a download: `{}`, then an entry a `DATA`, now and after each change, until the job ends |
| `0x020b` | step | an answer's job and name | kept: the answer the job's run kept under the name, found false for none |
| `0x020c` | keep | an answer | `{}` once the answer is written, the attempt holding the job's lease |

A queue:

| key | field | type | |
|---|---|---|---|
| 1 | name | str | `[a-z0-9][a-z0-9_-]{0,63}` |
| 2 | lease | uint | milliseconds; 30 seconds when absent |
| 3 | max attempts | uint | 10 when absent |
| 4 | backoff first | uint | milliseconds, the first wait after a failure; a second when absent |
| 5 | backoff most | uint | milliseconds, the longest; an hour when absent |
| 6 | max waiting | uint | ten million when absent |
| 7 | keep failed | uint | milliseconds; seven days when absent |
| 8 | keep done | uint | milliseconds; absent forgets a key when its job is done |
| 9 | schedule | a repeat | a schedule rather than a queue |
| 10 | max running | uint | the jobs that may run at once, across every worker of the store; absent bounds none |
| 11 | in | str | a database's name. The queue lives in that database's file instead of `jobs.db`, and the database's batches can enqueue on it. A client opens the database first, on any connection. Absent means `jobs.db` |

A repeat is `{1: cron, 2: zone}`, five cron fields and the zone's name, or
`{3: every}`, milliseconds, at least a second. A job, a batch's item:

| key | field | type | |
|---|---|---|---|
| 1 | value | str | its JSON |
| 2 | key | str | 1 to 1024 bytes |
| 3 | at | int | unix milliseconds; a time past runs now |
| 4 | after | uint | milliseconds from now |
| 5 | repeat | a repeat | needs a key |

A batch is `{1: handle, 2: [job…]}`; a change is a job's fields with the
handle under key 6; a key is `{1: handle, 2: key}`. An entry:

| key | field | type | |
|---|---|---|---|
| 1 | found | bool | |
| 2 | key | str | |
| 3 | value | str | |
| 4 | at | int | unix milliseconds: the time it runs for |
| 5 | attempt | uint | its attempts, a running one included |
| 6 | state | uint | 1 waiting, 2 running, 3 failed, 4 done while keep done keeps its key, 5 cancelled, which only a watch sends |
| 7 | err | str | its last failure |
| 8 | repeat | str | a repeating job's cron text and zone, as jobs.db keeps it |
| 9 | ahead | uint | the jobs that run before a waiting one, up to 10,000; get and watch alone |
| 10 | progress | str | what a running job's worker last reported, as JSON |
| 11 | ran | int | unix milliseconds: when the last run a worker finished began |
| 12 | took | uint | milliseconds: how long that run took |

A job a work loop claimed ahead for a busy worker is waiting, 0 ahead, until
a worker has it. A lease is `{1: handle, 2: lease}`, milliseconds, the
queue's when absent. A held job is `{1: found, 2: job, 3: key, 4: value, 5:
at, 6: attempt, 7: cancelled}`, job being the number its outcome names: a
claim's lives on its connection until it is settled, and a work stream's on
its stream. An outcome:

| key | field | type | |
|---|---|---|---|
| 1 | job | uint | |
| 2 | how | uint | 1 ack, 2 retry, 3 fail for good, 4 snooze, 5 extend, 6 progress |
| 3 | err | str | why a retry or a failure |
| 4 | at | int | unix milliseconds: when a retry or a snooze runs again |
| 5 | after | uint | milliseconds: the same from now, or how long an extend holds; written when given, 0 too, which runs a retry or a snooze now where one without a time waits its backoff |
| 6 | progress | str | a progress's report, any JSON, which get and watch show until the job is settled |

A progress settles nothing: the job stays in its worker's hands, claimed or on
a work stream, and a report that is not JSON is `invalid`. The server keeps
the last one in memory, 4 KiB of JSON at most, a larger one dropped and
logged, so the SDKs refuse one past it before it leaves.

Outcomes are `{1: [outcome…]}` and settled `{1: [nil or error…]}`; settling a
claimed job a cancel took is `conflict`. A query is `{1: handle, 2: prefix, 3:
state, 4: after, 5: limit}`, state 1, 2 or 3, and a page `{1: more, 2:
after}`, as `jobs.Query` and `jobs.Page` are.

Workers are `{1: handle, 2: workers, 3: timeout, 4: until idle, 5: cancels}`:
the queue's own Work loop runs for the stream, `workers` at once, 1 when
absent and at most the streams in flight, each job `timeout` milliseconds in
the client's hands, a minute when absent, and with `until idle` only until no
job is due and none runs, as a test wants it. Each job it hands a handler goes
out as a held job; the client settles it by sending its outcome as `DATA` on
the same stream, and ends its side with `DATA`·END once it takes no more. An
extend is not one of those outcomes: the loop extends its leases itself, and
ends the stream `invalid` on one rather than take it as an ack. A progress is,
and leaves the job in the client's hands. When that side ends, or the client
cancels or leaves, the jobs in its hands fail that attempt, as a worker whose
process died fails it, and only after their handlers return does the loop
stop, so that the jobs it claimed ahead and never handed over go back
uncounted. The server's `DATA`·END, `{}`, follows.

A cancel that takes a job in the client's hands sends it again as `{2: job,
7: true}` on a stream that asked with `cancels`, so that its handler stops; a
stream that did not ask is told nothing. Either way the outcome the client
sends for that job settles nothing, and the stream goes on.

A held job's number is its connection's, the same for a claim's and a work
stream's, so that `jobs.step` and `jobs.keep` name either; a work stream's job
is settled on its stream alone, and `jobs.settle` of one is `invalid`. An
answer names a step of the held job's run:

| key | field | type | |
|---|---|---|---|
| 1 | job | uint | the held job's number |
| 2 | name | str | the step's, 1 to 256 bytes, within the run |
| 3 | answer | str | jobs.keep's: the step's answer as JSON, at most 1 MiB; jobs.step carries none |

and kept is `{1: found, 2: answer}`. A step kept by an attempt whose lease
another claim took is `conflict`, and nothing is kept; a run that ends takes
its steps along, and a repeating job's next run starts without them. A worker
asks `jobs.step` before it runs a step and sends `jobs.keep` once it has, so
that the attempt after a retry or a lost worker gets the answer back without
running the step again.

`jobs.watch` answers `{}`, then the job under the key as an entry a `DATA`,
now and each time its state, place, attempt, time, progress or error changes,
and ends after the entry in which the job ended: done, failed or cancelled. A
key that names no job ends it at once. A watcher behind a busy queue is sent
the latest entry, read at most five times a second, never every change. It
follows the job it found, so a later job under the key is another watch's,
and it also ends when the client cancels it, when the client's side of the
connection ends, or with the server.

### blobs

`blobs.open` answers a handle on a bucket; every other call carries it, the
owners a call names being the folder under the bucket's root it works in.

| method | | request | answer |
|---|---|---|---|
| `0x0301` | open | a bucket | a handle |
| `0x0302` | stat | a call | an object, found false for a key that holds none |
| `0x0303` | delete | a call, if match its one option | `{}` |
| `0x0304` | copy | a call from key to to, with the destination's options | the object written |
| `0x0305` | move | the same | the object written |
| `0x0306` | usage | a call naming a folder | a total |
| `0x0307` | clear | a call naming a folder | `{}` |
| `0x0308` | scan | a call naming a folder, prefix, after and limit | a download: `{}`, an object a `DATA`, a page |
| `0x0309` | put | a call with the object's options | an upload: its bytes as `DATA`, 64 KiB at most each; the object committed |
| `0x030a` | get | a call, with an offset and a length for a range | a download: the object, then its bytes as `DATA`; or the object alone, found false, with END |

A bucket is `{1: name, 2: default ttl, 3: max size}`, milliseconds and bytes.
A call:

| key | field | type | |
|---|---|---|---|
| 1 | handle | uint | |
| 2 | owners | array of str or int | the folder's segments |
| 3 | key | str | a path |
| 4 | to | str | copy's and move's destination |
| 5 | content type | str | absent keeps what a copy's source has |
| 6 | meta | a map of names | absent keeps it; present replaces all of it |
| 7 | ttl | uint | milliseconds |
| 8 | expire at | int | unix milliseconds |
| 9 | size | uint | a put's length: a stream that disagrees leaves nothing |
| 10 | if match | str | as an HTTP `If-Match` header spells it |
| 11 | if none match | bool | writes only where no live object is |
| 12, 13, 14 | prefix, after, limit | str, str, uint | a scan's |
| 15, 16 | offset, length | uint, uint | a get's range; a length absent reads to the end |

A method refuses an option it does not take, as the engine's call does. An
object:

| key | field | type | |
|---|---|---|---|
| 1 | found | bool | absent, and nothing else set, for a key that holds no live object |
| 2 | key | str | its path under the handle's folder |
| 3 | size | uint | |
| 4 | etag | str | quoted, as a header carries it |
| 5 | content type | str | |
| 6 | modified | int | unix milliseconds |
| 7 | expires | int | unix milliseconds; absent for an object that does not expire |
| 8 | meta | a map of names | |

A total is `{1: objects, 2: bytes}` and a page `{1: more, 2: after}`. A put
that does not commit leaves nothing: a client that cancels it, a stream
shorter or longer than its size, and a connection lost halfway. A get read
whole is checked against the object's hash, and one whose bytes changed ends
with `DATA`·END·ERROR and `corrupt` instead of its last bytes; a range is not
checked.

### sql

`sql.open` answers a handle on a database by its name, and every other call
carries it. The first open of a name in the server opens it: an admin
connection's applies the migrations it carries, and a data connection's finds
them applied, or is refused with `permission` and makes no file. A database
opens once a store, so every later open, and an open of a database the
embedding program passed the server, checks the migrations it carries against
those the file applied: one the file has not applied is `permission` for a
data connection and `in_use` for an admin one, since a database applies its
migrations as it opens, and one changed after it was applied is `invalid`. A
first open without migrations is `invalid`; a later one may carry none.

| method | | request | answer |
|---|---|---|---|
| `0x0401` | open | a database | a handle |
| `0x0402` | exec | a statement | done: the rows it changed and the rowid of the last it inserted |
| `0x0403` | query | a statement | a download: the columns, a row a `DATA`, then `{}` |
| `0x0404` | batch | statements one transaction runs, all or none, or with read, reads from one snapshot | results; a statement that fails fails them all, and `what` names it as `call` |

A database:

| key | field | type | |
|---|---|---|---|
| 1 | name | str | `[a-z0-9][a-z0-9_-]{0,63}`, its file `sql/<name>.db` |
| 2 | migrations | array of files | each `{1: name, 2: text}`: a `.sql` file's name, without a directory, and its text |

A statement:

| key | field | type | |
|---|---|---|---|
| 1 | handle | uint | absent in a batch, whose handle is its own |
| 2 | sql | str | |
| 3 | args | array of values | positional |
| 4 | named | a map of names | each name without the `:`, `@` or `$` the statement spells it with |
| 5 | write | bool | a query's: run on the writer, for a returning clause |
| 6 | rows | bool | a batch statement's: answer the rows it returns rather than what it changed |

An argument is nil, an integer, a float, str, bin, or a bool, which SQLite
keeps as 1 or 0; a NaN is `invalid`, since SQLite would keep NULL. A value
that comes back is one of SQLite's five, nil, an integer, a float, str or bin,
bin also carrying TEXT that is not UTF-8, and it is what the row keeps: a time
held as text travels as its text, whatever its column declares.

Done is `{1: changes, 2: last id}`, the columns `{1: [name…]}` and a row
`{1: [value…]}`. Statements are `{1: handle, 2: [statement…], 3: read, 4:
[jobs…]}` and their results `{1: [result…]}`, a result being done, or `{3:
columns, 4: [[value…]…]}` for a statement that returned rows.

Each item of `jobs` has the shape of an enqueue's request, `{1: handle, 2:
[job…]}`, and its queue must be opened `in` this database. The transaction
writes the jobs after the statements, so a job commits with the rows it is
about, or not at all. A job has no result. When a job fails, `what` names it as
`call`, counted after the statements. A job in a view, or on a queue that lives
in another file, is `invalid`.

A message past the agreed body is `limit`: a row a query downloads, or a
batch's results, which travel in one message. A query holds its rows in the
server's memory, 64 MiB of them at most, before the first leaves.

A data connection's statement, each of a batch's included, runs only once the
check [server.md](https://github.com/tinyshed/research/blob/main/tinystore/design/server.md#what-a-connection-may-do) describes lets it, and
one it refuses is `permission`; it runs 30 seconds at most, past which it is
`limit`. An admin connection's runs as it is.

`sql.exec` of an insert on handle 1, and its answer:

```text
31 00 00 00  03  01  02 04  03 00 00 00                                   a body of 49 bytes, REQUEST, END, 0x0402, stream 3
83                                                                        a map of three
   01 01                                                                  handle: 1
   02 d9 24 69 6e 73 65 72 74 20 69 6e 74 6f 20 6e 6f 74 65 73 20 28 74   sql: "insert into notes (title) values (?)"
      69 74 6c 65 29 20 76 61 6c 75 65 73 20 28 3f 29
   03 91 a4 6d 69 6c 6b                                                   args: ["milk"]

05 00 00 00  04  01  00 00  03 00 00 00                                   a body of 5 bytes, RESPONSE, END, stream 3
82                                                                        a map of two
   01 01                                                                  changes: 1
   02 07                                                                  last id: 7
```

### records

Records are one log of the store's, and each call names the streams it is
about, so none opens a handle.

| method | | request | answer |
|---|---|---|---|
| `0x0501` | append | records, one transaction writes all or none | `{}`; `what` names a refused record as `call`, with its stream and name |
| `0x0502` | read | a query | a download: `{}`, a record a `DATA`, then a page |
| `0x0503` | follow | a cursor | a download: `{}`, a record a `DATA`, then the cursor after them |
| `0x0504` | lines | a stream | an upload: another program's output as `DATA`, cut anywhere; `{}` |
| `0x0505` | damaged | `{}` | the damage: `{1: [damage…]}` |
| `0x0506` | drop | a damage | `{}`; an admin connection's alone, since it is a repair |

A record:

| key | field | type | |
|---|---|---|---|
| 1 | at | int | unix nanoseconds, past what a JavaScript number holds |
| 2 | stream | str | a namespace the application names |
| 3 | name | str | the event: `log` for a log line |
| 4 | level | int | slog's: -4 debug, 0 info, 4 warn, 8 error; absent for none |
| 5 | body | str | absent for none, which an empty body is not |
| 6 | trace id | bin | 16 bytes; absent for none |
| 7 | span id | bin | 8 bytes; absent for none |
| 8 | context | array | who produced it: a key, then its value's JSON, and so on |
| 9 | attrs | array | what happened, as context |

`["user", "42", "tags", "[\"a\"]"]` is two fields, `user` and `tags`, whose
values are JSON spelled as they were given, `1.2300`, `-0` and a big integer
coming back byte for byte; keys keep their order and may repeat. A text,
a stream, a name, a body, a key or a value, whose bytes are not UTF-8, as
another program's output may be, travels as bin and comes back byte for byte.

A query:

| key | field | type | |
|---|---|---|---|
| 1, 2 | from, to | int | unix nanoseconds, to excluded; absent for an open end |
| 3 | streams | array of str | absent or empty for every stream |
| 4 | names | array of str | absent or empty for every name |
| 5 | min level | int | a record without a level does not match |
| 6 | trace id | bin | 16 bytes |
| 7, 8 | attrs, context | array | fields as a record's, each one a record must hold |
| 9 | newest | bool | newest first; oldest first when absent |
| 10 | limit | uint | the records a page holds: 1000 when absent, at most 10000 |
| 11 | budget blocks | uint | the blocks one read may open; each budget narrows the server's |
| 12 | budget bytes | uint | the bytes it may fetch |
| 13 | budget records | uint | the records it may decode |
| 14 | search | str | text a record's body or name holds, its case ignored; 1 KiB at most |

A page, a read's trailer, is `{1: more, 2: from, 3: to}`: more says the limit
or the budget ended the page before the range did, and from and to are the
range the next page reads, the query's own with one end moved past this page.
A page never splits a timestamp; more records at one time than a page holds
is `limit`.

A cursor is `{1: segment, 2: row, 3: limit, 4: expired}`: follow's request
names the place the records it asks for begin at, `{}` being the oldest
segment kept, and the records it takes at most; its trailer the place the
next follow begins at, and the segments retention or a drop removed before
the cursor reached them. A record reaches follow once its head is sealed, a
segment's worth or the server's seal age, an hour unless it says otherwise,
after it arrived; read sees it at once.

`records.lines` is `{1: stream}`, then the program's output in `DATA` frames:
each line becomes a record of the stream as Go's `Lines` makes it, a stack
trace's lines joined, a JSON or logfmt line's fields kept, its level found
where its program writes it. The upload's end hands over the record the
writer holds, a line cut short included, and its answer follows; a line
longer than a record holds is dropped and counted in the engine's stats.

A damage is `{1: stream, 2: segment, 3: head row, 4: from, 5: to, 6:
reason}`: a sealed segment, dropped whole, or a head row, the other absent;
the times it held, unix nanoseconds; the invariant its bytes broke. A read or
a follow that meets one ends with `corrupt`, its `what` naming it by the same
names, `segment` or `head row`, `stream`, `from`, `to` and `reason`, and a
drop of one that still reads is `conflict`.

`records.append` of one click on stream 5:

```text
2a 00 00 00  03  01  01 05  05 00 00 00                                   a body of 42 bytes, REQUEST, END, 0x0501, stream 5
81                                                                        a map of one
   01 91                                                                  records: an array of one
      84                                                                  a map of four
         01 cf 18 d7 5b 84 2b 4e cd 15                                    at: 1,790,000,000,123,456,789 ns
         02 a3 77 65 62                                                   stream: "web"
         03 a5 63 6c 69 63 6b                                             name: "click"
         09 92 a7 65 6c 65 6d 65 6e 74 a5 22 62 75 79 22                  attrs: ["element", "\"buy\""]
```

### metrics

Metrics are one store of series, each named by its labels, so none opens a
handle.

| method | | request | answer |
|---|---|---|---|
| `0x0601` | ingest | series and their samples, stored all or none | `{}`; `what` is a refused series' labels |
| `0x0602` | read | a range | a download: `{}`, a series a `DATA`, then `{}` |
| `0x0603` | aggregate | a range with a width and an operation | a download: `{}`, a series' buckets a `DATA`, then `{}` |
| `0x0604` | drop | labels | `{1: found, 2: unreadable groups}` |
| `0x0605` | explain | a range; with an operation, the aggregate's | a plan |

A plan is what a read, or an aggregate when the range names an operation,
would spend, found in one snapshot from the series, their block directories
and their heads, with no payload fetched and no sample decoded:

| key | field | type | |
|---|---|---|---|
| 1 | series | uint | the series it matches |
| 2 | blocks | uint | the blocks it opens |
| 3 | summarized | uint | the blocks an aggregate answers from their summaries |
| 4 | bytes | uint | the bytes it fetches |
| 5 | decoded | uint | the samples it decodes |
| 6 to 10 | limit series, blocks, bytes, decoded, answered | uint | the limits it runs under |
| 11 | stops | an error | the `limit` it would end with, `what` naming it; absent when it fits |

A series:

| key | field | type | |
|---|---|---|---|
| 1 | labels | a map of names | the series' name as `__name__` among them, required on ingest; names unique, every text UTF-8 |
| 2 | kind | str | `gauge` or `counter` |
| 3 | times | bin | unix milliseconds, a little-endian int64 each |
| 4 | values | bin | a little-endian float64 each, its bits the data: -0 and a NaN's payload come back as they went |

The wire carries a series' name as the store keeps it, its label `__name__`;
the Go API and every SDK give it apart from the labels, as `name`, and refuse a
label of the application's beginning with `__`.

The two columns hold as many values each. A series longer than half a body
holds comes in several `DATA`, one after another, each with its labels and
kind, its samples going on in time order; a client joins them. Ingest keeps
the value given last for a time repeated.

A range:

| key | field | type | |
|---|---|---|---|
| 1 | matchers | a map of names | the labels a series has, exactly, its name as `__name__` |
| 2, 3 | from, to | int | unix milliseconds, to excluded; both required, 2^63−1 the open end |
| 4 | limit series | uint | the series it matches; each limit narrows the server's |
| 5 | limit blocks | uint | the blocks it decodes |
| 6 | limit bytes | uint | the bytes it fetches |
| 7 | limit decoded | uint | the samples it decodes |
| 8 | limit answered | uint | the samples or buckets it answers |
| 9 | width | uint | aggregate's: milliseconds, the buckets starting at from |
| 10 | op | str | aggregate's: `count`, `sum`, `min`, `max`, `avg`, `increase` or `rate`, a counter's alone, or `delta`, a gauge's |
| 11 | where | array of conditions | labels beyond equality, each label once, in the byte order of its name |
| 12, 13 | by, without | array of str | aggregate's: the labels a group keeps, or all but these; an empty `by` is sent and joins every series of a name |

A condition:

| key | field | type | |
|---|---|---|---|
| 1 | label | str | the label's name |
| 2 | kind | str | `one_of` its values, `none_of` them, which a series without the label is too, or `prefix`, its one value |
| 3 | values | array of str | one at least, a thousand at most |

A range finds its series by a matcher, a `one_of` or a `prefix`: a range of
`none_of` alone is invalid, since it would scan every series. A condition of a
kind the server does not have is `unimplemented`, `what` naming it, as is an
operation, `what` naming it as `op`.

The server reads a range whole, within its limits, before the first series
leaves, so that a slow client holds none of the engine's readers; a read or an
aggregate that fails sends no series.

An aggregate's item is a series, keys 1 and 2, and its buckets as columns, as
many values each:

| key | field | type | |
|---|---|---|---|
| 3 | from | bin | each bucket's start, unix milliseconds, a little-endian int64 each |
| 4 | to | bin | its end, excluded |
| 5 | count | bin | the samples it counted, an int64 each |
| 6 | resets | bin | the resets among them, an int64 each |
| 7 | values | bin | its value, a float64 computed exactly and rounded once |
| 8 | flags | bin | a byte each: 1 when the value overflowed to an infinity, 2 when retention cut the bucket, which counted only its samples from the cutoff on |

An increase counts a reset inside its bucket and not the step from one bucket
to the next; only a bucket holding samples is answered.

Drop removes one series and everything it holds, whether it still reads or
not, the labels naming it exactly; unreadable groups counts the groups removed
without the payload rows their directory no longer names.

`metrics.ingest` of one sample of `cpu{host="web-1"}` on stream 9:

```text
3b 00 00 00  03  01  01 06  09 00 00 00                                   a body of 59 bytes, REQUEST, END, 0x0601, stream 9
81                                                                        a map of one
   01 91                                                                  series: an array of one
      84                                                                  a map of four
         01 82                                                            labels: a map of two names
            a8 5f 5f 6e 61 6d 65 5f 5f a3 63 70 75                        __name__: "cpu"
            a4 68 6f 73 74 a5 77 65 62 2d 31                              host: "web-1"
         02 a5 67 61 75 67 65                                             kind: "gauge"
         03 c4 08 00 6c 50 c4 a0 01 00 00                                 times: [1,790,000,000,000]
         04 c4 08 00 00 00 00 00 00 e0 3f                                 values: [0.5]
```
