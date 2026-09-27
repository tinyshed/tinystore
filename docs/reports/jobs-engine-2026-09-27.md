# The jobs engine under a burst — 2026-09-27

The engine of [the first slice](../jobs.md), measured on what
[the mechanics round](jobs-mechanics-2026-09-27.md) left and on what its gates
found: a Work loop that wrote twice for a job, how far it claims ahead of its
workers, a keyed burst, what opening a large queue costs, and a handler's
writes through sqldb. Each comparison runs both sides in one container session
on identical input, alternated twice.

## For a reader in a hurry

A burst due at once drained through `Work`, the handler returning at once, no
keys; jobs a second, and jobs a commit.

| workers | before: one job a worker | the loop fixed: one job a worker | two a worker, the default | four a worker | the round's prototype |
|---:|---:|---:|---:|---:|---:|
| 1 | 182 (0.5) | 350 (1.0) | 472 (1.4) | 785 (2.4) | 371 |
| 8 | 928 (2.7) | 1,390 (4.0) | 2,650 (8.0) | 5,053 (16) | 1,387 |
| 64 | 6,345 (21) | 8,955 (32) | 15,328 (64) | 23,562 (129) | 10,149 |
| 512 | 24,624 (174) | 32,144 (273) | 41,820 (649) | 47,339 (990) | 50,538 |

The same with a random key on every job:

| workers | before | fixed, one a worker | two a worker | four a worker |
|---:|---:|---:|---:|---:|
| 1 | 181 | 352 | 485 | 777 |
| 8 | 934 | 1,361 | 2,461 | 4,428 |
| 64 | 5,060 | 6,877 | 10,899 | 15,358 |
| 512 | 14,772 | 17,376 | 20,990 | 23,145 |

The second run of each side came within 5 % of the first in most cells and
within 17 % at the widest, eight workers with keys holding two; both runs are
under [Both runs](#both-runs).

| | |
|---|---|
| opening a queue of a million jobs, which counts them | 105 to 111 ms, the file in the page cache |
| a keyed burst of a million, key index on the jobs' table, prototype | 22,192 jobs a second in batches of 1000 |
| the same, keys in a table of their own dropped later, prototype | 57,795 a second, and 333,602 keys dropped a second |
| sqldb inserts at 1, 8, 64 and 512 goroutines, a transaction each | 300, 302, 301, 304 a second |
| the same through `Exec`, grouped | 301, 1,447, 10,663, 63,727 a second |

## What the numbers say

- **One worker wrote twice for each job.** A worker still held its finished
  job when the loop counted free workers, so the write that settled the job
  claimed nothing and the next write claimed its successor: 0.5 jobs a commit,
  182 a second, half the round's prototype. Counted free in the write that
  settles its job, a worker's next claim shares that commit: 1.0 a commit and
  350 a second. An acknowledgement also deletes the job in one statement
  unless it repeats or was asked to run again, where it read the row first.
  Together they are 1.9 times at one worker and 1.3 at 512.
- **Claiming ahead is the larger lever.** A loop that holds a worker's next
  job while the worker runs the one before carries that job in the commit
  that settles the last: two jobs a worker is 1.3 to 1.9 times one, and four
  is 1.1 to 1.9 times two. The loop holds two, the default: a job claimed
  ahead is leased, so `Update` and `Cancel` find it too late, and a crash
  counts its attempt though it never ran, and four doubles both for what is
  mostly a gain at eight workers and fewer.
- **The engine now passes its prototype from one to 64 workers** and stays
  below it at 512, 41,820 against 50,538, where every job costs a lease
  object, a JSON decode and the statements the prototype did not run.
- **A random key halves a burst at 512 workers**, 23,145 against 47,339 at
  four a worker: acknowledging a keyed job deletes an entry of the key index
  at a random page. The prototype that keeps keys in a table of their own,
  leaves them behind on acknowledgement and drops them later in key order
  drained a million keyed jobs 2.6 times as fast, 57,795 a second against
  22,192, and dropped the million keys it left in 3.0 s, 101 transactions,
  the slowest 48.6 ms. Its key read took 18.1 µs against 8.5, a join against
  an index, and its enqueues ran at 13,417 a second against 21,544; that run
  shared the host with compiles of this session's code, and the enqueue of
  that prototype went through a trigger, where the engine would write the key
  itself. The engine keeps its index until a run on an idle host with the
  engine's statements says what the change costs an enqueue.
- **Opening a large queue is not free.** `OpenQueue` counts the queue's jobs
  for `MaxWaiting` by reading every page of the queue: 105 to 111 ms for a
  million, from the page cache, so ten million cost about a second before the
  queue's first call.
- **sqldb's grouped `Exec` holds the round's prototype.** From 8 goroutines
  on, statements that wait for the writer commit together: 63,727 inserts a
  second at 512 against 63,790 in the round's prototype, and against 304 a
  second, a commit each, which is what `Exec` did before and a `Tx` a call
  still does.

## Keys kept apart, on an idle host

The keyed prototype ran again after the blobs round, the host doing nothing
else and its enqueue writing a job's key with a statement of its own, as the
engine would, instead of the trigger of the first run. It supersedes that
run's enqueue figure:

| a million keyed jobs, shuffled, due within one minute | key index on the jobs' table | keys in a table of their own |
|---|---:|---:|
| enqueued, 10,000 a transaction | 20,413 a second | 20,998 |
| a key read | 9.0 µs | 12.7 µs |
| drained in batches of 1000 | 21,367 a second | 58,176 |
| the keys left behind dropped, 10,000 a transaction | — | 2.9 s, the slowest 43 ms |

Keys kept apart cost an enqueue nothing, a key read 3.7 µs, and drain a keyed
burst 2.7 times as fast.

## Environment and reproduction

- AMD Ryzen 7 7700, 8 cores / 16 logical processors, Samsung 990 PRO NVMe;
  Docker Desktop 29.6.2 on WSL2, 16 CPUs and 15 GiB visible; `golang:1.27`,
  go1.27.1 linux/amd64; the files on a named volume. `modernc.org/sqlite`
  v1.59.0, `synchronous=FULL`, WAL, 4 KiB pages, the 1 MiB page cache a
  connection of `internal/sqlite`.
- The loop before at `85bcebe`, the loop fixed at `199aa9a`, each built from
  its commit with the burst test of `85bcebe`; a burst of `min(1000 + 1000 ×
  workers, 200,000)` jobs due at once, enqueued shuffled in transactions of ten
  thousand, a message of about 190 bytes each.
- Opening a queue, the keyed prototype and sqldb's inserts at the commit that
  adds this report, one run each.

```sh
git -C <repo> archive 85bcebe | tar -x -C <base>
git -C <repo> archive 199aa9a | tar -x -C <candidate>
cp <base>/jobs/burst_test.go <candidate>/jobs/
docker run --rm -v <base>:/base -v <candidate>:/cand -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off golang:1.27 sh -c '
  cd /base && go test -c -o /perf/base.test ./jobs && cd /cand && go test -c -o /perf/cand.test ./jobs
  for bin in base cand base cand; do
    TINYSTORE_SPIKE=1 TINYSTORE_JOBS_DIR=/perf /perf/$bin.test -test.run "^TestWorkBurstMeasured$" -test.v
  done'

docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off -e TINYSTORE_SPIKE=1 \
  -e TINYSTORE_JOBS_DIR=/perf -e TINYSTORE_SQL_DIR=/perf -w /src golang:1.27 sh -c '
  go test ./jobs -run "^TestOpenQueueMeasured$" -v -count=1 &&
  go test ./spike -run "^TestJobsKeys$" -v -count=1 -timeout 60m &&
  go test ./sqldb -run "^TestExecsMeasured$" -v -count=1'
```

## Both runs

Jobs a second, first run and second, without keys and then with them.

| workers, held a worker | before | fixed |
|---|---:|---:|
| 1, one | 182 · 176 | 350 · 350 |
| 1, two | 281 · 270 | 472 · 508 |
| 1, four | 465 · 483 | 785 · 856 |
| 8, one | 928 · 952 | 1,390 · 1,391 |
| 8, two | 1,779 · 1,737 | 2,650 · 2,712 |
| 8, four | 3,332 · 3,412 | 5,053 · 5,007 |
| 64, one | 6,345 · 6,095 | 8,955 · 8,934 |
| 64, two | 10,687 · 10,444 | 15,328 · 15,322 |
| 64, four | 17,951 · 17,535 | 23,562 · 23,981 |
| 512, one | 24,624 · 24,155 | 32,144 · 31,785 |
| 512, two | 32,237 · 32,021 | 41,820 · 40,550 |
| 512, four | 38,661 · 37,455 | 47,339 · 44,533 |
| keyed 1, one | 181 · 176 | 352 · 346 |
| keyed 1, two | 282 · 283 | 485 · 479 |
| keyed 1, four | 479 · 488 | 777 · 875 |
| keyed 8, one | 934 · 934 | 1,361 · 1,381 |
| keyed 8, two | 1,885 · 1,714 | 2,461 · 2,875 |
| keyed 8, four | 3,617 · 3,115 | 4,428 · 4,953 |
| keyed 64, one | 5,060 · 5,006 | 6,877 · 6,968 |
| keyed 64, two | 8,318 · 8,339 | 10,899 · 10,991 |
| keyed 64, four | 12,047 · 12,314 | 15,358 · 14,990 |
| keyed 512, one | 14,772 · 14,526 | 17,376 · 17,346 |
| keyed 512, two | 17,260 · 17,390 | 20,990 · 21,020 |
| keyed 512, four | 20,649 · 20,063 | 23,145 · 22,865 |

## What this leaves

- **Keys a burst does not scatter**: measured again on an idle host, keys
  kept apart cost an enqueue nothing and drain a keyed burst 2.7 times as
  fast, which is the engine's next change.
- **The engine against a table an application polls by hand**, on the five
  cases: what the engine buys over the way it replaces.
- **A queue's count kept rather than read at open**, when a queue of ten
  million opening in a second matters to someone.
- **The writer's page cache in a burst over a large file**: every burst here
  ran with 1 MiB a connection.
