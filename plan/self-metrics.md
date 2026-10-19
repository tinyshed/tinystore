# The store's own metrics

A store can measure itself into its own `metrics.db`: its commits and their
waits, its files, its memory, its background work, then each engine's own.
A program reads them as it reads its own series, from a chart in Dashbin
with no Prometheus beside it. Dashbin's A10; asked for on 19 October.

## The call site

```rust
let store = Store::open("./data", Options { self_metrics: Some(SelfMetrics::default()), ..Options::default() })?;
let alone = SelfMetrics { system: false };   // the store's own, the machine left out
let slow = store.metrics()?.query().name("tinystore_commit_ms").since(HOUR).every(MINUTE).aggregate(Op::Max)?;
```

```ts
const store = await open('./data', { selfMetrics: true })
const alone = await open('./data', { selfMetrics: { system: false } })
```

```python
store = await tinystore.open("./data", self_metrics=True)
alone = await tinystore.open("./data", self_metrics={"system": False})
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.SelfMetrics())
alone, err := tinystore.Open(ctx, "./data", tinystore.SelfMetrics(tinystore.WithoutSystem()))
```

The option goes in the protocol's `store.Options` to a core in the SDK's own
process, and as `tinystore serve --self-metrics` to a private child or a
sidecar the SDK starts, `--no-system-metrics` with `{ system: false }`; a sidecar found running keeps its own, and a core
this process holds already, opened without them, refuses to be asked.

- One option of the store, off unless asked for, and the machine it runs on
  measured with it unless `system: false` leaves it out. No engine has a
  switch of its own: an embedded library is not a control panel.
- Nothing else is new: the series are read, aggregated, listed and kept as
  any other, through the calls of [api/metrics.md](api/metrics.md).
- Not on a node's copy: a copy's `metrics.db` is its home's, which it cannot
  write.

## How it is measured

- **Through the instruments.** What the store measures goes into counters,
  gauges and timers of the metrics engine, kept in memory and written with
  the program's own every 15 seconds, at `flush` and when the store closes,
  as their book says. No measurement opens a transaction, and a crash loses
  at most the last 15 seconds.
- **Engines never import each other**, so the SQLite under every engine does
  not call the metrics engine. The root holds a `Probe`: each file tells it
  of each commit, and the metrics engine, when the store asks for its own
  metrics, is what listens. A store without them has no listener, and a
  commit pays one check of an empty cell.
- **No loop.** The metrics engine's own writes of its instruments' totals,
  and every read the probe makes of a file's size, are not measured: a
  commit of `metrics.db` that carried only those is no commit of
  `tinystore_commits_total`. What a program writes into `metrics.db`, an
  `ingest` or a `delete`, is measured as any other write.
- **A measurement never fails a write.** A total the store cannot write is
  said once in the store's log and written at the next write, as an
  instrument's is; a commit is answered whatever its measurement does.
- **Opened with the store.** The store opens the metrics engine when it
  opens, so that the files opened after it are measured from their first
  commit. With `background: false` nothing writes every 15 seconds; a test
  calls `flush` and closes the store, as for any instrument.

## Names

Names that start with `tinystore_` are the store's: an instrument of the
program's, or an `ingest`, that writes one is `Invalid`. They follow the
book's spelling: a counter ends in `_total`, a timer is in milliseconds and
ends in `_ms`, as `http_request_ms`, a size ends in `_bytes`.

### First: the store, SQLite and the writer

| Name                                  | Kind    | Labels | What it is                                                                               |
|---------------------------------------|---------|--------|------------------------------------------------------------------------------------------|
| `tinystore_commits_total`             | counter | `file` | commits of the file's writer, grouped or a transaction's                                 |
| `tinystore_commit_writes_total`       | counter | `file` | the writes those commits carried: over `tinystore_commits_total`, a group's average size |
| `tinystore_commit_ms`                 | timer   | `file` | a commit's `commit` statement, its sync included: the local file alone, no other node    |
| `tinystore_writer_wait_ms`            | timer   | `file` | a write from its queueing to the begin of the commit that carries it                     |
| `tinystore_file_bytes`                | gauge   | `file` | the file's size                                                                          |
| `tinystore_wal_bytes`                 | gauge   | `file` | its write-ahead log's size                                                               |
| `tinystore_readers_open`              | gauge   | `file` | its read connections open                                                                |
| `tinystore_memory_used_bytes`         | gauge   |        | what the store's work holds of its memory budget                                         |
| `tinystore_memory_limit_bytes`        | gauge   |        | the budget, absent when the store has none                                               |
| `tinystore_disk_free_bytes`           | gauge   |        | the disk's free space under the store's directory                                        |
| `tinystore_background_failures_total` | counter | `task` | failures of the store's periodic work, by its task's name                                |

A gauge is read when the instruments write, every 15 seconds.

### The machine

With `system`, unless it says `false`, and the library's feature `system`,
on by default, which reads the machine through sysinfo 0.39:

| Name                                      | Kind  | Labels | What it is                                              |
|-------------------------------------------|-------|--------|---------------------------------------------------------|
| `tinystore_process_memory_bytes`          | gauge |        | the process's resident memory                           |
| `tinystore_process_virtual_memory_bytes`  | gauge |        | its virtual memory                                      |
| `tinystore_process_cpu_percent`           | gauge |        | its processor use since the read before, 100 a core     |
| `tinystore_system_memory_bytes`           | gauge |        | the system's memory                                     |
| `tinystore_system_memory_available_bytes` | gauge |        | what of it is available                                 |
| `tinystore_system_swap_bytes`             | gauge |        | the system's swap                                       |
| `tinystore_system_swap_used_bytes`        | gauge |        | what of it is in use                                    |
| `tinystore_system_cpus`                   | gauge |        | its logical processors                                  |
| `tinystore_system_cpu_percent`            | gauge |        | their use since the read before, 100 every one busy     |
| `tinystore_system_core_cpu_percent`       | gauge | `core` | each one's use, `core` its number from 0                |
| `tinystore_system_load`                   | gauge | `span` | the load average over `1m`, `5m` and `15m`; not Windows |

A processor's use is counted between two reads, so its gauges begin at the
second write, 15 seconds after the store opens. The machine is the one the
process runs on: a store beside another process's in a container reads that
container's.

### Then, engine by engine

Each is a slice of its own, in this order, its names written here when it is
built:

- **replication**: `tinystore_replication_ack_wait_ms`, a synchronous home's
  wait for a copy to hold its commit, apart from `tinystore_commit_ms`, so
  that a slow disk and a slow link far away read apart; a copy's lag in
  entries and in milliseconds; `CommittedLocally`; links cut.
- **jobs**: jobs waiting and running by queue, completed, failed and retried,
  the wait from a job's due time to its claim, its run's time.
- **kv**: calls by kind, keys expired, transactions that ran again.
- **records**, **metrics**: events and samples taken, a logger's lines
  dropped, a sample refused by why, sets of labels past `__overflow`.
- **channels**, **locks**, **search**, **blobs**, **wire**: messages missed,
  waits for a lock and leases lost, the head waiting to be packed, bytes
  written and read and what the scrub found, connections and calls by method.

## Labels

A label's values are a few the store knows in advance, never what a program
gives at a call: no key, no SQL text, no job's id, no path of a file.

- `file` is `kv`, `jobs`, `records`, `metrics`, `search`, `catalog`, and
  `sql/<name>` for an application's database. A store of ten thousand
  databases would make a hundred thousand series, so the first 32 databases
  a store opens are named, and every one after them counts into
  `sql/__overflow`, which a program that holds more names as it sees fit.
- `task` is the name an engine gives its periodic work, a fixed set.
- Later slices bound `queue`, `index` and `node` the same way: 32 named, then
  `__overflow`.

## Tests

- a store opened with its own metrics holds `tinystore_commits_total` and the
  rest after a write and a `flush`, with the values those writes made; one
  opened without holds none of them.
- the metrics engine's writes of its totals add nothing to
  `tinystore_commits_total{file="metrics"}`, an `ingest` adds one.
- a total survives the store's close and its opening again, its run beginning
  with a zero as an instrument's does.
- the 33rd database counts into `sql/__overflow`.
- a name of `tinystore_` from the program is `Invalid`.
- a failure of periodic work counts by its task.

## Cost

A write-heavy run, kv's `set` of 20,000 keys from 1, 8 and 64 threads, the
store measured and not in turns, each the median of five passes, with
`durability: Os` so that the sync hides nothing. Off costs one load of an
empty cell a write; on is to cost under 3 % of the writes a second, or the
slice changes what it measures.

19 October, Windows 11 Pro, AMD Ryzen 7 7700, 31 GiB, rustc 1.99.0, release,
three runs:

| Threads | Off, sets a second       | On, sets a second        | On against off           |
|---------|--------------------------|--------------------------|--------------------------|
| 1       | 34,656 · 34,353 · 34,249 | 34,401 · 34,198 · 34,269 | −0.7 % · −0.5 % · +0.1 % |
| 8       | 49,300 · 47,203 · 47,462 | 48,796 · 48,769 · 48,615 | −1.0 % · +3.3 % · +2.4 % |
| 64      | 53,917 · 49,177 · 52,157 | 57,370 · 50,749 · 50,636 | +6.4 % · +3.2 % · −2.9 % |

One thread, where the run is steadiest, puts on within 1 % of off; with
more, the runs move by 6 % either way between passes, and on is not the
slower of the two. Linux and macOS are not measured.

```sh
cargo test --release -p tinystore --lib metrics::own::own_tests::sets_measured_and_not -- --ignored --nocapture
```
