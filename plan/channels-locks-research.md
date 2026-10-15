# Channels and Locks research handoff — 15 October 2026

The standalone round reviewed `rust` commit
`76b844cdc8c3410e746a5d5dc3e9aafec83c9962`. This branch contains only the
research pointer; the product implementation and accepted API are unchanged.
The owner requested the prototypes and reports on `tinyshed/research` main,
with product-side work isolated on a branch from rust.

[Implementation handoff](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-locks-handoff-2026-10-15.md)
links the committed harness, raw repeated measurements, environment and
remaining correctness gates. Its topic reports are:

- [Channels: fan-out, gaps and literal path subscriptions](https://github.com/tinyshed/research/blob/main/tinystore/reports/channels-2026-10-15.md).
- [Locks: conditional ownership, waiter admission and fencing](https://github.com/tinyshed/research/blob/main/tinystore/reports/locks-2026-10-15.md).
- [FTS5/R*Tree: write, WAL and rebuild cost](https://github.com/tinyshed/research/blob/main/tinystore/reports/indexes-2026-10-15.md).

The proposed Channels baseline is a runtime-neutral shared bounded log with
cursors, sticky gap/reset state, selector routing and credit-aware drains.
The custom mutex ring did not justify a lock-free implementation. Publication
rate without delivered-event/loss counts is not delivery throughput.

The proposed Locks baseline is persisted conditional state with exact
owner/fence/deadline checks and bounded per-resource waiter admission. Queue
admission cuts failed attempts but can reduce commit grouping at low load;
the synthetic rates do not establish an inherent SQL-over-KV advantage.

Jobs' implementation is a useful starting seam, not the complete lifecycle
of a public lock. Its renewals compare attempts without a live-TTL predicate,
and startup clears process-owned job leases. Its group permits must remain
atomic with claims in their own file, including an application's SQL file;
moving them to kv.db would break that boundary. Share a file-local primitive
and keep policy in the caller.

Transaction event staging must follow savepoint rollback, and after-return
callbacks do not by themselves preserve commit order. Stream credit and a
connection-wide byte budget must bound the wire output as well as the core
event log. A background SQLite rebuild still occupies the one writer; the
frontier/chunked generation algorithm in the handoff remains unimplemented.

These are research recommendations and implementation gates. No phase 5/6
checkbox is closed by this round; socket/SDK throughput, distributed leases,
HA election and chunked rebuild correctness remain unproved.
