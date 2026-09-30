# Group commit rules

`internal/sqlite.File.UpdateGrouped` follows these rules, and the kv engine
writes through it since 26 September 2026. There is no actor: the first
caller to find no leader commits the queue and hands the lead on. The writes
that arrive during one commit make the next, and since 30 September 2026 its
leader gathers before it takes them: it waits until the queue holds as many
writes as the last batch answered, or for a quarter of that batch's commit and
never past 2 ms, whichever comes first. The callers a commit answers need tens
of microseconds to write again, and a leader that took the queue at once left
them to the commit after, so 64 writers made groups of about 32 and a sync
carried half what it could; gathering took 64 writers from 10,000 to 14,900
grouped writes a second on WSL2 and 8 from 1,400 to 2,550, and one writer alone
never waits. A group holds at most 1024 writes and 8 MiB, a heavier write
alone, and at most ten seconds of the writer; kv bounds the writes waiting
through its admission slots. `TestAGroupGathersTheWritesItsLastBatchAnswered`
is the gate. The measurements are [the kv round](https://github.com/tinyshed/research/blob/main/tinystore/reports/kv-mechanics-2026-09-26.md).
Metrics' `Ingest` still commits one call in one immediate SQLite transaction;
checkpoint policy is a separate decision.
The [batching round](https://github.com/tinyshed/research/blob/main/tinystore/reports/grouping-ceiling-2026-09-23.md) measured a large
successful-path benefit from explicit caller batching but no actor's queue or
failure path. Keep explicit batching as the implementation choice until
independent low-volume requests justify an actor that satisfies these rules.

An actor belongs to one Store and its one writer connection. It may group
independent `Ingest` calls into one outer transaction, preserving their FIFO
admission order. Each call keeps a savepoint. A call-specific validation or
capacity failure rolls back that savepoint and reports an error only to that
caller; subsequent calls see the state after successful earlier savepoints.
Errors that make the connection or outer transaction uncertain roll back the
whole group and fail every waiter. No successful waiter is acknowledged before
the outer `COMMIT` succeeds under `synchronous=FULL`.

Cancellation before the actor starts a call removes that call from the queue
without writing it. Once its savepoint begins, the call is committed or rolled
back under the actor's bounded transaction context; caller cancellation cannot
turn a committed write into a reported clean cancellation. The caller waits for
the definitive transaction result. If the outer `COMMIT` fails and durability
is uncertain, every affected caller receives a distinct outcome-unknown error,
the writer connection is closed, and no per-handle statistics or cached state
are published as committed. A caller must reconcile before retrying an
outcome-unknown batch.

A started call's statements do not run under its caller's context. SQLite
rolls back the whole transaction of a write statement it interrupts, so a
caller cancelling while its statement ran used to fail every write of its
group; since 29 September 2026 a grouped statement runs without its caller's
cancel or deadline, and the group's savepoints without the hold's.
`sqlite.UntilDeadline` gives the application's SQL, which sqldb runs and which
may never end, its caller's deadline back: past it the statement ends and the
group fails with it, each write told so. A cancel still lets it finish.
`TestAWriteThatHasStartedFinishesWithItsGroup` and
`TestAStatementUntilItsDeadlineEndsThere` are the gates.

The queue must have explicit request and byte caps, a maximum gathering delay,
and a maximum writer hold time. A request holds per-store and shared admission
through preparation, queueing and acknowledgement. Full queues apply
context-cancellable backpressure. If one request exceeds the group byte cap
but is otherwise valid, it runs alone; it is never split into partially
committed calls. A slow or failed request must not hold unrelated requests
after its bounded savepoint or the outer transaction terminates.

The acceptance comparison uses identical one-sample, 100-sample and large
batches on saved revisions, and records throughput, p50/p99 latency, successful
commits, WAL frames, CacheWrite/Spill, syscall sync counts and cancellation at
each boundary. The actor must preserve `Ingest` atomicity, last-input-wins,
postings, ready marks and reopen behavior. Until those tests and measurements
exist, this is a contract for a candidate, not shipped behavior.

Checkpointing starts with observed WAL frames and PASSIVE/automatic
checkpointing. `RESTART` at a size threshold can wait behind a reader and must
not be attached to the writer actor without a separate latency comparison.
