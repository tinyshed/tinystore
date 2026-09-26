# What kv's mechanics cost — 2026-09-26

The round [docs/kv.md](../kv.md) asks for before the kv engine exists. Its API
and contracts are agreed; this measures the mechanics under them through
`internal/sqlite`, on the layout the design proposes: what a durable `Set`
costs, what a point `Get` costs, how fast a `LoseAtMost` counter adds and
flushes, how the file divides by object, how long `Clear` holds the writer,
and how far all of it is from the traffic production services see. Nothing
here is the engine: it is the prototype `spike/kv_*`.

| Question | Answer on this machine | Decision |
|---|---|---|
| durable `Set`s from many callers | a transaction each holds 340 a second at any concurrency in the container, 660 on Windows; grouped, 53,000 and 85,000 at 512 callers | group commit is in the first version |
| a point `Get` | a statement without `View`'s transaction serves 8 to 31 % more; eight readers serve 79,000 to 244,000 a second | a point read is one statement without a transaction |
| a `LoseAtMost` counter | Adds in memory: millions a second; a flush: 102,000 to 223,000 keys a second, and 100,000 keys held the writer 0.55 to 0.93 s | a flush writes at most 10,000 keys a transaction; 100,000 keys may wait |
| the file by object | at 1 KiB pages a 256-byte value overflows; at 4 KiB a 1024-byte value in the row costs 4,740 bytes a row, spilled 1,480 | 4 KiB pages; a value over 512 bytes spills |
| `Clear` | 115 to 364 keys a millisecond: 10,000 keys in 35 to 45 ms, a million in 5.3 to 8.7 s | up to 10,000 keys in its transaction; a larger branch gets a generation |
| production traffic | the services that log their requests peaked at 106 in a second, 4.9 a second over their busiest minute | the ceilings above are 500 to 2,300 times it |

## Environment and reproduction

- The prototype at `5957fdb` on `research/kv`, over the design at `edb1168`.
- AMD Ryzen 7 7700, 8 cores / 16 logical processors, 31.1 GiB, one Samsung
  SSD 990 PRO 1 TB (NVMe), Windows 11 Pro 10.0.26200.
- **The container**, whose figures come first: Docker Desktop 29.6.2 on WSL2,
  kernel 6.18.33.2-microsoft-standard-WSL2, 16 CPUs and 15.2 GiB visible;
  `golang:1.27`, go1.27.1 linux/amd64; the files on a named volume, ext4 in
  the WSL2 virtual disk on the same NVMe.
- **Windows**: go1.27.1 windows/amd64, the files on NTFS on the same NVMe. Go's
  clock on Windows advances in steps of about half a millisecond here, so
  latencies below a millisecond are not reported from Windows; throughput is,
  since it divides operations by a whole run.
- `modernc.org/sqlite` v1.59.0, SQLite 3.53.4; `synchronous=FULL`, WAL and
  the writer's and each reader's 1 MiB page cache, as `internal/sqlite` opens
  a file, with eight readers for the point reads.
- Each load runs three seconds; latency is one operation from its call to its
  return, waiting for a connection or a commit included.
- The production host's figures ran there at the owner's request; see
  [below](#on-a-production-host).
- The production figures are aggregates of the private container-log snapshot
  `6bf41f814539fdced24d5403e3adc27939446028717d006e63946601695d1ea0`, read
  locally; no host, service or line leaves it.

```sh
docker run --rm -v <repo>:/src -v <volume>:/perf -v <go cache>:/go \
  -e GOCACHE=/go/build-cache -e GOMODCACHE=/go/mod-cache -e GOWORK=off \
  -e TINYSTORE_SPIKE=1 -e TINYSTORE_KV_DIR=/perf -e TINYSTORE_KV_LARGE=1 \
  -w /src golang:1.27 go test ./spike -run TestKV -v -count=1 -timeout 150m

# on Windows, from <repo>
TINYSTORE_SPIKE=1 TINYSTORE_KV_DIR=<dir> go test ./spike -run TestKV -v -count=1 -timeout 60m
TINYSTORE_KV_DOCKER=<corpus> go test ./spike -run TestKVProductionRequestRates -v -count=1
```

## Durable Sets

Each goroutine writes new sessions, 64-byte values, into a file of 100,000.
**A transaction each** is `File.UpdatePrepared` a `Set`: the writer's slot, an
upsert, the revision's row, a commit and its fsync. **Grouped** is the
prototype `kvGroup`: a caller that finds no leader commits every `Set` queued
behind it, up to 1024, each in a savepoint, then hands the lead to the first
caller still waiting; no goroutine is started.

| goroutines | a transaction each | p50 | p99 | grouped | p50 | p99 | `Set`s a commit | Windows: each; grouped |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 347/s | 3.2 ms | 5.9 ms | 333/s | 3.3 ms | 6.6 ms | 1.0 | 671/s; 654/s |
| 8 | 354/s | 22 ms | 32 ms | 1,549/s | 5.1 ms | 12 ms | 4.5 | 653/s; 2,825/s |
| 64 | 327/s | 192 ms | 220 ms | 9,894/s | 6.6 ms | 17 ms | 32.6 | 660/s; 18,114/s |
| 512 | 343/s | 1,464 ms | 1,541 ms | 53,338/s | 9.2 ms | 25 ms | 299.9 | 654/s; 84,808/s |

A transaction each is an fsync each, and the writer is one: throughput does
not move with concurrency, and every caller waits for all the fsyncs ahead of
it. Grouped, callers that arrive during one commit share the next, so a
commit's cost is divided among them without a gathering delay: a caller alone
pays what it paid before. The prototype has no failure path; a refused `Set`,
cancellation and a commit whose outcome is unknown are
[the contract](../group-commit-contract.md)'s tests, not this round's.

## Point Gets

Sessions under their users, written in a scattered order, as users sign in:
one million, 358 MiB with the log, and ten million, 2,335 MiB. A hit reads a
stored session, a miss a key the file does not hold, as a revocation list is
mostly asked about. **View** is `File.ViewPrepared` with its one statement,
which begins and ends a transaction; **statement** is the same prepared
statement on a connection opened the same way, without a transaction of its
own; **statement, 64 MiB** gives each connection a 64 MiB page cache instead
of 1 MiB. Eight connections in each case.

| keys | lookup | way | 1 goroutine | p50 | 8 | p50 | 512 | p50 | Windows, 8 |
|---:|---|---|---:|---:|---:|---:|---:|---:|---:|
| 1 M | hit | View | 71,250/s | 11.1 µs | 187,403/s | 26 µs | 177,813/s | 2.8 ms | 108,160/s |
| 1 M | hit | statement | 96,825/s | 8.6 µs | 244,127/s | 21 µs | 224,988/s | 1.5 ms | 132,204/s |
| 1 M | hit | statement, 64 MiB | 71,472/s | 10.9 µs | 94,833/s | 33 µs | 147,338/s | 2.4 ms | 196,247/s |
| 1 M | miss | View | 64,544/s | 12.4 µs | 153,834/s | 31 µs | 153,700/s | 3.1 ms | 79,428/s |
| 1 M | miss | statement | 78,279/s | 10.3 µs | 201,756/s | 24 µs | 193,577/s | 1.8 ms | 85,724/s |
| 1 M | miss | statement, 64 MiB | 93,178/s | 8.4 µs | 131,270/s | 33 µs | 123,607/s | 2.8 ms | 193,724/s |
| 10 M | hit | View | 56,836/s | 13.4 µs | 96,696/s | 54 µs | 91,328/s | 5.5 ms | — |
| 10 M | hit | statement | 74,762/s | 10.7 µs | 111,084/s | 39 µs | 103,877/s | 3.4 ms | — |
| 10 M | hit | statement, 64 MiB | 75,263/s | 10.5 µs | 66,752/s | 47 µs | 100,634/s | 3.5 ms | — |
| 10 M | miss | View | 57,317/s | 13.6 µs | 140,849/s | 36 µs | 138,266/s | 3.5 ms | — |
| 10 M | miss | statement | 65,685/s | 12.1 µs | 177,476/s | 29 µs | 167,751/s | 2.1 ms | — |
| 10 M | miss | statement, 64 MiB | 85,345/s | 9.7 µs | 119,037/s | 36 µs | 101,465/s | 3.5 ms | — |

- **`View`'s transaction is paid for nothing.** With eight callers the bare
  statement serves 30 % more hits and 31 % more misses at one million keys in
  the container, 15 % and 26 % more at ten million, and 22 % and 8 % more on
  Windows; alone it answers 2.5 µs sooner. A single statement is its own
  snapshot, so the transaction buys no consistency.
- **Eight readers are the ceiling.** From 8 to 512 goroutines throughput holds
  and latency grows with the queue: 512 callers wait 1.5 to 5.5 ms at the
  median.
- **A larger page cache helped on Windows and not in the container.** With
  eight callers, 64 MiB a connection served 196,247 hits and 193,724 misses a
  second on Windows against the bare statement's 132,204 and 85,724, and
  fewer in the container. That reading a page from the system costs more on
  Windows is the likely reason and was not measured. Eight connections make it
  512 MiB either way, and one run of each is not enough to choose.

## A LoseAtMost counter

**Adds in memory** is the prototype `kvDeltas`: 64 shards of a lock and a map,
a delta a key, 100,000 distinct keys. The harness reads the clock twice around
every Add, which is most of what it measures.

| goroutines | container | p50 | p99 | Windows |
|---:|---:|---:|---:|---:|
| 1 | 4,124,697/s | 0.1 µs | 0.6 µs | 4,963,033/s |
| 8 | 14,365,554/s | 0.2 µs | 0.6 µs | 36,216,373/s |
| 512 | 23,124,663/s | 0.2 µs | 164 µs | 51,569,669/s |

**The flush** writes a delta of one to every key in one transaction with the
counter's upsert, the expired-is-absent and overflow guards included, first as
new rows and then onto them; the second flush checks that a delta was added.

| keys | new rows | onto them | Windows: new; onto |
|---:|---:|---:|---:|
| 1,000 | 7.4 ms | 8.5 ms | 7.8 ms; 9.8 ms |
| 10,000 | 44.8 ms | 51.6 ms | 71.0 ms; 92.0 ms |
| 100,000 | 547 ms | 853 ms | 730 ms; 926 ms |

A flush writes 102,000 to 223,000 keys a second, so a one-second interval
keeps up with about a hundred thousand distinct keys a second. It also holds
the writer: 100,000 keys in one transaction keep every durable `Set` waiting
half a second or more. Ten thousand a transaction hold it 45 to 92 ms.

## The file, by object

Twenty thousand sessions with an expiry, written in a scattered order, divided
by `dbstat`; bytes a row, and the median of a lookup that reads a row and not
its value, as `Has`, `SetIfAbsent`'s check and the expiry sweep do. The bytes
are the same on both systems; the lookups are the container's.

| page | value | kept | cells | overflow | unused | expiry index | spilled | file | lookup p50 |
|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 1 KiB | 64 | in the row | 149 | 0 % | 21 % | 54 | — | 203 | 5.8 µs |
| 1 KiB | 256 | in the row | 1,158 | 88 % | 73 % | 54 | — | 1,212 | 17.7 µs |
| 1 KiB | 1024 | spilled | 63 | 0 % | 15 % | 54 | 1,153 | 1,270 | 8.7 µs |
| 4 KiB | 16 | in the row | 76 | 0 % | 12 % | 51 | — | 128 | 7.5 µs |
| 4 KiB | 64 | in the row | 133 | 0 % | 13 % | 51 | — | 185 | 5.5 µs |
| 4 KiB | 256 | in the row | 368 | 0 % | 16 % | 51 | — | 420 | 5.7 µs |
| 4 KiB | 512 | in the row | 693 | 0 % | 18 % | 51 | — | 745 | 5.9 µs |
| 4 KiB | 512 | spilled | 60 | 0 % | 11 % | 51 | 587 | 698 | 5.4 µs |
| 4 KiB | 768 | in the row | 1,218 | 0 % | 32 % | 51 | — | 1,270 | 6.7 µs |
| 4 KiB | 768 | spilled | 60 | 0 % | 11 % | 51 | 821 | 933 | 5.5 µs |
| 4 KiB | 1024 | in the row | 4,688 | 87 % | 77 % | 51 | — | 4,740 | 19.5 µs |
| 4 KiB | 1024 | spilled | 60 | 0 % | 11 % | 51 | 1,369 | 1,480 | 5.4 µs |
| 4 KiB | 4096 | in the row | 4,688 | 87 % | 11 % | 51 | — | 4,740 | 21.7 µs |
| 4 KiB | 4096 | spilled | 60 | 0 % | 11 % | 51 | 4,609 | 4,721 | 5.2 µs |

- **1 KiB pages are wrong for kv.** A row with a 256-byte value passes what a
  1 KiB page keeps of a row, so SQLite writes most of it to an overflow page
  it leaves three quarters empty: 1,158 bytes a row, and a lookup three times
  slower. Records chose 1 KiB for rows of about 7 KB; kv's rows are small.
- **At 4 KiB a value up to 512 bytes belongs in the row.** It costs what
  spilling costs, 745 against 698 bytes a row, and a `Get` reads one row
  instead of two. At 768 bytes four rows fit a page and a third of it is
  empty, 1,270 against 933; at 1024 the row passes what a page keeps and costs
  4,740 bytes against 1,480, with lookups four times slower.
- **The expiry index is 51 bytes a row**, the path repeated after the expiry:
  28 % of a 64-byte session's 185.

## Clear

A branch of one owner among 200,000 sessions, deleted by its range in one
transaction, the expiry index included.

| keys | container | Windows |
|---:|---:|---:|
| 1,000 | 4.1 ms | 2.7 ms |
| 10,000 | 35 ms | 45 ms |
| 100,000 | 494 ms | 868 ms |
| 1,000,000 | 5,289 ms | 8,719 ms |

A `Clear` holds the writer for as long as it deletes, 115 to 364 keys a
millisecond. Signing a user out everywhere, tens of keys, costs a fraction of
a millisecond; the same `Clear` on a bucket of a million sessions, signing everybody out after a
leaked secret, would stop every other write for five seconds or more.

## Production traffic

Container logs from two production hosts over 14 days. A request is a line
that names an HTTP method with a path or a URL and then an HTTP version or a
status, as text or as JSON fields; four services log their requests that way,
and the proxy in front of the busier host's services logs none of them, so
these are the requests that reached services that say so, not everything that
reached the hosts.

| | requests | mean a second | p99 | p99.9 | busiest minute | peak second |
|---|---:|---:|---:|---:|---:|---:|
| the busier host, three services together | 246,442 | 0.20 | 2 | 3 | 4.9/s | 106 |
| its service with the most requests | 124,560 | 0.10 | 1 | 1 | 4.8/s | 59 |
| the other host, one service | 1,068 | 0.00 | 0 | 0 | 0.2/s | 9 |

The peak second, 106, is one burst of a service that logged 1,218 requests in
nine days. The proxy on one host reports two CPUs to its runtime; this machine
has sixteen logical processors.

## Under a flood

A flood is a session read and a counter's `Add` a request, and nothing
durable until a limit lets a request in. On this machine:

- a flood a thousand times the peak second, 106,000 requests a second, is about
  what eight readers answer for ten million sessions, 104,000 to 111,000 hits a
  second, and half of what they answer for one million;
- the counters add in memory, and a flush keeps up with about a hundred
  thousand distinct keys a second;
- behind the limit, grouped durable writes hold 53,000 a second, 500 times the
  peak second; a transaction each would hold three times it.

A production host has a fraction of this machine; [below](#on-a-production-host)
is one.

## On a production host

The same prototype, built for linux/amd64 at `5957fdb`, ran on the busier of
the two production hosts while its services kept running: a KVM guest with two
vCPUs of an AMD EPYC 7763, 3.9 GB of memory with swap in use, a virtual disk,
Linux 6.8. It ran at `nice -n 19` with `GOMEMLIMIT=512MiB`, without the
ten-million set, whose file is larger than the memory the host had free, and
without the layout, whose bytes do not depend on the machine; its files and the
binary were deleted afterwards.

| | production host | the container here |
|---|---:|---:|
| durable `Set`s, a transaction each, 1 and 512 goroutines | 1,585/s; 1,506/s | 347/s; 343/s |
| grouped, 8, 64 and 512 goroutines | 5,760/s; 21,376/s; 54,971/s | 1,549/s; 9,894/s; 53,338/s |
| grouped at 512, p50 and p99 | 7.3 ms; 32 ms | 9.2 ms; 25 ms |
| point Gets, one million sessions, eight callers, hits: `View`, statement | 58,529/s; 100,292/s | 187,403/s; 244,127/s |
| the same, misses | 49,243/s; 76,594/s | 153,834/s; 201,756/s |
| Adds in memory, 512 goroutines | 3,949,893/s | 23,124,663/s |
| a flush of 100,000 keys, new rows and onto them | 744 ms; 1,014 ms | 547 ms; 853 ms |
| `Clear` of 10,000 and 1,000,000 keys | 52 ms; 11.1 s | 35 ms; 5.3 s |

- **Its disk acknowledged an fsync faster than this machine's NVMe**: a
  transaction each held 1,500 `Set`s a second there against 340 here. The
  hypervisor answers the flush; whether what it holds survives the host losing
  power is the provider's to say, and this round cannot.
- **Its reads were two fifths of the container's here**, 100,292 hits a second
  through eight readers on two vCPUs; a 64 MiB page cache was slower there too.
- **Against its own traffic**: the host's busiest second held 106 requests. Its
  reads hold about 900 times that, grouped writes 520 times, and a
  transaction each 14 times.

## What this round does not settle

- **Whether an fsync the hypervisor acknowledged is durable** on the
  production host, and the ten-million set there.
- **The read path past eight readers**, `mmap_size`, and one page cache for the
  whole file rather than one a connection.
- **A spilled value's `Get`**, which reads two rows; only the lookup without a
  value was timed.
- **The writer's cache under random upserts into a large file.** Ten million
  sessions took 6 min 44 s to write in fifty-thousand-row transactions, 25,000
  a second, with the writer's 1 MiB cache.
- **Services that do not log their requests.** The production rates are a
  floor.
