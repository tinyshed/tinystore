# The wire protocol, version 1

Designed, not built: the bytes a TinyStore server and its clients exchange
over any byte stream, a child's stdin and stdout, a Unix socket, TCP or TLS.
What they mean, who may send them and what bounds them is
[server.md](server.md). Every example on this page becomes a test vector in
`server/wire` when it is built.

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
   for `HELLO`.
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
| 1 | protocol | uint | 1 |
| 2 | client | str | a name and version, for logs |
| 3 | token | str | required on TCP |
| 4 | max body | uint | the largest body the client takes; the server's when absent |
| 5 | stream credit | uint | the `DATA` the server may send on a stream before the client grants more; 1 MiB when absent |
| 6 | instance | bin | the instance `SERVE` named, when the client found the server there |

`WELCOME`:

| key | field | type | |
|---|---|---|---|
| 1 | protocol | uint | 1 |
| 2 | server | str | its version |
| 3 | instance | bin | 16 random bytes a start |
| 4 | capability | str | `admin` or `data` |
| 5 | max body | uint | the largest body either side sends |
| 6 | in flight | uint | the streams a client may have open at once |
| 7 | connection credit | uint | the bytes of `REQUEST` and `DATA` bodies a client may send before credit comes back; at least the max body |
| 8 | stream credit | uint | the bytes of `DATA` a client may send on a stream before credit comes back |
| 9 | engines | array of str | what this server serves |
| 10 | now | int | the store's clock, unix milliseconds |

A server whose instance is not the one `HELLO` names still answers with its
own: the client found a stale `SERVE`, and decides.

`GOAWAY`:

| key | field | type | |
|---|---|---|---|
| 1 | code | str | as [an error's](#errors) |
| 2 | message | str | |

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
  with ERROR, a cancelled stream too.
- **`DATA` carries bytes or items.** A blob's bytes are raw; rows, jobs and
  series are a message each. The method says which.
- **A transfer shares the connection.** A sender splits bytes into frames of
  64 KiB at most, so that no stream holds the connection longer than one such
  frame takes to send.
- **`CANCEL` asks the server to stop.** It answers with the final frame it
  would have sent, or with the error `cancelled`; a client may drop what
  arrives on a stream it cancelled.

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
  holds at most its streams in flight times the agreed body in answers.
- **A body past its credit** ends the connection with `GOAWAY` and `protocol`.

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
  key twice is invalid. A key the decoder does not know is skipped, its value
  still well formed and within bounds; an absent key is the field's default,
  and nil stands only where a field allows it.
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
- **An SDK's library** writes plain maps with no extension and reads 64-bit
  integers without rounding. Python's `msgpack.unpackb` needs
  `strict_map_key=False` for integer keys.

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
| `limit` | a bound: memory, size or count | later, or smaller |
| `closed` | the store or the handle closed | after opening again |
| `in_use` | a name is taken | no |
| `conflict` | a condition or a version no longer holds | after reading again |
| `corrupt` | stored bytes no longer read | no |
| `too_old`, `too_new` | a time outside its engine's window | no |
| `suspended` | a series in quarantine | after its repair |
| `outcome_unknown` | a commit whose result is unknown | after reading what it wrote |
| `permission` | the connection's capability does not allow it | no |
| `unimplemented` | a method this server does not have | no |
| `cancelled` | the client cancelled it | — |
| `unavailable` | the server is closing | on another connection |
| `internal` | a fault of the server's | — |

The first nine are the root's sentinels, so that an SDK's error classes mean
what `errors.Is` means in Go. A frame or a `HELLO` the server cannot take is a
`GOAWAY` with `protocol` or `unauthenticated`, and the connection ends.

A conflict on stream 7:

```text
2c 00 00 00  04  03  00 00  07 00 00 00                                   a body of 44 bytes, RESPONSE, END and ERROR, stream 7
83                                                                        a map of three
   01 a8 63 6f 6e 66 6c 69 63 74                                          code: "conflict"
   02 ad 73 74 61 74 65 20 63 68 61 6e 67 65 64                           message: "state changed"
   03 81 a6 62 75 63 6b 65 74 a8 73 65 73 73 69 6f 6e 73                  what: {"bucket": "sessions"}
```

## Methods

Each engine's messages are fixed on this page with its slice, before its
code: numbers, keys, types, shapes and vectors. A call answers with one
message; whatever returns a collection is a download, its items `DATA` under
credit and its last `DATA` saying where the next page begins. What each will
carry:

| engine | operations | shape |
|---|---|---|
| kv | open, get, has, set, delete, take, touch, add, max, clear, batch, view | calls |
| | scan | a download, an entry a message |
| jobs | open, enqueue of many, update, cancel, get, claim, settle of many | calls |
| | scan | a download, an entry a message |
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
| metrics | ingest | a call |
| | read, aggregate | a download, a series a message; an error may end it after some series, as `Stream`'s does |

The values they carry:

- a kv value is nil, an integer or bin, as its row keeps it, and a version is
  an opaque bin compared only for equality;
- a job's value is its JSON as str;
- an SQL value is one of SQLite's five: nil, an integer, a float, str or bin;
- a record's attribute keeps its JSON spelling as str;
- a series' samples, and an aggregate's buckets and values, are bin columns.
