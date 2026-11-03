# One budget of memory

A store runs inside your program, or beside it on the same machine. It must
not take the machine's memory when a report reads a year of logs, and it must
say so when it cannot answer, instead of answering less. This chapter explains
the bounds every call has, the budget a store can be given, and what a limit
error tells you.

## Every call is bounded

Every call that could hold a lot of data has a bound of its own, whatever
else is going on: the rows of one SQL read or one search, a page of a list,
the buckets of an aggregate, the samples of an ingest, a value, the messages
of every channel. [Limits and defaults](../production/limits.md) gives each
one. An upload needs no bound of its own: it holds a few buffers of 64 KiB to
1 MiB, whatever the file's size.

A call past its bound fails with a `limit` error. It never returns a smaller
answer that looks complete.

## A budget for the whole store

A bound per call does not stop ten calls at once from each taking their 64
MiB. A store can be given one budget of bytes that all its work shares:

```rust
let store = Store::open("data", Options { memory: Some(512 << 20), ..Options::default() })?;
```

```sh
tinystore serve ./data --memory 512MiB
```

In Bun, Node, Python and Go, `open` takes no budget. To give a store one,
run its server yourself with `--memory`, and `open` finds that server.

With a budget, work reserves the bytes it will hold before it allocates them,
and gives them back when it is done. A call whose reservation does not fit
waits a while for other work to give memory back, then fails with `limit`. A
server that listens on the network has a budget by default, because its
callers are not under your control. [Limits and
defaults](../production/limits.md#the-store) has both numbers. Without a
budget, as in an embedded store or the sidecar by default, every call still
has its own bound. The store never reads a container's memory
limit to make a budget of its own: inside a container, give `memory` (or
`--memory`) a part of the container's limit yourself (contract STORE-05).

The budget counts what the store's work holds: the rows of an answer, a block
being decoded, an upload's buffers, the series of a Prometheus text while it
is read and written, which it reserves before it reads a line. It is not a limit on the process's
resident memory, which also holds your program and SQLite's page caches.

Background work fits inside the budget too. Packing a stream's events into a
segment holds four times what the segment weighs, so under a budget smaller
than 16 MiB records packs and merges smaller segments, a quarter of the budget
each, instead of failing to pack them.

## What a limit error says

A `limit` error names which bound it met, what the call wanted and what the
bound was:

| The error's `limit`      | It means                                                     | What to do                       |
|--------------------------|--------------------------------------------------------------|----------------------------------|
| `bytes of a call's rows` | one call's rows passed 64 MiB                                | read a page, or `each`           |
| `store memory`           | one call alone wanted more than the whole budget             | read a page, or raise the budget |
| `store memory, now`      | the call did not fit beside what others hold or wait for now | try again, or do less at once    |

The last one is the only one that a retry can fix. The first two say the
call is too big, and will be too big every time. Every other bound a call
meets is listed by its name in
[Limits and defaults](../production/limits.md#bounds-by-name).

## Why this design?

**Why refuse instead of answering less.** An answer cut silently is the worst
kind of bug: tests with small data pass, and production quietly shows a
user's dashboard with data missing. A refusal is loud, names its bound, and
says what to do.

**Why reserve before allocating.** If work allocated first and checked later,
the check would come after the memory was already gone. Reserving first means
a call that does not fit fails before it takes anything, and the store's
other work goes on.

**Why work that holds a writer never waits for memory.** A commit that waited
for memory while other work waited for that commit would deadlock. So work
that holds a file's writer takes its memory or fails at once, and only reads
wait. A large reservation is never passed over forever by small ones: waiters
are served in order (contract STORE-05).

## See also

- [SQLite as TinyStore runs it](sqlite.md): readers that close when idle,
  which also keeps memory down.
- [Time and errors](time-and-errors.md): every kind of error, `limit` among
  them.
