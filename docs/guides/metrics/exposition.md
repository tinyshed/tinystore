# To Prometheus

The Metrics engine writes the newest sample of every series it holds as
Prometheus' text format, the page a Prometheus scrapes, so that an existing
Prometheus or Grafana sees what the store keeps. TinyStore serves no HTTP:
your program's own server answers the scrape, and the bytes come from one
call.

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

The call is in Rust alone: it is not over the wire yet, so Bun, Node, Python
and Go have no call for it. From any of them, and from a shell, the command
line writes the same text beside the program that holds the store:

```sh
tinystore metrics ./data export --prefix shop_
```

[metrics export](../cli/metrics.md#export-for-prometheus) has its flags, and
a cron line for node_exporter's textfile collector, which takes samples
without their times.

## Give the metrics to a Prometheus

A program that already runs an HTTP server adds one handler, here with axum
and the async API:

```rust
async fn metrics(State(stats): State<tinystore::aio::metrics::Metrics>) -> Result<Vec<u8>, StatusCode> {
    match stats.exposition().write(Vec::new()).await {
        Ok((body, _)) => Ok(body),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
```

and Prometheus scrapes it as it scrapes any exporter:

```yaml
scrape_configs:
  - job_name: shop
    static_configs:
      - targets: ['shop:8080']
```

The awaited `write` runs on the store's pool, takes the writer and gives it
back with what it wrote, as the [async API](README.md#async-rust) does with
every read.

## What the text holds

- One family a name the store holds, in the byte order of the names, each
  with its `# HELP` and `# TYPE` from what the store keeps of the name, then
  one line a series: its labels, its newest value and that sample's time in
  milliseconds. `timestamps(false)` leaves the times out, for
  node_exporter's textfile collector and the Pushgateway, which refuse a
  sample with a time of its own, and for a Prometheus that would drop a
  silent series' old time as out of bounds.
- A counter and a gauge are what they are. A histogram is its `_bucket`
  series, with the `_sum` and `_count` beside it in its family; a timer of
  the program's own is that, and `_max`, a gauge of its own. The store keeps
  no quantile of a summary an exporter sent: what it made of one, gauges and
  counters, is written as those.
- A `_bucket` whose family name the store also holds as a name, such as a
  gauge `rpc` beside `rpc_bucket`, is written `untyped`, since one family
  cannot be two kinds, and its `_sum` and `_count` as counters.
- A unit goes on a `# UNIT` line, which Prometheus passes over as a comment
  and the store's own ingest reads back. A histogram's family carries its
  `_sum`'s unit, the one part a family's unit goes back onto. A unit of more
  than one word is left out, since the line has no escapes; `describe` and
  an instrument refuse a unit with a line break or a tab.
- Values are spelt so that a reader parses the same float back: `-0`,
  `+Inf`, `-Inf` and `NaN`. A NaN's payload is the one thing the text cannot
  carry: it reads back as the plain NaN.
- The text is format 0.0.4, which Prometheus 2, VictoriaMetrics and every
  scraper read, and which spells a name with letters, digits, `_` and `:`,
  and a label without the `:`; a reader refuses the whole scrape past them.
  Any other character is written `_`, and a leading digit gets a `_` before
  it, as OpenTelemetry's exporter does: `app.load{url.scheme="https"}` is
  written `app_load{url_scheme="https"}`.
- A name so spelt that the store also holds, as `cpu.user` beside
  `cpu_user`, or that a name before it in byte order took, would be one
  family twice; a series whose labels so spelt are two of one name would be
  refused. Neither is written, and `skipped` counts their series, so the
  caller knows what the text left out.

The text the store writes is read back by its own
[ingest](ingesting.md#from-prometheus) to the same names, labels, kinds, help
and newest samples, a name or a label spelt with `_` read back as spelt
(MET-31).

## Which series

`prefix` writes only the names that start with it. Without `since`, every
series the store still keeps is written, however long it has been silent,
with the time of its last sample: the caller is told what it was given, and
Prometheus decides what is stale. `since` leaves out a series whose newest
sample is older:

```rust
let live = stats.exposition().since(Duration::from_mins(5)).write(&mut body)?;
```

Every series within them is written, however many a name has, or the call
fails, the writer then holding the lines before the failure (MET-32). The
program's instruments write their totals before the exposition reads.

## What it costs

The exposition reads one small piece of each series, as
[`latest`](reading.md#the-newest-value) does: a chunk of its head, or its
newest block's directory. It is not one moment of the file: it reads the
names a thousand at a time, and each name's series a thousand at a time,
each part one snapshot, and writes each part's lines before it reads the
next, so that it holds one part whatever the store keeps. The newest time it
reads up to is the store's clock once, when it starts.

To time it on your machine, this test writes 100,000 series of 100 names and
times an exposition of them:

```sh
cargo test --release -p tinystore --lib --features testing exposition_of_a_hundred_thousand -- --ignored --nocapture
```

## See also

- [Ingesting](ingesting.md#from-prometheus): the text an exporter serves,
  read into the store.
- [Reading](reading.md): `latest`, names and labels.
- [Instruments](instruments.md): timers and histograms, and what they write.
