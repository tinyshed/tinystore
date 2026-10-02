# The wire protocol, version 1

The bytes a TinyStore server and its clients exchange over any byte stream, a
child's stdin and stdout, a Unix socket, a Windows named pipe, TCP or TLS.
What they mean, who may send them and what bounds them is
[server.md](server.md). Frames, the profile, the handshake, the errors and
every engine's methods, kv, jobs, blobs, sql, records and metrics, are built
in `server/wire`. Every example on this page is a vector of
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
| `limit` | a bound: memory, size or count | later, or smaller |
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
| metrics | ingest, drop | calls |
| | read, aggregate | a download, a series a message, or several for a series longer than a body holds |

The values they carry:

- a kv value is nil, an integer or bin, as its row keeps it, and a version is
  an opaque bin compared only for equality;
- a job's value is its JSON as str;
- an SQL value is one of SQLite's five: nil, an integer, a float, str or bin;
- a record's attribute keeps its JSON spelling as str;
- a series' samples, and an aggregate's buckets and values, are bin columns.

### kv

`kv.open` answers a handle on a bucket of values or on counters, and every
other call carries it. A handle holds `kv.Raw` values, so the server reads
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

A bucket:

| key | field | type | |
|---|---|---|---|
| 1 | name | str | `[a-z0-9][a-z0-9_-]{0,63}` |
| 2 | counters | bool | counters rather than values |
| 3 | default ttl | uint | milliseconds |
| 4 | sliding | uint | milliseconds; values alone |
| 5 | lose at most | uint | milliseconds; counters alone |

A handle is `{1: uint}`. A call:

| key | field | type | |
|---|---|---|---|
| 1 | handle | uint | |
| 2 | owners | array of keys | the branch below the bucket's root; absent for the root |
| 3 | key | a key | |
| 4 | value | nil, int or bin | nil when absent |
| 5 | ttl | uint | milliseconds |
| 6 | expire at | int | unix milliseconds |
| 7 | if version | bin | a version an entry carried |
| 8 | if absent | bool | |
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
| `0x0204` | cancel | a key | an entry: found says it came in time |
| `0x0205` | get | a key | an entry |
| `0x0206` | claim | a lease | a held job, found false when none was due |
| `0x0207` | settle | outcomes of claimed jobs, written in one group | settled: nil or an error each |
| `0x0208` | scan | a query | a download: `{}`, an entry a `DATA`, a page |
| `0x0209` | work | workers | both ways: `{}`, then held jobs out and outcomes back, `DATA`·END each side |

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
| 6 | state | uint | 1 waiting, 2 leased, 3 failed |
| 7 | err | str | its last failure |
| 8 | repeat | str | a repeating job's cron text and zone, as jobs.db keeps it |

A lease is `{1: handle, 2: lease}`, milliseconds, the queue's when absent. A
held job is `{1: found, 2: job, 3: key, 4: value, 5: at, 6: attempt}`, job
being the number its outcome names: a claim's lives on its connection until
it is settled, and a work stream's on its stream. An outcome:

| key | field | type | |
|---|---|---|---|
| 1 | job | uint | |
| 2 | how | uint | 1 ack, 2 retry, 3 fail for good, 4 snooze, 5 extend |
| 3 | err | str | why a retry or a failure |
| 4 | at | int | unix milliseconds: when a retry or a snooze runs again |
| 5 | after | uint | milliseconds: the same from now, or how long an extend holds; written when given, 0 too, which runs a retry or a snooze now where one without a time waits its backoff |

Outcomes are `{1: [outcome…]}` and settled `{1: [nil or error…]}`. A query
is `{1: handle, 2: prefix, 3: state, 4: after, 5: limit}` and a page `{1:
more, 2: after}`, as `jobs.Query` and `jobs.Page` are.

Workers are `{1: handle, 2: workers, 3: timeout, 4: until idle}`: the
queue's own Work loop runs for the stream, `workers` at once, 1 when absent
and at most the streams in flight, each job `timeout` milliseconds in the
client's hands, a minute when absent, and with `until idle` only until no job
is due and none runs, as a test wants it. Each job it hands a handler goes
out as a held job; the client settles it by sending its outcome as `DATA` on
the same stream, and ends its side with `DATA`·END once it takes no more. An
extend is not one of those outcomes: the loop extends its leases itself, and
ends the stream `invalid` on one rather than take it as an ack. When that side ends, or the
client cancels or leaves, the jobs in its hands fail that attempt, as a
worker whose process died fails it, and only after their handlers return
does the loop stop, so that the jobs it claimed ahead and never handed over
go back uncounted. The server's `DATA`·END, `{}`, follows.

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
`{1: [value…]}`. Statements are `{1: handle, 2: [statement…], 3: read}` and
their results `{1: [result…]}`, a result being done, or `{3: columns, 4:
[[value…]…]}` for a statement that returned rows. A message past the agreed
body is `limit`: a row a query downloads, or a batch's results, which travel in
one message. A query holds its rows in the server's memory, 64 MiB of them at
most, before the first leaves.

A data connection's statement, each of a batch's included, runs only once the
check [server.md](server.md#what-a-connection-may-do) describes lets it, and
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
