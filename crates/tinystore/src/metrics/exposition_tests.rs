use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::metrics::fixture::{Fixture, HOUR, MINUTE, fixture};
use crate::metrics::{Kind, Labels, Latest, Metrics, Series, Written};
use crate::{ErrorKind, unix_millis};

/// The text an exposition writes, and what it says it wrote.
fn exposed(stats: &Metrics) -> (String, Written) {
    let mut text = Vec::new();
    let written = stats.exposition().write(&mut text).unwrap();
    (String::from_utf8(text).unwrap(), written)
}

/// Every name a store holds with its kind, help and unit, and each of its series'
/// labels with its newest sample's time and the bits of its value.
type Held = BTreeMap<String, (Kind, String, String, BTreeMap<Labels, (i64, u64)>)>;

fn held(stats: &Metrics) -> Held {
    let names = stats.names().limit(10_000).page().unwrap();
    assert!(names.next.is_none());
    let of = |name: &str| {
        newest_of_every_series(stats, name)
            .into_iter()
            .map(|latest| (latest.labels, (unix_millis(latest.at), latest.value.to_bits())))
            .collect()
    };
    names.names.into_iter().map(|info| (info.name.clone(), (info.kind, info.help, info.unit, of(&info.name)))).collect()
}

/// `latest` of every series of a name, a part at a time.
fn newest_of_every_series(stats: &Metrics, name: &str) -> Vec<Latest> {
    let asked = stats.query().name(name).asked().unwrap();
    let (mut from, mut all) = (i64::MIN, Vec::new());
    loop {
        let (part, next) = stats.engine.latest_from(&asked, from).unwrap();
        all.extend(part);
        let Some(next) = next else { return all };
        from = next;
    }
}

/// A text ingested into a second store, and what that store then holds.
fn read_back(text: &str) -> (Fixture, Held) {
    let other = fixture();
    other.metrics().prometheus().ingest(text).unwrap();
    let held = held(&other.metrics());
    (other, held)
}

/// What a store holds as a text can carry it: any NaN reads back as the one
/// NaN a parser makes, its payload being no part of the text, and a
/// histogram's unit is read onto its `_sum` alone, so its `_bucket` has none.
fn as_text_carries(mut held: Held) -> Held {
    let nan = f64::NAN.to_bits();
    for (kind, _, unit, series) in held.values_mut() {
        if *kind == Kind::Histogram {
            unit.clear();
        }
        for (_, bits) in series.values_mut() {
            if f64::from_bits(*bits).is_nan() {
                *bits = nan;
            }
        }
    }
    held
}

const SCRAPED: &str = r#"# HELP rpc_seconds How long a call takes.
# TYPE rpc_seconds histogram
rpc_seconds_bucket{le="0.1"} 240
rpc_seconds_bucket{le="+Inf"} 320
rpc_seconds_sum 41.7
rpc_seconds_count 320
# HELP http_requests_total Requests served,\nby route; a \\ too.
# TYPE http_requests_total counter
http_requests_total{route="/a\"quoted\"",status="200"} 1027
http_requests_total{route="C:\\temp\nnext",status="500"} 3
# TYPE "app.load" gauge
{"app.load","url.scheme"="https"} 0.25
"#;

// contract: MET-31
#[test]
fn the_text_an_exposition_writes_is_read_back_to_the_same_series_kinds_help_units_and_newest_samples() {
    let f = fixture();
    let stats = f.metrics();
    stats.prometheus().at(f.now() - MINUTE).ingest(SCRAPED).unwrap();
    let odd = |value| Series::gauge("odd").sample(f.now(), value);
    let values = [-0.0, f64::INFINITY, f64::NEG_INFINITY, f64::from_bits(0x7ff8_0000_0000_0001), 0.1, 1e300, 5e-324];
    let written: Vec<Series> = values.iter().enumerate().map(|(at, value)| odd(*value).label("at", at)).collect();
    stats.ingest(&written).unwrap();
    let calls = stats.timer("call_ms").help("Calls made.").buckets(&[5.0, 10.0]).open().unwrap();
    calls.with(&[("route", "/x")]).record(Duration::from_millis(7));
    let (text, written) = exposed(&stats);
    let (_other, again) = read_back(&text);
    let before = held(&stats);
    assert_eq!(as_text_carries(again), as_text_carries(before.clone()), "{text}");
    let series: usize = before.values().map(|(_, _, _, series)| series.len()).sum();
    assert_eq!(written, Written { names: before.len() as u64, series: series as u64 });
    assert!(text.contains("rpc_seconds_bucket{le=\"+Inf\"} 320 1799999940000\nrpc_seconds_sum 41.7"), "{text}");
    assert!(text.contains("# TYPE call_ms histogram\n"), "a timer's spread is a histogram: {text}");
    assert!(text.contains("# TYPE call_ms_max gauge\n"), "its longest a gauge: {text}");
    let mut odd_lines: Vec<&str> = text.lines().filter(|line| line.starts_with("odd{")).collect();
    odd_lines.sort_unstable();
    let spelt: Vec<&str> = odd_lines.iter().map(|line| line.split(' ').nth(1).unwrap()).collect();
    assert_eq!(spelt, ["-0", "+Inf", "-Inf", "NaN", "0.1", "1e300", "5e-324"]);
}

#[test]
fn an_exposition_writes_each_family_once_its_comments_before_its_series() {
    let f = fixture();
    let stats = f.metrics();
    stats.prometheus().at(f.now()).ingest(SCRAPED).unwrap();
    let (text, _) = exposed(&stats);
    let at = unix_millis(f.now());
    let expected = format!(
        "# TYPE \"app.load\" gauge\n\
         {{\"app.load\",\"url.scheme\"=\"https\"}} 0.25 {at}\n\
         # HELP http_requests_total Requests served,\\nby route; a \\\\ too.\n\
         # TYPE http_requests_total counter\n\
         http_requests_total{{route=\"C:\\\\temp\\nnext\",status=\"500\"}} 3 {at}\n\
         http_requests_total{{route=\"/a\\\"quoted\\\"\",status=\"200\"}} 1027 {at}\n\
         # HELP rpc_seconds How long a call takes.\n\
         # TYPE rpc_seconds histogram\n\
         rpc_seconds_bucket{{le=\"0.1\"}} 240 {at}\n\
         rpc_seconds_bucket{{le=\"+Inf\"}} 320 {at}\n\
         rpc_seconds_sum 41.7 {at}\n\
         rpc_seconds_count 320 {at}\n"
    );
    assert_eq!(text, expected);
}

// contract: MET-32
#[test]
fn every_series_of_a_name_is_written_past_what_one_query_finds() {
    let f = fixture();
    let stats = f.metrics();
    let many: Vec<Series> = (0..2500)
        .map(|shard| Series::gauge("shard_load").label("shard", shard).sample(f.now(), shard as f64))
        .collect();
    for part in many.chunks(1000) {
        stats.ingest(part).unwrap();
    }
    let (text, written) = exposed(&stats);
    assert_eq!(written, Written { names: 1, series: 2500 });
    let lines: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    assert_eq!(lines.len(), 2500, "each series once");
    let (_other, again) = read_back(&text);
    assert_eq!(again, held(&stats));
}

#[test]
fn a_prefix_and_a_since_choose_the_names_and_the_series_written() {
    let f = fixture();
    let stats = f.metrics();
    let gauge =
        |name: &str, host: &str, ago: Duration| Series::gauge(name).label("host", host).sample(f.now() - ago, 1.0);
    stats
        .ingest(&[gauge("shop_orders", "a", MINUTE), gauge("shop_orders", "b", 2 * HOUR), gauge("cpu", "a", MINUTE)])
        .unwrap();
    let write = |exposition: crate::metrics::Exposition| {
        let mut text = Vec::new();
        let written = exposition.write(&mut text).unwrap();
        (String::from_utf8(text).unwrap(), written)
    };
    let (text, written) = write(stats.exposition().only_prefix("shop_"));
    assert_eq!((written.names, written.series), (1, 2), "a silent series is still written: {text}");
    assert!(!text.contains("cpu"));
    let (text, written) = write(stats.exposition().only_prefix("shop_").since(HOUR));
    assert_eq!((written.names, written.series), (1, 1));
    assert!(text.contains("host=\"a\"") && !text.contains("host=\"b\""), "{text}");
    let (text, written) = write(stats.exposition().only_prefix("nothing_"));
    assert_eq!((text.as_str(), written), ("", Written::default()));
}

#[test]
fn a_bucket_whose_family_name_the_store_holds_is_untyped_and_its_parts_counters() {
    let f = fixture();
    let stats = f.metrics();
    let text = "# TYPE rpc histogram\nrpc_bucket{le=\"+Inf\"} 2\nrpc_sum 3\nrpc_count 2\n";
    stats.prometheus().at(f.now()).ingest(text).unwrap();
    stats.ingest(&[Series::gauge("rpc").sample(f.now(), 1.0)]).unwrap();
    let (text, written) = exposed(&stats);
    assert_eq!(written, Written { names: 4, series: 4 });
    for family in ["rpc gauge", "rpc_bucket untyped", "rpc_count counter", "rpc_sum counter"] {
        assert!(text.contains(&format!("# TYPE {family}\n")), "{family}: {text}");
    }
    let (_other, again) = read_back(&text);
    assert_eq!(again["rpc_bucket"].0, Kind::Gauge, "untyped reads back as a gauge");
}

#[test]
fn a_writer_that_fails_fails_the_exposition() {
    struct Refusing;
    impl std::io::Write for Refusing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = fixture();
    f.metrics().ingest(&[Series::gauge("cpu").sample(f.now(), 1.0)]).unwrap();
    let error = f.metrics().exposition().write(&mut Refusing).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io, "{error}");
    assert!(error.to_string().starts_with("metrics: an exposition"), "{error}");
}

/// Writes 100,000 series of 100 names, then times one exposition of them
/// and samples the store's memory the exposition holds while it runs.
///
/// `cargo test --release -p tinystore --lib --features testing exposition_of_a_hundred_thousand -- --ignored --nocapture`
#[test]
#[ignore = "a timing, run by hand"]
fn exposition_of_a_hundred_thousand_series_takes() {
    let f = fixture();
    let stats = f.metrics();
    for name in 0..100 {
        let series: Vec<Series> = (0..1000)
            .map(|host| Series::gauge(format!("load_{name:03}")).label("host", host).sample(f.now(), host as f64))
            .collect();
        stats.ingest(&series).unwrap();
    }
    let memory = std::sync::Arc::clone(&f.engine().memory);
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (memory, done) = (std::sync::Arc::clone(&memory), std::sync::Arc::clone(&done));
        std::thread::spawn(move || {
            let mut peak = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                peak = peak.max(memory.used());
                std::thread::sleep(Duration::from_micros(50));
            }
            peak
        })
    };
    let mut passes = Vec::new();
    let mut bytes = 0;
    for _ in 0..5 {
        let mut sink = std::io::sink();
        let start = Instant::now();
        let mut counted = Counted { inner: &mut sink, bytes: 0, largest: 0 };
        let written = stats.exposition().write(&mut counted).unwrap();
        passes.push(start.elapsed());
        assert_eq!(written.series, 100_000);
        bytes = bytes.max(counted.largest);
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let peak = sampler.join().unwrap();
    passes.sort();
    println!(
        "exposition of 100,000 series over 100 names: median {:?} of {passes:?}; store memory sampled at most {peak} bytes; \
         the largest write {bytes} bytes",
        passes[2]
    );
}

/// A writer that counts what it is given and the largest of its writes.
struct Counted<'a, W: std::io::Write> {
    inner: &'a mut W,
    bytes: usize,
    largest: usize,
}

impl<W: std::io::Write> std::io::Write for Counted<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len();
        self.largest = self.largest.max(buf.len());
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
