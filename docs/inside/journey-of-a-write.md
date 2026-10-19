# The journey of a write

`await sessions.set(key, session)` returns after the session is on the disk,
and a hundred requests doing the same at once share a handful of disk syncs.
This chapter follows one write from a TypeScript program down to the SQLite
file and back, layer by layer, and says why each layer is there.

## The layers

```text
your program        sessions.set('k7q2', session)
  SDK               encodes the call as a message of the wire protocol
  transport         the pipe in this process, or a socket to the sidecar, or TLS to a server
  session           decodes the message and finds the bucket's handle
  engine            kv: turns the call into a write of kv.db
  writer            queues the write; one caller commits the queue
  SQLite            one transaction, a savepoint per write, one sync of the WAL
back up             the answer goes back the same way, and the promise resolves
```

A Rust program skips the first three layers: it calls the engine directly,
in its own thread, with no bytes in between.

## Through the protocol

An SDK does not know how buckets work. It knows how to write one message per
call: a frame of twelve bytes, then a body in MessagePack, a map from small
integer keys to values. `set` becomes a `REQUEST` frame with the method
`kv.set` and a body that holds the bucket's handle, the key and the value's
bytes.

Where the frame goes depends on how the store was opened:

| Opened as             | The frame goes                                               |
|-----------------------|--------------------------------------------------------------|
| `embedded`            | through four C functions into the core, in the same process  |
| the sidecar (default) | through a Unix socket or a Windows named pipe to `tinystore` |
| `private`             | through the stdin of a child process the program started     |
| `connect`             | through TLS to `tinystore serve` on another machine          |

The bytes are the same in all four cases. That is why a program moves from
one way to another by changing its `open` line, and why chapter 22, One core,
four languages, can describe every SDK at once.

## Into the writer

The session decodes the message, finds the bucket by its handle and calls
kv's `set`. kv turns it into an `insert … on conflict do update` of `kv.db`,
with the value encoded and the key's expiry computed from the store's clock.

That write does not run yet. It joins the queue of `kv.db`'s writer. Every
file of the store has exactly one write connection, and every write to the
file goes through it:

```scene
code ts
  await sessions.under(userId).set(digest(token), session)
end
actor a      "Request A" at 8,18  icon=server
actor b      "Request B" at 8,50  icon=server
actor c      "Request C" at 8,82  icon=server
actor queue  "kv.db queue" at 48,50 icon=list-checks
actor disk   "kv.db"     at 90,50 icon=hard-drive
link a queue
link b queue
link c queue
link queue disk
set disk "commits: 40"

step "Request A writes while kv.db's writer is idle. A finds no leader, so A becomes the leader and starts a commit." line=1
  send a -> queue "set"
  say a "leader"

step "While A's commit is syncing, B and C write. They find a leader at work, so they queue and wait." line=1
  send b -> queue "set"
  & send c -> queue "set"
  set queue "waiting: B, C"

step "A's commit ends. A wakes B, the first in line, and returns to its own caller." line=1
  send disk -> a "durable" color=ok
  set disk "commits: 41"
  say b "leader"

step "B commits everything that waited: its own write and C's, each in a savepoint, with one sync of the disk." line=1
  send queue -> disk "B + C, 1 sync"
  set disk "commits: 42"
  send disk -> b "durable" color=ok
  & send disk -> c "durable" color=ok
```

The first caller that finds no leader commits the whole queue in one
transaction, then hands the lead to the next caller still waiting. No thread
of the store's own does this: the callers take turns. A write that came
through the pipe has no caller waiting on a thread. It carries a completion
instead, a callback that delivers its answer, and the leader calls it.

Before it commits, a leader waits a little for more writes: at most a quarter
of the last commit's time. The callers a commit just answered need a few
microseconds to wake and write again, and a leader that took the queue at
once left them all to the next commit.

## Inside the commit

```text
begin immediate
  savepoint grouped   write A   release grouped
  savepoint grouped   write B   release grouped
  savepoint grouped   write C   rollback to grouped   ← C broke a constraint
commit                                              ← one sync of the WAL
```

Each write runs in a savepoint of its own. A write that fails, a broken
constraint or a value the bucket cannot keep, rolls back its savepoint, and
the others commit (contract DUR-03). The commit syncs the write-ahead log
once, and SQLite's WAL means a sync is one append to the end of one file.

The transaction begins `immediate`, taking the write lock at once. A
transaction that began deferred and tried to upgrade on its first write would
get a "busy" that no timeout can wait out.

## Back to the program

The commit returned, so every write in it is durable. The leader calls each
write's completion with its answer. For a write that came through the pipe,
the session writes a `RESPONSE` frame, and the core calls the host's `wake`
function: Bun's threadsafe callback, Node's threadsafe function, or nothing in
Go and Python, where a thread already waits in `recv`. The SDK reads the frame
and resolves the promise.

No thread of the host's event loop ever waited for the disk. A Node or Bun
process with one thread and a thousand writes in flight fills a commit with
all of them.

If the sync itself fails, the leader cannot know whether the commit reached
the disk. Every write in it then answers `outcome unknown` (contract DUR-05),
never success and never an ordinary failure. Read what you wrote before you
write again.

## Why this design?

**Why one writer per file.** SQLite lets one connection write a file at a
time, whatever you do. Two connections that try at once wait on a file lock
and retry, and under load one of them gets `database is locked`. TinyStore
says it in the code instead: one write connection per file, behind a mutex,
so a busy moment is a short orderly line and never an error.

**Why callers take turns instead of a writer thread.** A dedicated writer
thread adds a hand-off to every write, two context switches, and a thread per
file that does nothing most of the time. With leaders, the thread that
already holds the CPU does the work, and a burst of writers keeps one of them
busy committing while the others queue.

**Why a savepoint per write.** Grouping writes into one commit is what makes
them cheap: the sync is the slow part, and a hundred writes share it. Without
savepoints, one bad write would fail the hundred. With them, each write
succeeds or fails alone, exactly as if it had its own transaction.

**Why a transaction does not join a group.** A transaction's code reads, then
decides what to write. It must hold the writer while it decides, so it cannot
wait in a queue with writes that are known in advance. That is why `db.tx`
holds the writer alone, pays its own sync, and is bounded at five seconds.
Writes known before they run, `exec` and `batch`, share commits.

**Why the protocol even inside one process.** The pipe could have been a C
API with a function per call. Then every new call would need a new function
in every binding of every language. Carrying whole protocol frames keeps the
binding at four functions forever, and makes the embedded store and a remote
server the same code to test.

## See also

- [SQLite as TinyStore runs it](sqlite.md): the writer, the readers and the
  rules every connection follows.
- [One core, four languages](one-core.md): the protocol, the pipe and the
  sidecar.
- [Messages](../chat/messages.md#many-messages-at-once): a shared commit from
  the program's side.
