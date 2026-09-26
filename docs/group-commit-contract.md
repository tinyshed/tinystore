# Group commit rules

`internal/sqlite.File.UpdateGrouped` follows these rules, and the kv engine
writes through it since 26 September 2026. There is no actor: the first
caller to find no leader commits the queue and hands the lead on. There is no
gathering delay: the writes that arrive during one commit make the next. A
group holds at most 1024 writes and 8 MiB, a heavier write alone, and at most
ten seconds of the writer; kv bounds the writes waiting through its admission
slots. The measurements are [the kv round](reports/kv-mechanics-2026-09-26.md).
Metrics' `Ingest` still commits one call in one immediate SQLite transaction;
checkpoint policy is a separate decision.
The [batching round](reports/grouping-ceiling-2026-09-23.md) measured a large
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
