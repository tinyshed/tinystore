# To Prometheus

The Metrics engine can write the newest sample of every series it stores in
Prometheus' text format. This is the page that Prometheus scrapes, so an
existing Prometheus or Grafana can see what the store keeps. TinyStore
doesn't serve HTTP. Your program's own server answers the scrape, and one
call gives it the bytes.

```rust
let stats = store.metrics()?;
let mut body = Vec::new();
let written = stats.exposition().only_prefix("shop_").write(&mut body)?; // Written { names, series, skipped }
```

```text
# HELP shop_orders_total Orders placed.
# TYPE shop_orders_total counter
shop_orders_total{shop="msk"} 1027 1732622400000
```

The call exists only in Rust. It isn't in the wire protocol yet, so Bun,
Node, Python and Go have no call for it. From any of them, and from a shell,
the command line writes the same text. It can run beside the program that
has the store open:

```sh
tinystore metrics ./data export --only-prefix shop_
```

[metrics export](../cli/metrics.md#export-for-prometheus) lists its flags.
It also has a cron line for node_exporter's textfile collector, which
accepts samples only without their times. Use `--no-timestamps` for it, or
`timestamps(false)` in Rust.

## Give the metrics to a Prometheus

If your program already runs an HTTP server, add one handler. This example
uses axum and the async API:

```rust
async fn metrics(State(stats): State<tinystore::aio::metrics::Metrics>) -> Result<Vec<u8>, StatusCode> {
    match stats.exposition().write(Vec::new()).await {
        Ok((body, _)) => Ok(body),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
```

Prometheus then scrapes it like any other exporter:

```yaml
scrape_configs:
  - job_name: shop
    static_configs:
      - targets: ['shop:8080']
```

The awaited `write` runs on the store's thread pool, as every read of the
[async API](README.md#async-rust) does. It takes the output by value and
gives it back with the text written into it.

## What the text holds

- The text has one family per name in the store, sorted by the names' bytes.
  Each family starts with its `# HELP` and `# TYPE`, taken from what the
  store keeps about the name. Then it has one line per series: its labels,
  its newest value and that sample's time in milliseconds.
  `timestamps(false)` leaves the times out. Use it for node_exporter's
  textfile collector and the Pushgateway, because they refuse a sample that
  has its own time. Use it also for a Prometheus that would drop the old
  time of a silent series as out of bounds.
- A counter is written as a counter, and a gauge as a gauge. A histogram is
  written as its `_bucket` series, with its `_sum` and `_count` in the same
  family. A timer of your program's own is written the same way, plus
  `_max`, which is a separate gauge. The store doesn't keep the quantiles of
  a summary that an exporter sent. It stored that summary as gauges and
  counters, so it writes them as gauges and counters.
- Sometimes the store also has a name that equals a `_bucket`'s family
  name, for example a gauge `rpc` beside `rpc_bucket`. One family can't have
  two kinds, so the `_bucket` is written as `untyped`, and its `_sum` and
  `_count` as counters.
- A unit goes on a `# UNIT` line. Prometheus skips that line as a comment,
  and the store's own ingest reads it back. A histogram's family gets the
  unit of its `_sum`, because that is the only part that ingest puts a
  family's unit back onto. A unit of more than one word is left out,
  because the line has no escapes. `describe` and an instrument refuse a
  unit that contains a line break or a tab.
- Values are written so that a reader parses back the same float: `-0`,
  `+Inf`, `-Inf` and `NaN`. The only thing the text can't carry is a NaN's
  payload, so it reads back as a plain NaN.
- The text uses format 0.0.4, which Prometheus 2, VictoriaMetrics and every
  scraper read. In this format a name contains only letters, digits, `_` and
  `:`, and a label the same without `:`. If any other character appears, a
  reader refuses the whole scrape. So the store writes any other character
  as `_`, and puts a `_` before a leading digit, as OpenTelemetry's exporter
  does. For example, `app.load{url.scheme="https"}` is written
  `app_load{url_scheme="https"}`.
- After this renaming, two names can collide. For example, `cpu.user`
  becomes `cpu_user`, which the store may also have, or which a name earlier
  in byte order already took. That would write one family twice. Two labels
  of one series can collide in the same way, and a reader would refuse that
  series. The store writes neither, and `skipped` counts their series, so
  you know what the text left out.

The store's own [ingest](ingesting.md#from-prometheus) reads this text back
to the same names, labels, kinds, help and newest samples. A name or a label
that was written with `_` reads back with `_` (MET-31).

## Which series

`only_prefix` writes only the names that start with it. Without `since`,
the call writes every series that the store still keeps, however long it
has been silent, with the time of its last sample. The store doesn't hide
anything, and Prometheus decides what is stale. `since` leaves out a series
whose newest sample is older than that:

```rust
let live = stats.exposition().since(Duration::from_mins(5)).write(&mut body)?;
```

The call writes every series that matches, however many series a name has,
or it fails. If it fails, the output contains the lines written before the
failure (MET-32). Your program's instruments write their totals before the
exposition reads.

## What it costs

The exposition reads one small piece of each series, as
[`latest`](reading.md#the-newest-value) does: a chunk of its head, or the
directory of its newest block. It doesn't read the file at one moment. It
reads the names a thousand at a time, and each name's series a thousand at
a time, each part from its own snapshot. It writes each part's lines before
it reads the next part, so it holds only one part in memory, however much
the store keeps. It reads the store's clock once, when it starts, and reads
samples up to that time.

To time it on your machine, run this test. It writes 100,000 series of 100
names and times an exposition of them:

```sh
cargo test --release -p tinystore --lib --features testing exposition_of_a_hundred_thousand -- --ignored --nocapture
```

## See also

- [Ingesting](ingesting.md#from-prometheus): the text an exporter serves,
  read into the store.
- [Reading](reading.md): `latest`, names and labels.
- [Instruments](instruments.md): timers and histograms, and what they write.
