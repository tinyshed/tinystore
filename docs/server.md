# The server: one runtime for another process

Designed, not built. This page is the contract the server and its SDKs are
built to: what a connection may do, how a sidecar starts and is found, what a
lost connection means, and what bounds the server's memory. The bytes are
[wire.md](wire.md). Its figures are
[the round](reports/rpc-mechanics-2026-09-28.md)'s: `spike/rpc_*`, a sidecar
holding one kv bucket reached from Go, Bun and Python, on Windows and in a
Linux container; the probe before it, outside the repository, is superseded
but for the encoding, which the round did not measure again.

## What it is for

TinyStore runs three ways, and they differ in the process around the store,
never in what an operation means:

| mode | who calls | through |
|---|---|---|
| embedded | a Go program, in its own process | function calls, as today |
| sidecar | a Bun or Python program on the same machine | a child's stdin and stdout, or a local socket |
| server | programs on other machines | TCP with TLS |

- **One protocol over any byte stream.** A sidecar and a server speak the same
  frames; what differs is the transport, who starts the process and what a
  connection may do. A new transport is a listener, and nothing above it
  changes.
- **One process owns one directory.** The store's `LOCK` already says so: one
  server serves one application's directory, and two applications run two
  servers. There are no tenants.
- **The engines are the same code.** A handler turns a message into an
  engine's call and its answer into a message. Claims, leases, group commits,
  snapshots and bounds stay the engines'.

## What a call costs

A call that waits for its answer before the next pays a round trip, and no
encoding takes it back. The round, kv `Get`s a second through one connection,
the mean latency at one in flight in brackets:

| client → sidecar | Windows, 1 in flight | 256 | container, 1 in flight | 256 |
|---|---:|---:|---:|---:|
| Go, embedded: no sidecar | 155,562; 155,985 (6.4 µs) | 403,076; 405,971 | 159,793; 161,275 (6.2 µs) | 324,336; 341,819 |
| Go → named pipe, or Unix socket in the container | 31,915; 32,148 (31.1–31.3 µs) | 260,976; 265,816 | 5,502; 5,569 (179.6–181.8 µs) | 214,911; 219,116 |
| Bun → named pipe, or Unix socket | 31,216; 31,371 (31.9–32.0 µs) | 246,239; 249,290 | 8,961; 9,192 (108.8–111.6 µs) | 210,300; 212,015 |
| Python asyncio → named pipe, or Unix socket | 23,309; 23,465 (42.6–42.9 µs) | 246,761; 257,749 | 6,375; 6,524 (153.3–156.9 µs) | 219,422; 221,186 |

What follows from it:

- **Throughput comes from calls in flight.** From one to sixty-four in flight
  a connection serves 8 to 16 times the `Get`s on Windows and 19 to 38 times
  in the container, and at 256 Go, Bun and Python serve within 15 % of each
  other: the ceiling is the sidecar's, not theirs. So every call is a stream
  of its own, answers come in any order, and an SDK's calls return promises.
- **A write is an fsync, and the group hides the wire.** At 256 `Set`s in
  flight a sidecar commits from 87 % of the embedded rate to all of it. So the
  server runs calls concurrently, and the calls of every client meet in the
  engine's group.
- **The first sender writes.** Frames queued while one is written leave in
  the next write, written by whoever found nobody writing: a write a frame
  loses 3 to 27 times at depth, and a goroutine of their own costs 2 to 18 µs
  a call at one in flight.
- **The sidecar's own ceiling is its runtime.** A goroutine started for each
  call grows its stack on the way into SQLite every time, a quarter of the
  sidecar's time in the container, and the default GC target collects some 500
  times a second, shrinking the stacks that workers keep. 256 long-lived
  workers with GOGC 400 served 453,423 to 456,905 `Get`s on Windows and
  414,741 to 416,474 in the container, 69 to 96 % above a goroutine a call and
  what the embedded program serves. So calls run on workers, as many as the
  streams in flight, and the server sets its collector's target.
- **The encoding is the smallest lever, and exactness chooses it.** The
  probe: a kv set request and a get's answer, encoded and decoded, took 1,370
  and 1,103 ns with 8 and 5 allocations through `encoding/json`, 80 and 31 ns
  with 4 and 0 through MessagePack written by hand; 220 and 205 bytes against
  146 and 122. What rules JSON out is that it carries no NaN, loses digits past
  2^53 in JavaScript, where a record's time in nanoseconds lives, and turns
  bytes into base64.
- **The transport is the platform's.** On Windows a named pipe is the fastest
  for every client, and CPython there has neither `AF_UNIX` nor Unix
  connections in asyncio; in the container a Unix socket and stdio carried
  1.14 to 1.16 million of Go's echoes a second, TCP 0.65 million.
- **A latency from WSL2 is not Linux's.** In the container one call in flight
  took 82.5 to 189.7 µs; with its virtual processors kept busy, a Go echo
  through a socket took 44.5 to 58.1 µs instead of 134.4 to 147.0, so most of
  it is waking an idle virtual processor. Bare Linux and macOS are measured
  before a latency there is quoted.

## Layers

```text
   Bun SDK      Python SDK        Go program (embedded)
      │              │                   │
      ▼              ▼                   │  function calls
  transport:  stdio │ Unix socket │ named pipe │ TCP │ TLS
      │                                  │
  server/wire  frames, MessagePack, messages, error codes    standard library only, fuzzed
      │                                  │
  server       sessions, credit, cancelling, limits, tokens, │
               listeners; handlers: kv.get → Bucket.GetEntry │
      │                                  │
  engines      kv  jobs  blobs  sqldb  records  metrics  ◄───┘  the same code
      │
  tinystore    LOCK, Memory, Now, Every, Close
```

- **`server/` is a module of its own**, as AGENTS.md always said a service is:
  `github.com/tinyshed/tinystore/server`. It requires the root module and the
  standard library and nothing else, and `cmd/tinystore` requires it for
  `tinystore serve`. A Go program that embeds the store may serve it too,
  through the same package.
- **`server/wire` is the bytes**: frames, the MessagePack profile, the
  messages and the error codes. It imports only the standard library and
  knows no engine's Go type, as `codec/` knows samples and bytes, so a Go
  client links it without linking SQLite.
- **`server` holds everything else**: sessions, credit, limits, tokens,
  listeners and the handlers. A handler imports no transport, a transport no
  engine, and engines import neither.
- **The SDKs live here while the protocol moves**: `sdk/bun` and
  `sdk/python`, each with its own manifest and releases, and `sdk/go.mod`
  keeping them out of the Go module's zip. One commit changes the protocol,
  the server, both SDKs and the vectors they are tested against.

## Starting and finding a server

```text
tinystore serve --dir ./data --stdio              a private child: frames on stdin and stdout
tinystore serve --dir ./data --local --idle 30s   the directory's shared sidecar
tinystore serve --dir /srv/data --listen tls://0.0.0.0:7443 --tls-cert … --tls-key … --tokens tokens.txt
```

```ts
const store = await open("./data")                    // the directory's sidecar, found or started
const mine  = await open("./data", { private: true }) // a child on stdio, living and dying with this process
const there = await connect("tls://db.internal:7443", { token })
```

- **Shared, the default.** One sidecar a directory, found through `SERVE` or
  started by whoever needs it first, and gone after `--idle` without a
  connection. Four uvicorn workers and a cron script share one.
- **Private.** The SDK starts `tinystore serve --stdio` with pipes: frames on
  stdin and stdout, the server's logs on stderr, which the SDK reads to its
  end, since a pipe nobody drains stops the server when it fills. No socket,
  no file, no port. The end of stdin is the parent's end: the server drains
  and exits. Tests, scripts and a single Bun process want this.
- **External.** Something else runs it: systemd, a container, another
  machine. The SDK is given an endpoint and, off the machine, a token.
- **The binary travels in the SDK's package**: per-platform optional
  dependencies on npm, per-platform wheels on PyPI. It is pure Go built with
  `CGO_ENABLED=0`, so every target builds from one machine.
- **Engine options are the server's**, from its flags: retention, clock
  skew, memory. A client's `open` carries only a handle's options.
- **The server opens an engine the first time a client asks for it**, with
  the server's options, so a sidecar for an application that keeps only kv
  makes no `jobs.db`. A Go program that embeds the server passes the handles
  it opened instead, since an engine opens once a store.

### SERVE: finding the directory's sidecar

Everything a local server publishes lives in `<dir>/server/`, which only its
owner may enter. `SERVE` in it is a hint; the directory's `LOCK` is the
truth.

```json
{"protocol": 1, "server": "0.4.0", "pid": 4212, "instance": "q1q0l3yZ3JvYw8g7oM2sYA",
 "endpoints": ["unix:///srv/app/data/server/tinystore.sock"]}
```

- **Written whole, under the lock, for its owner.** The server writes
  `SERVE` only while it holds `LOCK` and only once it listens, as a temporary
  file renamed over the last, and removes it on a clean exit. `server/` is
  mode 0700; on Windows, which lets anyone traverse a directory to a file whose
  path they know, it carries an inherited DACL naming its owner alone, so that
  every file made in it is the owner's alone.
- **An instance is sixteen random bytes a start.** `WELCOME` repeats it, so a
  client that reached another process through an endpoint left behind knows.
- **Find or start.** A client reads `SERVE`, connects and checks the
  instance. Any failure (no file, a refused connection, another instance)
  starts `tinystore serve --dir <dir> --local`. That child either takes
  `LOCK`, which means the old server is gone, removes the socket it left and
  replaces `SERVE`; or finds `LOCK` held and exits with a code of its own, and
  the client reads `SERVE` again until the winner answers, five seconds at
  most.
- **No pid is trusted.** A pid is reused; a lock is released by the operating
  system when its process dies.
- **A socket's path fits.** `<dir>/server/tinystore.sock` when that absolute
  path fits `sockaddr_un` (104 bytes on macOS, 108 on Linux and Windows); past
  it, a directory of the owner's own under the user's runtime directory, named
  after the store's absolute path.
- **Windows serves every client through a named pipe**, `pipe:tinystore-`
  and a hash of the store's absolute path, the fastest there for Go, Bun and
  Python alike. It is created with a DACL naming its owner alone, refusing
  remote clients, and as the first instance of its name, so that no other
  process holds the name before it. Go opens it as an overlapped handle that
  `os.NewFile` gives the runtime's completion port, since a synchronous one
  would hold every write behind a pending read; Bun through `node:net`;
  Python's asyncio through its Proactor loop's `create_pipe_connection`. A
  Python client without asyncio needs overlapped I/O too, through `_winapi`
  as `multiprocessing` does, and is built and measured with the Python SDK.
  No local endpoint takes a token: its permission is the file system's.

## What a connection may do

`WELCOME` states one of two capabilities:

| capability | may | given to |
|---|---|---|
| admin | everything, the schema included: apply sqldb migrations, run any SQL; later backup, maintenance and repairs | a private child; the local socket or pipe; a remote token marked admin |
| data | every engine's reads and writes; open buckets, queues and databases whose migrations are applied; SQL that reads and changes rows | a remote token marked data |

- **A local connection is admin**, since its user could open the files
  anyway.
- **A data client cannot change a schema.** Its `sql.open` may carry
  migrations: the server checks them against the file, as `Open` does, and
  refuses a pending one with `permission`. An application checks its schema
  without the right to change it, and a deploy step holding an admin token
  migrates.
- **SQL from a data client is one statement a step and changes only rows.**
  Its first word past comments is `SELECT`, `VALUES`, `WITH`, `INSERT`,
  `REPLACE`, `UPDATE` or `DELETE`. DDL, `ATTACH`, `DETACH`, `VACUUM` (whose
  `INTO` writes a file wherever the server may), `PRAGMA`, `ANALYZE`,
  `REINDEX` and transaction control are `permission`, and so is a second
  statement in the same string, which the driver would run as well: modernc
  v1.59.0 runs every statement of a string (`stmt.go`). The driver exposes no
  SQLite authorizer, as its `driver.go` says, so the check is the server's own:
  a tokenizer that skips comments, string literals, quoted identifiers and
  blob literals, and behind it a second, independent line that refuses a
  statement whose compiled program, as `EXPLAIN` lists it, changes the schema,
  attaches a file or vacuums. An authorizer replaces both if the driver gains
  one.
- **That check is security-critical.** It is the one place where text a
  client wrote decides what the server may do, so it gets an adversarial round
  of its own with the sql slice, before a data token exists: comments and
  nested comments, CTEs, quoted and bracketed identifiers, Unicode whitespace,
  semicolons inside literals, triggers and their side effects, virtual tables,
  SQLite's functions, `EXPLAIN` and nested syntax; every attempt that got
  through is a vector the fuzzer keeps.
- **A token is a line of a file**, `admin <token>` or `data <token>`, 32
  random bytes in base64url, compared in constant time. On TCP there is no
  connection without one.

## Across the wire

- **No transaction is held across the network.** The one writer of a file
  would wait for every round trip of a transaction a client holds open, and
  for good once that client hangs. Atomic work is a batch the server runs
  whole: `kv.batch` in one `Store.Tx`, where a conflict in any of its
  operations rolls back all of them, and `sql.batch` in one `Tx`, or in a
  `View` for reads from one snapshot. A race without the writer held is
  `IfVersion`. Embedded Go keeps `Tx` and `View` as they are.
- **A handle is a number.** `kv.open`, `jobs.open`, `blobs.open` and
  `sql.open` answer with a number the session's later calls carry, as a file
  descriptor is carried. Its options are the handle's, as `OpenBucket`'s are;
  a lost connection closes them, and the SDK opens them again on the next.
- **Every stream ends once.** The server sends each stream exactly one final
  frame, with its result or its error, a cancelled stream included.
- **Cancelling follows the engines.** A `CANCEL` ends the handler's context.
  A write whose turn has not come writes nothing; one that has started
  finishes with its group, and its final frame says which happened.
- **A lost connection** leaves each piece of work as a crash would:
  - a write in flight is `outcome_unknown` to its caller, who reads what it
    wrote before writing again, as with the engines' `ErrOutcomeUnknown`; a
    read may be sent again;
  - an upload is aborted and leaves nothing;
  - a job in a remote worker's hands has failed that attempt, as it would had
    the worker's process died, so a job that kills its worker fails after its
    attempts; the handlers in flight return first, and only then does `Work`'s
    context end, so the jobs claimed ahead and never handed over go back
    uncounted;
  - the session's handles close.
  A connection that says nothing past the idle timeout is asked with a
  `PING`, and closed if it does not answer.
- **`jobs.work` is the engine's own Work loop.** The server calls `Work`
  with a handler that hands each job to the client and settles it by the
  frame that answers. Claiming ahead, settling in the write of the next claim
  and extending leases stay the engine's, and nothing polls:

```go
// work hands the engine's Work loop to a remote worker: it claims ahead,
// settles in the write of its next claim and extends leases; the handler only
// carries a job to the client and its outcome back
func (s *session) work(ctx context.Context, st *stream, queue *jobs.Queue[json.RawMessage], ask wire.JobsWork) error {
	return queue.Work(ctx, func(ctx context.Context, job jobs.Job[json.RawMessage]) error {
		outcome, err := st.offer(ctx, job)
		switch {
		case err != nil:
			return err // the worker vanished: this attempt failed, as a process that died
		case outcome.Retry:
			return job.Retry(ctx, outcome.Err, jobs.At(outcome.At))
		case outcome.Fail:
			return job.Fail(ctx, outcome.Err)
		case outcome.Snooze:
			return job.Snooze(ctx, jobs.At(outcome.At))
		}
		return nil
	}, jobs.Workers(ask.Workers), jobs.Timeout(ask.Timeout))
}
```

- **The store's clock travels.** `WELCOME` carries it, and an SDK warns when
  its own is outside the engines' window of it.

## Memory and limits

A connection holds at most what the server granted it, so the server's
reader copies each frame into room it already has and never waits for a
client:

```text
a connection ≤ its read buffer + the credit granted on it + its answers queued to write
the server   ≤ connections × that + what engines reserve under Options.Memory
a client     ≤ its streams in flight × the agreed body + the credit it granted each download
```

| bound | proposed | set by |
|---|---:|---|
| a frame's body | 1 MiB and 64 KiB: the largest kv or jobs value and the message around it | `WELCOME`, lowered by `HELLO` |
| streams in flight on a connection | 256 | `WELCOME` |
| credit on a connection, client to server | 8 MiB | `WELCOME` |
| credit on a stream, each way | 1 MiB: a round trip's worth where a round trip is long, and more than blobs moves | `WELCOME`, `HELLO` |
| calls running at once | as many as the streams in flight, on workers that keep their stacks | the server |
| the collector's target | above Go's default, since a small live heap and a fast allocation rate collect hundreds of times a second | the server |
| answers queued to write; a handler past it waits | 4 MiB | the server |
| connections | 64 local, 1,024 remote | flags |
| the handshake | 5 s | the server |
| silence before a `PING` | 60 s | the server |

These are proposals; [the round](#the-round) sets them. A client past a bound
breaks the protocol and loses its connection.

## What the engines gain

The server calls each engine's public API. What it needs and the API lacks is
added as ordinary API:

- **kv: a value as its row holds it.** `OpenBucket[kv.Raw]` keeps nothing, an
  int64 or bytes, as the row does, since the server knows no SDK's types. An
  SDK maps its language's values onto those three as `codecFor` maps Go's:
  bytes and strings to bytes, integers and bools to an integer, floats to
  their eight bytes big-endian, anything else to JSON, so a Go program and a
  Python one read each other's buckets. `OpenBucket[any]` stays JSON. Its
  shape, a struct saying which of the three it holds, is settled with the kv
  slice.
- **sqldb: rows without a struct.** Each column as SQLite returned it, NULL,
  an int64, a float64, text or bytes, with the columns' names, for
  `sql.query`.
- **sqldb: migrations checked without applying them.** A client's migration
  files, as an `fs.FS` the server makes of them, compared with what an open
  database applied, for a data client and for every `sql.open` after the
  first.
- jobs (`OpenQueue[json.RawMessage]`), blobs (`Put` from an `io.Reader`,
  `Open`), records and metrics serve as they are.

## The round

[The round](reports/rpc-mechanics-2026-09-28.md) settled, on Windows and in a
Linux container:

1. **the transports**: a named pipe on Windows, a Unix socket elsewhere,
   stdio for a private child, TCP between machines;
2. **who writes a connection's frames**: the first sender to find nobody
   writing, as `UpdateGrouped`'s leader commits, with no goroutine of its own;
3. **what a call costs the server**: stacks grown by a goroutine a call and a
   collector at its default target; workers, as many as the streams in flight,
   and a target the server sets bring it to the embedded program's rate;
4. **a stream's credit**: 1 MiB;
5. **a Python client without asyncio**: one call at a time, since threads add
   no calls.

What it left: macOS, bare Linux and a network between two machines; how the
server sets its collector's target, a GOGC or a limit from `Options.Memory`;
workers that start and stop with the load; a Python client without asyncio on
a Windows pipe; and the SDKs' own codecs.

## Gates

Each promise above is a test once its code exists:

| promise | gate |
|---|---|
| a frame past its agreed size is refused before its body is read | `FuzzFrames`, `TestAFrameLargerThanAgreedIsRefusedUnread` |
| a body the profile does not allow is refused | `FuzzMessages`, a vector for each rule |
| the vectors are the bytes | `TestVectors`, the examples of wire.md among them |
| a client past its credit loses its connection, and the reader never waits | `TestAClientPastItsCreditIsCutOff` |
| every stream ends with one final frame, a cancelled one too | `TestEveryStreamEndsOnce` |
| answers queued during a write leave in the next | `TestQueuedAnswersShareAWrite` |
| a data client cannot change a schema | `TestADataClientCannotChangeTheSchema`, over the adversarial round's corpus; `FuzzDataSQL` against both lines |
| a lost connection aborts uploads and fails the attempts in hand | `TestALostConnectionAbortsUploadsAndFailsAttemptsInHand` |
| `SERVE` is written whole, only under `LOCK`, for its owner alone | `TestServeIsWrittenWholeUnderTheLock` |
| a stale `SERVE` starts one sidecar | `TestAStaleServeStartsOneSidecar` |
| the server module requires only the root | `TestTheServerRequiresOnlyTheRoot` |
| `server/wire` imports only the standard library | `TestWireImportsOnlyTheStandardLibrary` |
| engines import neither `server` nor `server/wire` | `TestEnginesDoNotImportEachOther`, extended |

## Not in the first version

- Transactions held open across the network.
- An HTTP and JSON gateway for browsers and curl: a module of its own over the
  same handlers.
- Windows named pipes, until the round has measured them.
- A blob put from a file path when the sidecar shares the disk.
- Tenants, replication, QUIC, compression.
- The server's own metrics, which wait for self-metrics.

## SDKs

Designed with the server and built after it, in `sdk/bun` and `sdk/python`:

```ts
const store = await open("./data")
const sessions = store.kv.bucket<Session>("sessions", { sliding: "30d" })
const s = await sessions.of(userId).get(token)                        // undefined when absent
await sessions.of(userId).set(token, s, { ttl: "1h" })

const reminders = store.jobs.queue<Reminder>("reminders")
await reminders.enqueue({ user: 42, text: "call mom" }, { at: evening })
await reminders.work(async (job) => remind(job.value), { workers: 8 })  // returning acknowledges, throwing retries

const { object, body } = await store.blobs.bucket("avatars").of(user.id).get("original") // body: a ReadableStream
const notes = await (await store.sql("app", { migrations: "./migrations" })).all<Note>(
  "select * from notes where author_id = ?", [user])
```

```python
store = await tinystore.open("./data")
sessions = store.kv.bucket("sessions", Session, sliding=timedelta(days=30))
s = await sessions.of(user_id).get(token)
await reminders.work(remind, workers=8)

store = tinystore.open_sync("./data")  # a program without asyncio: one call at a time
```

- **The Go API's vocabulary.** A type is given once, when a bucket or queue
  opens; the daily calls are plain verbs; what the engine chooses goes into
  open's options and never into a call. A plain verb returns the least and
  its entry twin the version, as kv's `Get` and `GetEntry` do.
- **Every call is a stream, and a turn's frames leave in one write**: those
  asked for in one microtask in JavaScript, in one loop turn in asyncio. Calls
  return promises; a batch goes where the engine commits once, an enqueue or
  an append of many.
- **Values as [wire.md](wire.md#methods) carries them.** A kv bucket's values
  map onto nothing, an integer or bytes as `codecFor` maps Go's; a job's value
  is JSON; a 64-bit integer past 2^53 is a `BigInt` in JavaScript; a float
  whose bits are the data travels as bytes.
- **An error is its code's class**, `ConflictError`, `LimitError` and the
  rest, carrying what it names.
- **A lost connection** rejects the writes in flight with `outcome_unknown`,
  while a read may be sent again; the SDK connects again, opens its handles
  again, and a work loop resumes.
- **Logs and instruments stay in the SDK**, as the Go engines keep them: a
  logging handler, Python's `logging.Handler` or a Bun logger, appends its
  records once a second without waiting, dropping and counting what does not
  fit; counters and gauges live in the SDK and are ingested every flush.
- **Python**: asyncio for concurrency, the MessagePack C extension,
  `unpackb(..., strict_map_key=False)`; a client without asyncio makes one call
  at a time, since threads add none. **Bun**: `Bun.connect` for sockets,
  `node:net` for Windows pipes, `Bun.spawn` for a private child.

## Building it

In slices, each engine's messages fixed in [wire.md](wire.md) before its
code: `server/wire`; a session with its transports; kv, with a Go client the
tests use; jobs; blobs; sql, with the adversarial round of its SQL check;
records; metrics; `tinystore serve` with `SERVE`; the Bun SDK; the Python SDK.

- **Read first**: AGENTS.md, this page, [wire.md](wire.md) and
  [the round](reports/rpc-mechanics-2026-09-28.md). The prototype
  `spike/rpc_*` is prior art to read, not code to import: production code
  calls no helper of the spike.
- **A slice is done** when its messages and vectors are on wire.md, its gates
  in the table above pass, and what its engine gained is in that engine's
  README and tests.
- **The first slice changes AGENTS.md**: Modules gains `server/`, and later
  `sdk/` with its empty `go.mod`; Shape its paths; Status what is built; the
  gates table each gate as it lands. The Taskfile and CI test, lint and tidy
  the new module as they do `cmd/tinystore`.
- **The built server is measured against the prototype**: the round's cases,
  on the same machine, beside its figures; a rate the server does not reach is
  a finding for the report, not a footnote.
