# What jobs' mechanics cost — 2026-09-27

The round [docs/jobs.md](../jobs.md) asks for before the jobs engine's
storage is settled: where the jobs of one minute lie in the file and what
draining a million of them costs, where a lease lives, how the file divides by
object, what an Enqueue costs, what a Work loop and its wake-up cost, what a
handler writing the application's database waits for, and what the raw Claim
and Ack calls hold. Nothing here is the engine: it is the prototype
`spike/jobs_*`.

| Question | Answer in the container | Decision |
|---|---|---|
| a million jobs due within one minute, drained in batches of 1000 | 94,000 a second when they arrived in time order; 17,000 when they arrived shuffled into a table in arrival order; 55,000 to 93,000 from a table ordered by time | rows ordered by `(queue, next, id)` |
| where a lease lives | a table of its own: 93,000 a second in the burst, 50,500 through Work, a crashed process's leases given back in 3 ms; the row moved to the lease's end: 55,000 and 40,800, but 21,000 against 8,900 for 512 lone claims | a lease in a table of its own |
| one job a transaction | 350 to 370 a second, every layout: an fsync each | claims and settlements in batches |
| a value's bytes | up to 512 in the row; at 1024 a row overflows its page, 4,690 bytes a job against 1,404 spilled | spill past 512, as kv |
| a keyed burst | 14,000 a second on both layouts: each job leaves the key index at a random page | open: lazy key cleanup |
| Enqueue, 512 goroutines | 50,000 a second due now; 16,700 at random times into the table ordered by time, 22,700 into the one in arrival order | the time order is paid at enqueue |
| from an Enqueue to its handler | 7.8 ms at the median, 9.6 at p99; a job due later starts 7.2 ms after its time | one commit to lease, no polling |
| a handler's insert into another database | 300 a second a transaction each at any concurrency, 63,800 grouped at 512 | sqldb commits its Execs grouped |

## Environment and reproduction

- The prototype at `f83f6dc`, over the design at `a067fcf`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, NVMe; Docker Desktop
  29.6.2 on WSL2, kernel 6.18.33.2-microsoft-standard-WSL2, 16 CPUs and
  15.2 GiB visible; `golang:1.27`, go1.27.1 linux/amd64; the files on a named
  volume. `modernc.org/sqlite` v1.59.0, `synchronous=FULL`, WAL, 4 KiB pages
  and `internal/sqlite`'s 1 MiB page cache a connection.
- One run; a load lasts three seconds unless a table says otherwise.

```sh
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_JOBS_DIR=/perf \
  -w /src golang:1.27 go test ./spike -run '^TestJobs' -v -count=1 -timeout 180m
```

## A burst

A million jobs due within one minute, a scheduled message of about 190 bytes
each, drained a transaction a cycle: the batch claimed two cycles before
acknowledged, the next claimed, so that one batch is in its workers' hands
while the next is. **arrival** keeps rows in the order jobs came with an index
by time; the **time** layouts keep the rows themselves in the order of their
time, and differ in where a lease lives: **time** moves the row to its lease's
end, **time-mark** marks it in place, **time-table** writes a row of a table of
its own. Jobs a second:

| layout | enqueued | batches of 1 | of 100 | of 1000 |
|---|---|---:|---:|---:|
| arrival | in time order | 372 | 26,661 | 93,952 |
| arrival | shuffled | 362 | 11,923 | 17,163 |
| time | shuffled | 352 | 17,557 | 54,703 |
| time-mark | shuffled | 372 | 24,410 | 78,236 |
| time-table | shuffled | 361 | 26,461 | 92,950 |
| arrival, keyed | shuffled | 359 | 9,469 | 13,918 |
| time, keyed | shuffled | 347 | 8,664 | 14,202 |

- **One job a transaction is an fsync**, 350 a second whatever the layout: a
  million jobs in 48 minutes. Batches are the first lever.
- **Where the rows lie is the second.** Shuffled into arrival order, the jobs
  of one minute sit on as many pages as there are jobs, and a batch of 1000 is
  slower than one of 100: every job rewrites a page of its own. Ordered by
  time, they sit together however they arrived, 5.4 times the throughput.
- **A lease that writes the job's row costs it.** Moving the row to its
  lease's end is a delete and an insert; marking it rewrites it; a lease row
  of its own leaves the job's row alone until it is acknowledged.
- **Keys undo the order.** A keyed job leaves its entry in the key index when
  it is acknowledged, and keys are random: 14,000 a second on either layout.

The next open after a crash, a thousand leases held: 110 to 154 ms for the
layouts that scan the file for them, 3.2 ms to empty a table of leases, and
nothing for moved rows, whose leases end by themselves within their 30 seconds.

## The file

A million scheduled messages with keys, spread over a week, enqueued
shuffled; `dbstat` by object, bytes a job.

| layout | jobs | key index | time index | total |
|---|---:|---:|---:|---:|
| arrival | 304.8 | 52.4 | 17.7 | 374.9 |
| time | 365.6 | 60.8 | — | 426.4 |

The table ordered by time leaves 19 % of its pages unused, where jobs arriving
at random times split them; the one in arrival order 4 %. The next due job is
one lookup at 5.0 µs on either; a cancel by key costs 42 to 48 µs of a
transaction of ten thousand.

Values of 64 to 4096 bytes, 100,000 jobs due within a minute, drained in
batches of 100:

| value | in the row | a second | spilled | a second |
|---:|---:|---:|---:|---:|
| 64 | 108 B | 24,996 | 110 B | 16,259 |
| 256 | 337 B | 21,807 | 309 B | 15,480 |
| 512 | 697 B | 19,718 | 622 B | 15,054 |
| 1024 | 4,690 B | 11,543 | 1,404 B | 13,144 |
| 4096 | 4,690 B | 10,699 | 4,645 B | 14,151 |

## Enqueue

Grouped writes into a queue of a million keyed jobs spread over a week, in the
same run; Enqueues a second at 1, 8, 64 and 512 goroutines.

| layout | kind | 1 | 8 | 64 | 512 |
|---|---|---:|---:|---:|---:|
| arrival | due now | 367 | 1,679 | 10,690 | 50,928 |
| arrival | at a random time | 361 | 1,533 | 8,467 | 22,721 |
| arrival | keyed | 355 | 1,504 | 6,419 | 11,134 |
| time | due now | 366 | 1,602 | 10,298 | 50,063 |
| time | at a random time | 374 | 1,535 | 7,895 | 16,735 |
| time | keyed | 359 | 1,434 | 5,445 | 8,271 |

A table ordered by time takes a job due at a random time into the middle of
its tree: the order the burst reads is paid for when the job arrives, spread
over the days before it.

## A Work loop

The loop `docs/jobs.md` describes, over two million jobs due at once in time
order with a handler that does nothing: as many claimed as it has free
workers, in one grouped write with the acknowledgements of the jobs finished
since; jobs a second, and jobs a write.

| layout | 1 | 8 | 64 | 512 |
|---|---:|---:|---:|---:|
| arrival | 362 | 1,386 | 10,222 | 49,403 (278 a write) |
| time | 375 | 1,521 | 10,467 | 40,845 (270) |
| time-mark | 378 | 1,481 | 10,242 | 46,929 (264) |
| time-table | 371 | 1,387 | 10,149 | 50,538 (285) |

Each write carries about half the workers: those finished while the last
write committed. A loop that claims ahead of its free workers would carry more;
that is measured next.

**The wake-up.** A job every 5 ms into an empty queue a loop of eight workers
waits on, an alarm in memory lowered by each Enqueue: the handler starts 7.8
ms after the Enqueue's call at the median, 9.6 at p99, the Enqueue itself 4.5
ms; a job due 100 ms after its call starts 7.2 ms after its time, 11.9 at p99.
It is a commit to lease the job, waiting behind the producer's own.

## What the handler writes

One message inserted into the application's database from 1 to 512 workers:
a transaction each, as sqldb's `Exec` commits today, and grouped.

| workers | a transaction each | grouped |
|---:|---:|---:|
| 1 | 291 | 301 |
| 8 | 297 | 1,517 |
| 64 | 301 | 10,540 |
| 512 | 317, p50 1.46 s | 63,790 |

However fast the queue, a handler that writes through sqldb waits for every
other handler's fsync.

## Raw calls

A Claim and its Ack, each a grouped write, from 1 to 512 goroutines over a
million jobs due:

| layout | 1 | 8 | 64 | 512 |
|---|---:|---:|---:|---:|
| time | 188 | 811 | 4,951 | 21,185 |
| time-mark | 190 | 831 | 4,991 | 14,745 |
| time-table | 172 | 812 | 4,894 | 8,926 |

With 512 jobs leased at once, a lease in place or in a table of its own leaves
them at the front of the queue, and every lone claim steps over them; a moved
row does not. Work claims many at once, and a remote worker should.

## What this settles and leaves

Settled, and built in `jobs/`: rows ordered by `(queue, next, id)`; a lease in
a table of its own, whose attempt is the token that settles it and whose rows a
next open counts and empties; values past 512 bytes spilled; claims and
settlements of a Work loop in one grouped write; an alarm in memory.

Left for the next round: a key index that a burst does not scatter; a Work
loop that claims ahead of its workers; sqldb's `Exec` grouped; the engine
measured against a table polled by hand, the way an application schedules
without it.
