# The metrics crate

Rust code measures itself through the `metrics` crate's macros:
`counter!`, `gauge!`, `histogram!`. The store's recorder makes those macros
write into `metrics.db`, as the logger's layer makes `tracing`'s macros write
into `records.db`. Libraries that already use the crate, an HTTP framework, a
connection pool, are recorded with no change.

```rust
let stats = store.metrics()?;
metrics::set_global_recorder(stats.recorder().buckets("http_request_ms", &[5.0, 25.0, 100.0, 500.0]))?;

metrics::counter!("http_requests_total", "route" => "/notes").increment(1);
metrics::gauge!("queue_depth").set(12.0);
metrics::histogram!("http_request_ms").record(started.elapsed().as_secs_f64() * 1000.0);
```

| Macro         | Becomes                                                          |
|---------------|------------------------------------------------------------------|
| `counter!`    | a counter, its total the store's                                 |
| `gauge!`      | a gauge                                                          |
| `histogram!`  | a histogram of its own unit, with the buckets given for its name |
| `describe_*!` | the name's unit and help, as `describe` sets them                |

- The recorder's buckets are given by name, as a timer's are, or
  `Buckets::Exponential`. A histogram without buckets keeps its count, sum and
  longest, and has no percentile.
- A process has one global recorder, which the crate lets be set once: a
  program that opens two stores, as a test may, records into the first.
- A call never waits and never writes: the recorder's series are written with
  the instruments every 15 seconds, at `stats.flush()` and when the store
  closes.

Name the store's handle something other than `metrics`, `stats` here, so the
crate's macros and the store's calls read apart.

## Or the store's own instruments

The store's instruments are the same series with a typed handle:

```rust
let requests = stats.counter("http_requests_total").open()?;
requests.with(&[("route", "/notes")]).inc();

let latency = stats.timer("http_request_ms").buckets(&[5.0, 25.0, 100.0, 500.0]).open()?;
let _timing = latency.start(); // recorded when it drops
```

| Use                 | When                                                          |
|---------------------|---------------------------------------------------------------|
| the `metrics` crate | the code, or a library it uses, already measures through it   |
| the instruments     | a timer's `measure` and `start`, a gauge read from a function |

Both write the same series, and every read finds both.

A timing from `start()` records when it drops, so it is kept in a name:
`let _timing = latency.start()`. `let _ = latency.start()` records at once,
since Rust drops a value thrown away on purpose; clippy's
`let_underscore_must_use` catches it.

## See also

- [Metrics](README.md): the engine's overview.
- [Instruments](instruments.md): the same in every language.
- [Logging](../records/logging.md): the `tracing` layer beside it.
