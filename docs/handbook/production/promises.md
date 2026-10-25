# What TinyStore promises

Every guarantee in this book has an id, such as DUR-01 or KV-11. The ids come
from `CONTRACTS.md`, the list of what TinyStore promises the program that
opens it. Each promise is kept by at least one test that names its id, and a
build fails when a promise has no test. This chapter groups the promises a
program relies on most, and ends with what TinyStore does not promise.

## Your data

| Id      | TinyStore promises                                                                 |
|---------|------------------------------------------------------------------------------------|
| DUR-01  | a write that returned success survives the process dying, at the file's durability |
| DUR-02  | a transaction commits all its changes or none                                      |
| DUR-03  | writes grouped into one commit each fail alone, unless one rolls back the commit   |
| DUR-05  | a write whose outcome cannot be known answers `outcome unknown`, never success     |
| DUR-06  | a write is atomic within one file, and across two only when you joined them        |
| DUR-08  | a reader sees one consistent snapshot, every commit whole                          |
| DUR-09  | packing, merging and expiry never change an answer                                 |
| KV-01   | a value comes back bit for bit, or fails `corrupt`, never a plausible wrong value  |
| BLOB-01 | a file appears whole at its commit, or not at all                                  |
| BLOB-02 | a whole read is checked against the file's SHA-256                                 |

## Your errors

| Id       | TinyStore promises                                                               |
|----------|----------------------------------------------------------------------------------|
| ERR-01   | every failure is one of a few kinds, the same in every engine and language       |
| ERR-02   | an error names what failed and keeps its cause                                   |
| ERR-05   | a call past a bound fails `limit`, naming the bound, and never answers less      |
| ERR-06   | a call that fails `limit` or `invalid` changes nothing                           |
| ERR-07   | damage to one item never stops its neighbours                                    |
| STORE-09 | a read you cancel, or whose timeout passes, stops at once and frees what it held |

## Your work in the background

| Id      | TinyStore promises                                                      |
|---------|-------------------------------------------------------------------------|
| JOB-01  | a job never runs before its time, and due jobs start in time order      |
| JOB-02  | a job added in a transaction commits with its rows, or not at all       |
| JOB-05  | a job whose lease ended runs again, and its stale lease settles nothing |
| JOB-06  | a step runs once per job, across its attempts                           |
| KV-11   | `once` runs a key's function once and keeps its answer for the others   |
| LOCK-01 | a key has one holder at a time, given to waiters in order               |
| LOCK-02 | each lease's fence is larger than every earlier one                     |
| CHAN-02 | a publisher never waits, and a slow subscriber is told what it missed   |

## Your languages and your security

| Id      | TinyStore promises                                                              |
|---------|---------------------------------------------------------------------------------|
| WIRE-01 | every SDK answers as the Rust core does, embedded, as a sidecar or remote       |
| WIRE-05 | a write whose connection ended before its answer surfaces as `outcome unknown`  |
| WIRE-10 | a read whose client cancels it, or leaves, stops on the server too              |
| WIRE-11 | an `open` or `connect` you cancel stops at once and keeps no connection         |
| SEC-01  | a read token only reads                                                         |
| SEC-02  | a remote connection shows its token; a local server proves itself to its client |
| SEC-03  | an encrypted bucket's file shows no value                                       |
| SEC-06  | no other user of the machine reads `SERVE` or connects to a local server        |
| BAK-02  | a restore checks every byte, and leaves nothing when it fails                   |

## What TinyStore does not promise

| Id     | Not promised                                                                                      |
|--------|---------------------------------------------------------------------------------------------------|
| NOT-01 | a write across two files is not atomic, unless you opened one engine inside the other's file      |
| NOT-02 | channels deliver at most once, in memory: a message nobody hears, or before a restart, is gone    |
| NOT-03 | locks live in the memory of the process that holds the store, and do not coordinate two stores    |
| NOT-04 | a copy's reads are stale by its lag                                                               |
| NOT-05 | a promotion made while the old home still writes may lose what the old home took after it         |
| NOT-06 | `committed locally` means no copy held the write, and a failover may lose it                      |
| NOT-08 | a metric sample behind its series' sealed frontier is refused, unless it is the one kept there    |
| NOT-09 | a log line that finds its buffer full is dropped and counted                                      |
| NOT-10 | a vector search with `recall` may miss some of the exact best                                     |
| NOT-11 | nothing before `v0.1.0` is compatible: formats, schemas and the protocol change without migration |
| NOT-12 | events of one time that v0.1.0 packed keep its order, not always the order they were appended     |

A job runs at least once, not exactly once, and `once` runs a key at most
once while its answer stands, not an effect exactly once. Both are said in
their chapters, with the code that makes a second run harmless.

## Why promises with ids

A guarantee written only in prose drifts: someone changes the code, a test
still passes, and the sentence in the docs is no longer true. A promise with
an id is tied to the tests that keep it, and the build refuses a promise that
no test keeps. When a promise must change, its line in `CONTRACTS.md`
changes in the same commit, so the change is visible to everyone who reads
the history.

## See also

- [Limits and defaults](limits.md): every bound in one place.
- [Time and errors](../inside/time-and-errors.md): every kind of error and
  what to do about it.
