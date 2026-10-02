# The server: one runtime for another process

Being built, in `server/` and `cmd/tinystore`: the bytes, a session with its
transports, every engine, kv, jobs, blobs, sql, records and metrics, and
`tinystore serve` with `SERVE` are; the SDKs are not, and
[Building it](#building-it) says which slice is where. This page is the
contract the server and its SDKs are built to: what a connection may do, how
a sidecar starts and is found, what a lost connection means, and what bounds
the server's memory. The bytes are [wire.md](wire.md). Its figures are
[the round](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-mechanics-2026-09-28.md)'s: research's `spike/rpc_*`, a sidecar
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
   JS SDK       Python SDK        Go program (embedded)
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
- **The SDKs live here while the protocol moves**: `sdk/js` and
  `sdk/python`, each with its own manifest and releases, and `sdk/go.mod`
  keeping them out of the Go module's zip. One commit changes the protocol,
  the server, both SDKs and the vectors they are tested against.

## Starting and finding a server

```text
tinystore serve --dir ./data --stdio              a private child: frames on stdin and stdout
tinystore serve --dir ./data --local --idle 30s   the directory's shared sidecar
tinystore serve --dir ./data --local --log ./data/server/serve.log   as an SDK starts it
tinystore serve --dir /srv/data --listen tls://0.0.0.0:7443 --tls-cert … --tls-key … --tokens tokens.txt
```

```ts
const store = await open("./data")                    // the directory's sidecar, found or started
const mine  = await open("./data", { private: true }) // a child on stdio, living and dying with this process
const there = await connect("tls://db.internal:7443", { token })
```

- **Shared, the default.** One sidecar a directory, found through `SERVE` or
  started by whoever needs it first, and gone after `--idle` without a
  connection, 30 seconds unless told, 0 for never. Four uvicorn workers and a
  cron script share one.
- **Private.** The SDK starts `tinystore serve --stdio` with pipes: frames on
  stdin and stdout, the server's logs on stderr, which the SDK reads to its
  end, since a pipe nobody drains stops the server when it fills. No socket,
  no file, no port. The end of stdin is the parent's end: the server drains
  and exits. A signal ends it too, though the parent keeps stdin open: stdin
  is read on a goroutine of its own and never closed, since closing a blocking
  stdin ends no read waiting on it. Tests, scripts and a single Bun process
  want this.
- **External.** Something else runs it: systemd, a container, another
  machine. The SDK is given an endpoint and, off the machine, a token. From
  the first release, each release publishes `ghcr.io/tinyshed/tinystore`, its
  own linux binary on distroless static, serving `/data` on `tls://` port 7443
  with the certificate, key and tokens it finds in `/etc/tinystore`.
- **The binary travels in the SDK's package**: per-platform optional
  dependencies on npm, per-platform wheels on PyPI. It is pure Go built with
  `CGO_ENABLED=0`, so every target builds from one machine.
- **Engine options are the server's**, from its flags: retention, clock
  skew, memory. A client's `open` carries only a handle's options. Built:
  `--memory`, which is `Options.Memory`: 1 GiB for a server with `--listen`
  unless it says, 0 for no bound, and no bound for a local one, whose clients
  are its user's own; the engines' own options wait for their flags.
- **A sidecar started in the background logs to a file.** Nothing reads its
  stderr, so the SDK starting it passes `--log <dir>/server/serve.log`, which
  serve opens first, owner-only, and appends every line to, the error it ends
  with included; an SDK whose sidecar does not come up says what its last
  lines say. Nothing rotates the file.
- **What serve cannot do opens nothing.** Its flags, a tokens file and a
  certificate are read before the store opens, so a mistake leaves no `LOCK`
  and no file behind; a `tls://` listener takes `--tls-cert` and `--tls-key`
  and nothing else does, and `--listen` takes `--tokens`.
- **The server opens an engine the first time a client asks for it**, with
  the server's options, so a sidecar for an application that keeps only kv
  makes no `jobs.db`. A Go program that embeds the server passes the handles
  it opened instead, since an engine opens once a store.

### SERVE: finding the directory's sidecar

`tinystore status --dir <dir>` prints, as JSON, what a store's directory holds
without opening it, so that it runs beside the server serving it: each
engine's file and its bytes with its write-ahead log, the blobs engine's files,
whether a `LOCK` is there, and the server `SERVE` names, its version, pid and
endpoints, never its secret. A directory without a `LOCK` is no store's.

Everything a local server publishes lives in `<dir>/server/`, which only its
owner may enter. `SERVE` in it is a hint; the directory's `LOCK` is the
truth.

```json
{"protocol": 1, "server": "0.4.0", "pid": 4212, "instance": "q1q0l3yZ3JvYw8g7oM2sYA",
 "secret": "Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE",
 "endpoints": ["unix:///srv/app/data/server/tinystore.sock"]}
```

- **Written whole, under the lock, for its owner.** The server claims
  `server/` from its store, which only the store holding `LOCK` can, removes a
  `SERVE` left behind before it listens, since the endpoint that one names may
  be another process's by now, and writes its own once it listens, as a
  temporary file renamed into place; a clean exit removes it. `server/` is
  mode 0700; on Windows, which lets anyone traverse a directory to a file whose
  path they know, it carries a protected DACL naming its owner alone, which
  every file made in it inherits.
- **A reader does not fail a change.** On Windows a client reading `SERVE`,
  which Go and Python open without sharing its deletion, refuses a rename or a
  remove of it while it holds the file, and so may a scanner; each is tried
  again, eight times from 1 to 64 ms apart.
- **An instance and a secret are random a start**: sixteen bytes `WELCOME`
  repeats, and thirty-two that key the proof below; `SERVE` names both.
- **Find or start.** A client reads `SERVE`, connects and checks the proof.
  Any failure (no file, a refused connection, a proof that does not check)
  starts `tinystore serve --dir <dir> --local`. That child either takes
  `LOCK`, which means the old server is gone, removes the socket and the
  `SERVE` it left and writes its own; or finds `LOCK` held and exits with code
  3, and the client reads `SERVE` again until the winner answers, five seconds
  at most, and starts one again when none does: a sidecar leaving for
  idleness removes its `SERVE` first and holds `LOCK` until its store has
  closed.
- **No pid is trusted.** A pid is reused; a lock is released by the operating
  system when its process dies.
- **Nor is an endpoint a dead server left.** Its name is anyone's to take once
  the server is gone, a Windows pipe's above all, whose namespace every user
  shares, and a client that reached another process would hand it the
  application's data and take its answers. So a client knows its server
  before its first `REQUEST`, and how depends on the transport:

  | transport | how the client knows its server |
  |---|---|
  | stdio | it started the server itself |
  | a Unix socket or a named pipe found through `SERVE` | the proof: its `HELLO` carries a challenge of 16 random bytes, and `WELCOME` answers with their HMAC-SHA256 keyed with `SERVE`'s secret, which only the directory's owner can read |
  | TCP with TLS | the certificate, before its token leaves |
  | TCP | not at all: a token travels in the clear, on a network its operator trusts |

  The proof asks nothing of the operating system, so every SDK checks it the
  same way: a peer's user, `SO_PEERCRED` on a socket or
  `GetNamedPipeServerProcessId` on a pipe, is out of reach of a Bun client,
  whose `node:net` shows neither. `HELLO` carries nothing a stranger could
  use, since a local connection takes no token, and no local endpoint is TCP:
  Windows serves its sidecar through a named pipe.
- **A socket's path fits.** `<dir>/server/tinystore.sock` when that absolute
  path fits `sockaddr_un` (104 bytes on macOS, 108 on Linux and Windows); past
  it, a directory of the owner's own in `$XDG_RUNTIME_DIR`, or in the
  temporary directory without one, `tinystore-` and the first eight bytes of
  the SHA-256 of the store's absolute path in hex: eight, so that a socket
  under macOS's temporary directory still fits.
- **Windows serves every client through a named pipe**, `pipe:tinystore-` and
  the same hash of the store's absolute path, the fastest there for Go, Bun
  and Python alike. It is created with a DACL naming its owner alone, refusing
  remote clients, and as the first instance of its name, so that no other
  process holds the name before it. Go opens it as an overlapped handle that
  `os.NewFile` gives the runtime's completion port, since a synchronous one
  would hold every write behind a pending read; Bun through `node:net`;
  Python's asyncio through its Proactor loop's `create_pipe_connection`. A
  Python client without asyncio needs overlapped I/O too, through `_winapi` as
  `multiprocessing` does, and is built and measured with the Python SDK. No
  local endpoint takes a token: its permission is the file system's.

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
  statement in the same string, which the driver would run as well: an exec
  without arguments runs every statement of its string. The check is the
  server's own, in two lines. The driver now exposes SQLite's authorizer,
  `Conn.SetAuthorizer`, which could replace both; `modernc.org/sqlite`, which
  the check was written for, had none.
- **The first line is SQLite's own tokens**, `sqltokens.go` following
  `sqlite3GetToken` rule by rule: whitespace is space, tab, newline, form feed
  and carriage return, and a byte order mark, while every byte past 0x7f is
  part of a name, so no Unicode space separates words; `--` runs to a newline
  and not to a carriage return; `/* */` does not nest; `'…'`, `"…"` and
  `` `…` `` take their delimiter twice, `[…]` ends at its first `]`; a
  parameter `$a(…)` runs to its `)` through quotes and semicolons, as Tcl's
  arrays do. It refuses a NUL, past which SQLite reads nothing, anything
  unclosed, a first word not of the seven, a token after the first `;`, and a
  name, bare, quoted or a string, since SQLite takes a string for a name where
  one may stand, that is `sqlite_dbpage` or a pragma's table: this build has
  `SQLITE_ENABLE_DBPAGE_VTAB`, whose table writes the file's pages, the
  schema's included, and `select * from pragma_optimize(0x10002)` analyzes and
  so makes `sqlite_stat1` and `sqlite_stat4`. The line reads tokens, not where
  they stand, so a value spelled exactly so, `'pragma_optimize'`, is refused
  too; an argument carries it.
- **The second line is the program SQLite compiles**, `EXPLAIN` of the
  statement on a reader, which runs nothing, with its arguments. It refuses
  `CreateBtree`, `Destroy`, `DropTable`, `DropIndex`, `DropTrigger`,
  `ParseSchema`, `SetCookie`, `VCreate`, `VDestroy`, `VRename`, `Vacuum`,
  `IncrVacuum`, `SqlExec`, `JournalMode`, `LoadAnalysis`, `Expire`,
  `Checkpoint`, `AutoCommit`, `Savepoint` and `MaxPgcnt`; a `Function` whose
  p4 is `sqlite_attach` or `sqlite_detach`; an `OpenWrite` of page 1, of a
  page it computes, or of the migration history's table or index, and a
  `Clear` of them, which is how `delete` without a `where` empties a table;
  and any virtual table operation on `sqlite_dbpage` or `pragma_optimize`,
  known by the pointer `EXPLAIN` shows for each on the same reader. The
  history's pages come from `sqlite_schema` in the same snapshot.
- **Where a statement ends is the first line's alone.** `EXPLAIN` prefixes the
  first statement of a string and runs the rest, and compiling a `PRAGMA`
  already sets its flag on the connection, `explain pragma foreign_keys = on`
  included, so the second line only ever sees one statement that begins with
  one of the seven words. What both lines see, each refuses on its own:
  `TestEachLineOfTheCheckRefusesOnItsOwn` holds each to its verdicts.
- **That check is security-critical.** It is the one place where text a
  client wrote decides what the server may do, so it has an adversarial round
  of its own, `attempts` in `datasql_test.go`, run over the wire through
  exec, query and batch by `TestADataClientCannotChangeTheSchema`: comments
  and nested comments, CTEs, quoted and bracketed names, Unicode whitespace
  and a long s, semicolons inside literals and a Tcl parameter's index,
  triggers, virtual tables, `pragma_` tables, `sqlite_dbpage`, `EXPLAIN`,
  `ATTACH` inside `WITH`, and writes to `_tinystore_migrations` and
  `sqlite_schema`. `FuzzDataSQL` runs whatever the check lets through against
  a scratch database and fails when the schema, the migration history, the
  writer's settings and attached databases or the store's files change; its
  seeds are the round, and five minutes of it on Windows 11, Ryzen 7 7700, Go
  1.27.1, 14.2 million inputs, found nothing:

  ```sh
  go -C server test -run '^$' -fuzz '^FuzzDataSQL$' -fuzztime 5m .
  ```

  A data client's statement ends at a deadline, 30 seconds, since a recursive
  query can hold a reader or the writer for good. A write that deadline ends
  takes its group with it, since SQLite rolls back the whole transaction of a
  write statement it interrupts; a client's cancel lets it finish.
- **What the check does not bound is memory; `--memory` does.** A data
  client's `zeroblob(1e9)` makes SQLite allocate its whole length, a gigabyte,
  before the store's memory sees a row, so sqldb sets `SQLITE_LIMIT_LENGTH` to
  the store's memory and such a statement is `limit` before it allocates
  anything. A remote server holds 1 GiB unless `--memory` says; a local one
  run without it keeps SQLite's gigabyte.
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

## Versions

One version names a release of everything, but a server and its clients
upgrade apart: a remote server deployed once while the applications' SDKs
move on, or a sidecar still serving a directory after its application's SDK
was updated, since it leaves only once it has been idle. So the protocol
promises what an older and a newer side do with each other.

- **A connection speaks the older protocol of its two sides.** `HELLO` says
  the newest the client speaks and `WELCOME` the one the connection speaks; a
  server refuses only a client older than the oldest it still speaks. A
  protocol moves only for what a field cannot carry: frames, the handshake,
  credit.
- **Within a protocol, a message grows by fields, and a request is understood
  whole or refused.** A field the server does not know is `unimplemented`,
  naming the field and the server's version: a newer client using something
  new learns which server to upgrade, rather than reading an answer to
  another question, as a read whose condition the server skipped would be. A
  client leaves a field out at its zero value, so one using nothing new talks
  to any server of its protocol.
- **An answer grows only by what a client may skip.** A client skips an
  answer's field it does not know; one whose meaning a client must know is
  answered only to a client that asked for it with a field of its request,
  which an older server refuses.
- **A method is added, never repurposed.** An older server answers a newer
  method `unimplemented`, naming its version. After the first release a
  message never changes meaning and a field's number is never reused.

An SDK's `status()` says what the `WELCOME` said: the server's version, the
protocol the connection speaks, its engines and the connection's capability.

Not built: a list of capabilities in `WELCOME`, for a client that must choose
before it calls rather than learn from `unimplemented`, which any release may
add, since it is a field a client may skip; an SDK saying when the sidecar it
found is another version than its own; and the matrix that keeps the promise,
CI running the previous release's SDKs against the new server and the new
SDKs against the previous server.

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
| credit on a stream, each way | 2 MiB: at least the largest body, which must fit it, and more than the 1 MiB a round trip needs where it is long | `WELCOME`, `HELLO` |
| calls running at once | as many as the streams in flight, on workers that keep their stacks | the server |
| a data connection's SQL statement, its check included | 30 s | the server |
| the collector's target | `GOGC` 400 unless the environment sets it, since a small live heap and a fast allocation rate collect hundreds of times a second at Go's default | `tinystore serve` |
| answers queued to write; a handler past it waits | 4 MiB | the server |
| connections | 64 local, 1,024 remote | flags |
| the handshake | 5 s | the server |
| silence before a `PING` | 60 s | the server |

These are proposals; [the round](#the-round) sets them. A client past a bound
breaks the protocol and loses its connection.

## What the engines gain

The server calls each engine's public API. What it needs and the API lacks is
added as ordinary API:

- **kv: a value as its row holds it.** Built: `OpenBucket[kv.Raw]` keeps
  nothing, an int64 or bytes, as the row does, since the server knows no SDK's
  types; `kv.Raw{Kind, Int, Bytes}` says which, and an empty string is empty
  bytes, not nothing. An SDK maps its language's values onto those three as
  `codecFor` maps Go's: bytes and strings to bytes, integers and bools to an
  integer, floats to their eight bytes big-endian, anything else to JSON, so a
  Go program and a Python one read each other's buckets. `OpenBucket[any]`
  stays JSON.
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

[The round](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-mechanics-2026-09-28.md) settled, on Windows and in a
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

Each promise above is a test once its code exists; those marked built pass:

| promise | gate |
|---|---|
| a frame past its agreed size is refused before its body is read | built: `FuzzFrames`, `TestAFrameLargerThanAgreedIsRefusedUnread` |
| a body the profile does not allow is refused | built: `FuzzMessages`, a refused vector for each rule |
| the vectors are the bytes | built: `TestVectors`, `TestFrameVectors`, `TestTheExamplesAreWhatTheMessagesWrite` |
| a client past its credit loses its connection, and the reader never waits | built: `TestAClientPastItsCreditIsCutOff` |
| every stream ends with one final frame, a cancelled one too | built: `TestEveryStreamEndsOnce` |
| answers queued during a write leave in the next | built: `TestQueuedAnswersShareAWrite` |
| a data client cannot change a schema | built: `TestADataClientCannotChangeTheSchema`, over the adversarial round's corpus; `TestEachLineOfTheCheckRefusesOnItsOwn`; `FuzzDataSQL` against both lines |
| the check ends a statement where SQLite does | built: `TestSQLiteEndsAStatementWhereTheCheckDoes`, `TestTheCheckReadsSQLitesTokens` |
| a database opens once, and every later open checks its migrations | built: `TestSQLOpenAppliesOnceAndChecksAfter`, `TestADatabaseTheProgramOpenedIsChecked` |
| an answer past the agreed body fails its stream, not the connection | built: `TestAnAnswerPastTheBodyIsALimit` |
| a lost connection aborts uploads and fails the attempts in hand | built: `TestALostConnectionAbortsUploadsAndFailsAttemptsInHand` |
| `SERVE` is written whole, only under `LOCK`, for its owner alone | built: `TestServeIsWrittenWholeUnderTheLock`, `TestADirectoryIsItsOwnersAlone` |
| a `SERVE` left behind goes before its server listens | built: `TestAServeLeftBehindGoesBeforeTheServerListens` |
| a client reading `SERVE` delays a change to it and does not fail it | built: `TestAChangeHeldUpByAReaderIsTriedAgain`, `TestAServeHeldPastEveryTryIsAnError` |
| a server goes idle only after its last connection | built: `TestAServerGoesIdleAfterItsLastConnection` |
| a socket's path fits, in the user's own directory when the store's is long | built: `TestALongSocketPathMovesToTheUsersOwnDirectory`, off Windows |
| a server proves it read `SERVE` to a local challenge, and to no other | built: `TestAServerProvesItselfOnlyToALocalChallenge`, `TestProofVectors` |
| an endpoint taken after its server left cannot prove itself | built: `TestAnEndpointTakenAfterItsServerLeftCannotProveItself` |
| a stale `SERVE` starts one sidecar, and the other exits held | built: `TestAStaleServeStartsOneSidecar`, `TestASecondServeOfADirectoryExitsHeld` |
| the sidecar found through `SERVE` leaves once idle, with `SERVE` and `LOCK` | built: `TestTheSidecarIsFoundThroughServeAndLeavesWhenIdle` |
| a private child leaves when its parent does, or when told though the parent stays | built: `TestAPrivateChildServesItsParent`, `TestAPrivateChildLeavesWhenToldThoughItsParentStays` |
| what serve cannot serve opens nothing | built: `TestServeRefusesWhatItCannotServe` |
| the tool requires only the store and the server | built: `TestTheToolRequiresOnlyTheStoreAndTheServer` |
| the server module requires only the root | built: `TestTheServerRequiresOnlyTheRoot` |
| `server/wire` imports only the standard library | built: `TestWireImportsOnlyTheStandardLibrary` |
| engines import neither `server` nor `server/wire` | built: `TestEnginesDoNotImportEachOther`, extended |
| a client newer than its server speaks the server's protocol | built: `TestAClientOfANewerProtocolIsWelcomedInTheServers` |
| a request is understood whole or refused, naming the field and the server's version | built: `TestARequestWithAFieldTheServerDoesNotKnowIsRefused`, `TestAMessageReadsWhatItKnowsAndNamesWhatItDoesNot` |

## Not in the first version

- Transactions held open across the network.
- An HTTP and JSON gateway for browsers and curl: a module of its own over the
  same handlers.
- A blob put from a file path when the sidecar shares the disk.
- Tenants, replication, QUIC, compression.
- The server's own metrics, which wait for self-metrics.

## SDKs

Built after the server, in `sdk/js`, for Bun and later Node, and
`sdk/python`: `tinystore` on npm, and `tinyshed-tinystore` on PyPI, imported
as `tinystore`, since PyPI's `tinystore` is another project's. What a program
sees, side by side with Go, and the rules that keep the three alike are
[sdk.md](sdk.md); this section is how they reach the server.

```ts
import { ConflictError, open } from 'tinystore'

await using store = await open('./data')                 // the directory's sidecar, found or started
// open('./data', { private: true })                     // a child on stdio, living and dying with this process
// connect('tls://db.internal:7443', { token })          // a remote server

const sessions = store.kv.bucket<Session>('sessions', { sliding: '30d' })   // JSON
const codes = store.kv.bucket('login-codes', 'int', { defaultTtl: '15m' })   // number
await sessions.of(user.id).set(token, { device }, { ttl: '1h' })
const s = await sessions.of(user.id).get(token)            // undefined when absent

const reminders = store.jobs.queue<Reminder>('reminders')
await reminders.enqueue({ user: 42, text: 'call mom' }, { at: evening })
await reminders.work(async job => remind(job.value), { workers: 8, signal })  // returning acks, throwing retries

const photo = await store.blobs.bucket('avatars').of(user.id).get('original') // photo.body: a ReadableStream
const app = await store.sql('app', { migrations: './migrations' })
const notes = await app.all<Note>`select * from notes where author_id = ${user.id}`
```

```python
async with tinystore.open("./data") as store:
    sessions = store.kv.bucket("sessions", Session, sliding=timedelta(days=30))
    s = await sessions.of(user_id).get(token)             # Session | None
    await reminders.work(remind, workers=8)
```

- **The Go API's vocabulary.** A type is given once, when a bucket or queue
  opens; the daily calls are plain verbs; what the engine chooses goes into
  open's options and never into a call. A plain verb returns the least and
  its entry twin the version, as kv's `Get` and `GetEntry` do.
- **A handle opens with its first call.** `bucket`, `counters`, `queue` and
  `schedule` return at once and send their open with the first call that
  needs it, and again on a new connection, so an application declares them
  where it builds its services; what refuses a name or an option without the
  server refuses at once. `open`, `connect` and `sql`, which applies or checks
  migrations, are awaited.
- **A value's type is given where its bucket opens.** TypeScript's types are
  gone when the program runs, so a bucket of a primitive names it by a word,
  `'int'`, `'float'`, `'bigint'`, `'string'`, `'bytes'`, `'bool'` or `'none'`
  for a set; any other holds JSON, its type the generic's, or a Standard
  Schema's, zod's or valibot's, which checks each value as it is read. Python
  gives the type itself: `int`, `float`, `str`, `bytes`, `bool`, `None`, a
  dataclass, a `TypedDict`, or a model with `model_validate`. Each maps onto
  nothing, an integer or bytes as `codecFor` maps Go's, so that every language
  reads every bucket.
- **No dependency when the program runs.** Each SDK has a MessagePack codec of
  the profile's own, which refuses what `refused` refuses, as no general
  library does; the server's binary is the one thing it carries. Python's C
  extension waits for a measurement that asks for it.
- **A sidecar found through `SERVE` proves itself before the first call**:
  the client's `HELLO` carries a fresh challenge, and no `REQUEST` leaves
  until `WELCOME`'s proof checks; `proofs` in the vectors is one to test by.
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
- **Python**: 3.12 or later, asyncio first; a program without asyncio gets
  `open_sync`, one call at a time, generated from the asyncio code as httpx
  generates its own, after the first version. **JavaScript**: `Bun.connect`
  for sockets, `node:net` for Windows pipes, `node:child_process` to start a
  sidecar that outlives its parent; Node takes all of it but the runtime's
  file, which the package's `exports` pick by the `bun` condition.

## Building it

In slices, each engine's messages fixed in [wire.md](wire.md) before its
code: `server/wire`; a session with its transports; kv, with a Go client the
tests use; jobs; blobs; sql, with the adversarial round of its SQL check;
records; metrics; `tinystore serve` with `SERVE`; the JS SDK; the Python SDK.
Where the slices stand, 29 September 2026:

| slice | state |
|---|---|
| `server/wire` | built: frames, profile, handshake, errors; vectors in `server/wire/testdata/vectors.json` |
| a session with its transports | built: `session.go`, `stream.go`, `workers.go`, `internal/flow`, Unix, TCP, TLS, `internal/pipe` |
| kv | built: `kv.go`, `kv.Raw` in the engine; the Go client is `server/internal/client`, a test's and a measurement's |
| jobs | built: `jobs.go`, `jobs_work.go`; the messages on wire.md |
| blobs | built: `blobs.go`; the messages on wire.md |
| sql | built: `sql.go`, the data connection's check in `sqltokens.go` and `datasql.go`; the messages on wire.md |
| records | built: `records.go`; the messages on wire.md |
| metrics | built: `metrics.go`; the messages on wire.md |
| `tinystore serve` with `SERVE` | built: `local.go`, `WaitIdle` in `server.go`, `internal/private`; `cmd/tinystore/serve.go` |
| the JS SDK, the Python SDK | built: `sdk/js` and `sdk/python`, every vector and every engine tested through a real `tinystore serve` (`task sdk`); not yet their READMEs, examples, packages of the binary, or a measurement against the prototype |
| the measurement against the prototype | done: [rpc-server-2026-09-29](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-server-2026-09-29.md), at depth 68 to 78 % of the prototype's best sidecar, much of the rest a point read's context; again with statements that start no goroutine for their contexts, [rpc-contexts-2026-09-29](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-contexts-2026-09-29.md): 84 to 87 % on Windows, the container waiting for Docker |

Every slice built passes `go test`, `-race` in the `golang:1.27` container
three times shuffled, and golangci-lint for Windows and Linux; nothing of it
is released, and the gates it brought are in AGENTS.md.

**What building sql settled**, beside the design above:

- **A migration history that does not match is `ErrInvalid`**: sqldb's `Open`
  and `Migrated` say it of a migration changed or renamed after it was
  applied, of a file that applied more than the migrations given and of
  another engine's file, as they said it of a pending one, so that a client
  learns `invalid` rather than `internal`.
- **A statement SQLite refuses is `ErrInvalid`**: sqldb says it of SQLite's
  `SQLITE_ERROR`, `SQLITE_RANGE` and `SQLITE_MISMATCH`, a syntax error, a
  table, column or function it does not have, and of a parameter no argument
  fills, which the driver reports in no type of its own; `SQLITE_TOOBIG` is
  `ErrLimit`.
- **A pending migration keeps its text, not its kind**: a data client's is
  `permission` and an admin's on an open database `in_use`, whose messages say
  what was pending.
- **Every answer is checked against the agreed body** before it leaves, a
  RESPONSE, a DATA item and a trailer alike, and one past it fails its stream
  with `limit`; a blob's bytes go in chunks no larger than it, which a client
  agreeing a body under 64 KiB would otherwise have refused as a broken
  protocol.
- **Named arguments are `sql.Named`'s**: the name without its prefix, which
  the driver matches against `:`, `@` and `$` alike.

**What building records settled:**

- **Records open no handle.** They are one log of the store's, whose calls
  name their streams, and the server's options are the engine's; the engine
  opens the first time a client asks, as the others do.
- **Fields travel as one array of keys and values**, so that keys keep their
  order and repeat, and every text whose bytes are not UTF-8, a program's
  output's, travels as bin.
- **A drop is a repair**, so it is an admin connection's alone; the damaged
  rows are anyone's to list, and a read or a follow that meets one names it
  in `what` as a drop names it.
- **An upload of lines keeps what reached the writer.** Its end, however it
  ends, closes the writer, which hands over the record it holds; a cancel or a
  lost connection drops the `DATA` still waiting for the handler, as an
  upload's inbox gives its error before the bodies it holds.
- **No streams or names in a query is every one**, an empty list as much as
  none, where Go's `Query` takes an empty `Streams` for none.

**What building metrics settled:**

- **A series may come in pieces.** A read's series of a hundred thousand
  samples is 1.6 MB of columns, past a body, so a series comes in `DATA` of
  half a body's samples at most, each with its labels, and a client joins
  them; so do an aggregate's buckets.
- **A read is whole before it is sent.** The server calls `Read` and
  `Aggregate`, not `Stream`, since a callback that waits for a client's credit
  would hold one of the engine's two read slots for as long as the client
  takes; the answer, bounded by the query's limits, waits in the handler
  instead.
- **Drop is a write, not a repair**, so a data connection may drop a series,
  as it may delete a kv key; the records drop, which removes only what no
  longer reads, is an admin's.
- **What a download holds until its last `DATA` is the store's memory.** An
  engine counts its answer while it reads it and lets go when it returns; the
  handler then holds it until the client has taken it, so the handler reserves
  its weight, strings and bytes and two words a value, before its first
  `DATA` and releases it when it returns: sql's rows, records' page and follow,
  metrics' series and buckets, and the pages of kv, blobs and jobs scans.

**What building `tinystore serve` settled:**

- **A directory held is exit code 3**, `ErrInUse` from the store's `Open`;
  any other failure is 1, and a server that left as asked, idle, told by a
  signal or its parent gone, is 0.
- **Leaving goes in order**: `SERVE` and the socket first, so that no client
  finds a server on its way out; then `Server.Close`, which gives the streams
  running ten seconds; then the store.
- **Idle counts connections, not calls.** `WaitIdle` returns once no
  connection has been open for `--idle`, counted from the last one's end, or
  from the server's start when none came.
- **The tool links the server.** `cmd/tinystore` requires the root and
  `server`, so an application's `go tool` directive selects a root at least as
  new as its tool's; the migrate commands still run the application's own
  test, whose sqldb is the one its module graph selects.

**Next, from here:**

- **A point read without a goroutine**, built: `internal/sqlite`'s
  `QueryRowByKey` checks the context and runs a statement that finds its row
  by a key with `context.WithoutCancel`, as
  [the measurement](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-server-2026-09-29.md#what-follows) proposed; a
  count or a sum over a range stays on `QueryRow`, which the context
  interrupts. Measured in
  [rpc-contexts-2026-09-29](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-contexts-2026-09-29.md) with a
  grouped write's statements run without their contexts too: 6 to 18 % more
  gets from Go at depth, 18 to 35 % more sets at 256, on Windows; the
  saved-revision Linux comparison and race suites are now complete in the
  [continuation round](https://github.com/tinyshed/research/blob/main/tinystore/reports/runtime-continuation-2026-10-01.md).
  Both SDKs passed their vectors and real-server suites. The round measures
  the checked ncruces port, rejects inline reader dispatch that blocks CANCEL,
  and records the Go client's upload termination and shared-credit fixes.
- **Checks to run**: `go test` in the root, `go -C server test` and
  `go -C cmd/tinystore test`; lint with `bin/golangci-lint` in all three, for
  Windows and with `GOOS=linux`; and the race suite in the container, as the
  earlier slices ran it, in `server` and again in `cmd/tinystore`:

  ```sh
  docker run --rm -v <repo>:/src -v tinystore-race-cache:/go -e GOWORK=off -e CGO_ENABLED=1 \
    -w /src/server golang:1.27 go test -race -count=3 -shuffle=on ./...
  ```

What building them settled, beside the proposals above:

- **A body fits the credit it is sent under.** A stream's credit is 2 MiB each
  way, at least the largest body, 1 MiB and 64 KiB, and a client granting less
  lowers the largest body the connection agrees.
- **A handler's frame that cannot be written ends the connection**, whose
  streams learn it through their context; a protocol error's `GOAWAY` is the
  connection's last frame, written before the server lingers a second for the
  client to read it, since closing TCP with bytes unread resets it.
- **Workers start as calls arrive** and live as long as their connection, as
  many as its calls running at once and never more than its streams in flight,
  so that no call waits for another to finish; a connection that never had
  more than one call running keeps one.
- **A malformed request fails its stream**, `invalid`, and the connection goes
  on; only a frame that breaks the protocol ends it.
- **A Windows named pipe's instance waits for the next client** before the one
  connected is handed over, its OVERLAPPED on the heap, since a goroutine's
  stack may move while the kernel holds it; the client dials with
  `SECURITY_IDENTIFICATION`, so a process that took the name cannot act as it.

- **Read first**: AGENTS.md, this page, [wire.md](wire.md) and
  [the round](https://github.com/tinyshed/research/blob/main/tinystore/reports/rpc-mechanics-2026-09-28.md). The prototype
  Research's `spike/rpc_*` is prior art to read, not code to import: production code
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
