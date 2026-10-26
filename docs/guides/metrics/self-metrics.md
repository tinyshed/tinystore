# The store's own metrics

A store can measure itself into its own `metrics.db`: its commits and how long
writes wait for them, its files' sizes, its memory, the machine it runs on,
and each engine's own counts. A program reads them as it reads its own series,
for a dashboard of the store with no Prometheus beside it. It is off unless
asked for.

```ts
const store = await open('./data', { selfMetrics: true })
const alone = await open('./data', { selfMetrics: { system: false } }) // the machine left out
```

```python
store = await tinystore.open("./data", self_metrics=True)
alone = await tinystore.open("./data", self_metrics={"system": False})
```

```go
store, err := tinystore.Open(ctx, "./data", tinystore.SelfMetrics())
alone, err := tinystore.Open(ctx, "./data", tinystore.SelfMetrics(tinystore.WithoutSystem()))
```

```rust
let store = Store::open("./data", Options { self_metrics: Some(SelfMetrics::default()), ..Options::default() })?;
let alone = SelfMetrics { system: false };
```

```sh
tinystore serve ./data --self-metrics
```

Then read them as any series:

```ts
const { groups: slow } = await store.metrics.aggregate({ name: 'tinystore_commit_ms', since: '1h', every: '1m', op: 'max', by: ['file'] })
```

```python
slow = (await store.metrics.aggregate(name="tinystore_commit_ms", since="1h", every="1m", op="max", by=["file"])).groups
```

```go
slow, err := metrics.Aggregate(ctx, store, metrics.Query{Name: "tinystore_commit_ms", Since: time.Hour, Every: time.Minute,
	Op: metrics.Max, By: []string{"file"}})
```

```rust
let slow = store
    .metrics()?
    .query()
    .name("tinystore_commit_ms")
    .since(Duration::from_hours(1))
    .every(Duration::from_mins(1))
    .by(&["file"])
    .aggregate(Op::Max)?.groups;
```

## The store, SQLite and the writer

| Name                                  | Kind    | Labels | What it is                                            |
|---------------------------------------|---------|--------|-------------------------------------------------------|
| `tinystore_commits_total`             | counter | `file` | commits of the file's writer                          |
| `tinystore_commit_writes_total`       | counter | `file` | the writes they carried; over commits, a group's size |
| `tinystore_commit_ms`                 | timer   | `file` | a commit, its sync included                           |
| `tinystore_writer_wait_ms`            | timer   | `file` | a write from its queueing to its commit's begin       |
| `tinystore_file_bytes`                | gauge   | `file` | the file's size                                       |
| `tinystore_wal_bytes`                 | gauge   | `file` | its write-ahead log's size                            |
| `tinystore_readers_open`              | gauge   | `file` | its read connections open                             |
| `tinystore_memory_used_bytes`         | gauge   |        | what the store's work holds of its memory budget      |
| `tinystore_memory_limit_bytes`        | gauge   |        | the budget                                            |
| `tinystore_disk_free_bytes`           | gauge   |        | the disk's free space under the directory             |
| `tinystore_background_failures_total` | counter | `task` | failures of the store's periodic work                 |

`file` is `kv`, `jobs`, `records`, `metrics`, `search`, and `sql/<name>` for a
database: the first 32 databases a store opens are named, and every one after
them counts into `sql/__overflow`.

## The machine

Unless `system: false`:

| Name                                      | What it is                                                   |
|-------------------------------------------|--------------------------------------------------------------|
| `tinystore_process_memory_bytes`          | the process's resident memory                                |
| `tinystore_process_cpu_percent`           | its processor use, 100 a core                                |
| `tinystore_system_memory_bytes`           | the system's memory                                          |
| `tinystore_system_memory_available_bytes` | what of it is available                                      |
| `tinystore_system_cpu_percent`            | every core's use together, 100 all busy                      |
| `tinystore_system_core_cpu_percent`       | each core's, labelled `core`                                 |
| `tinystore_system_load`                   | load over `1m`, `5m`, `15m`, labelled `span`; not on Windows |

A processor's use is counted between two reads, so its gauges begin 15
seconds after the store opens. In a container they read the container's.

## Each engine's

| Name                                                                                  | What it is                                |
|---------------------------------------------------------------------------------------|-------------------------------------------|
| `tinystore_jobs_completed_total`, `_failed_total`, `_retried_total`                   | jobs by `queue`                           |
| `tinystore_jobs_run_ms`                                                               | a run, from its claim to its end          |
| `tinystore_jobs_claim_delay_ms`                                                       | how late a claim took a job past its time |
| `tinystore_jobs_waiting`, `_running`                                                  | jobs waiting, and held                    |
| `tinystore_kv_expired_total`                                                          | keys deleted once they expired            |
| `tinystore_records_events_total`                                                      | events appended, by `stream`              |
| `tinystore_records_lines_dropped_total`                                               | a logger's lines dropped                  |
| `tinystore_channels_published_total`, `_delivered_total`, `_missed_total`             | by `channel`                              |
| `tinystore_locks_wait_ms`, `tinystore_locks_busy_total`, `tinystore_locks_lost_total` | by `lock`                                 |
| `tinystore_search_ms`                                                                 | a search, by `index`                      |
| `tinystore_blobs_written_total`, `_read_total`, `_damaged_total`                      | files, and their bytes                    |
| `tinystore_wire_requests_total`, `tinystore_wire_call_ms`                             | requests by `method`, `kv.get`            |
| `tinystore_replication_ack_wait_ms`                                                   | a synchronous home's wait for a copy      |

An engine's labels name its first 32 values, and every value after them counts
into `__overflow`. A label is never what a program gives at a call: no key, no
SQL, no job's id, no path.

## How it is measured

- Through the instruments: counters, gauges and timers kept in memory and
  written with the program's own every 15 seconds. No measurement opens a
  transaction, and a measurement never fails a write.
- The metrics engine's own writes of its totals are not measured, so the
  store does not measure itself measuring.
- Names that start with `tinystore_` are the store's: an instrument or an
  `ingest` of the program's that writes one is `invalid`.

## Cost

Off, a write pays one check of an empty cell. On, in a write-heavy run of
kv's `set` of 20,000 keys from 1, 8 and 64 threads, the median of five passes,
`durability: os` (Windows 11 Pro, AMD Ryzen 7 7700, 31 GiB, release build,
19 October 2026), one thread ran within 1 % of off, and with more threads the
passes moved 6 % either way, on not the slower of the two. Linux and macOS
were not measured.

```sh
cargo test --release -p tinystore --lib probe::probe_tests::sets_measured_and_not -- --ignored --nocapture
```

## See also

- [Metrics](README.md): the engine's overview.
- [Aggregates](aggregates.md): reading these series.
- [Running it](../../handbook/production/running-it.md) in the Handbook.
