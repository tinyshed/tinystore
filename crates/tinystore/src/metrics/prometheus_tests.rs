use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value as Json;

use super::parse;
use crate::ErrorKind;
use crate::metrics::fixture::{DAY, MINUTE, SECOND, fixture};
use crate::metrics::series::Labels;
use crate::metrics::{Description, Series};

/// Texts and the series each writes, which every SDK reads too.
const VECTORS: &str = include_str!("../../../../testdata/metrics/prometheus.json");

/// A series as a test compares it: its name, kind, labels, and each sample's
/// time and value's bits.
type Seen = (String, &'static str, Labels, Vec<(i64, u64)>);

fn seen(series: &Series) -> Seen {
    let samples = series.samples().iter().map(|sample| (crate::unix_millis(sample.at), sample.value.to_bits()));
    (series.name().to_owned(), series.kind().as_str(), series.labels().clone(), samples.collect())
}

/// A vector's series as the store holds them; a value is spelt as the text
/// format spells it, `NaN`, `+Inf` and `-0` among them.
fn wanted(series: &Json) -> Seen {
    let labels = series["labels"].as_object().unwrap();
    let labels = labels.iter().map(|(label, value)| (label.clone(), value.as_str().unwrap().to_owned())).collect();
    let samples = series["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sample| (sample[0].as_i64().unwrap(), sample[1].as_str().unwrap().parse::<f64>().unwrap().to_bits()));
    let kind = ["gauge", "counter", "histogram"].into_iter().find(|kind| *kind == series["kind"]).unwrap();
    (series["name"].as_str().unwrap().to_owned(), kind, labels, samples.collect())
}

// contract: MET-15
// contract: MET-01
#[test]
fn every_text_of_the_vectors_writes_its_series_or_is_refused_whole() {
    let vectors: Json = serde_json::from_str(VECTORS).unwrap();
    assert_eq!(vectors["keep"], "3650d");
    let (f, keep) = (fixture(), 3650 * DAY);
    let at = UNIX_EPOCH + Duration::from_millis(vectors["at"].as_u64().unwrap());
    let mut written = 0;
    for case in vectors["cases"].as_array().unwrap() {
        let named = case["name"].as_str().unwrap();
        let mut text = f.metrics().prometheus().at(at).keep(keep);
        for (label, value) in case["labels"].as_object().into_iter().flatten() {
            text = text.label(label, value.as_str().unwrap());
        }
        let ingested = text.ingest(case["text"].as_str().unwrap());
        if let Some(refused) = case.get("error") {
            let error = ingested.expect_err(named);
            assert_eq!(error.kind().as_str(), refused["kind"], "{named}: {error}");
            assert!(error.to_string().contains(refused["has"].as_str().unwrap()), "{named}: {error}");
            continue;
        }
        ingested.unwrap_or_else(|error| panic!("{named}: {error}"));
        let mut want: Vec<Seen> = case["series"].as_array().unwrap().iter().map(wanted).collect();
        let mut names: Vec<String> = want.iter().map(|series| series.0.clone()).collect();
        names.sort_unstable();
        names.dedup();
        let mut got = Vec::new();
        for name in names {
            got.extend(f.metrics().query().name(name).since(keep).read().unwrap().iter().map(seen));
        }
        want.sort();
        got.sort();
        assert_eq!(got, want, "{named}");
        written += want.len();
    }
    assert_eq!(f.rows("series"), written as i64, "a refused text writes no series");
}

#[test]
fn a_line_that_is_not_utf8_is_refused_by_its_number() {
    let error = parse(b"up 1\nup{host=\"\xff\"} 2\n", &Labels::new(), 0).unwrap_err();
    assert_eq!((error.kind(), error.what()), (ErrorKind::Invalid, "metrics: line 2: not UTF-8"));
}

#[test]
fn a_negative_nan_keeps_its_sign_and_a_byte_order_mark_is_read_past() {
    let series = parse(b"\xef\xbb\xbfodd -NaN\n", &Labels::new(), 0).unwrap();
    assert_eq!(series[0].samples()[0].value.to_bits(), 0xfff8_0000_0000_0000);
}

#[test]
fn openmetrics_seconds_are_rounded_to_the_millisecond() {
    let text = b"# TYPE late gauge\nlate 1 1789999990.789\nlate 2 1789999990.0016\n# EOF";
    let series = parse(text, &Labels::new(), 0).unwrap();
    let times: Vec<i64> = series.iter().map(|series| crate::unix_millis(series.samples()[0].at)).collect();
    assert_eq!(times, [1_789_999_990_789, 1_789_999_990_002]);
}

#[test]
fn what_follows_a_sample_is_its_time_and_an_exemplar_alone() {
    let given = Labels::new();
    assert!(parse(b"up 1 # {trace_id=\"a b\"} 0.5\n", &given, 7).is_ok());
    let error = parse(b"up 1 2 3\n", &given, 0).unwrap_err();
    assert!(error.what().ends_with("`up 1 2 3`: \"3\" after the sample's time"), "{error}");
    let error = parse(b"up.time 1\n", &given, 0).unwrap_err();
    assert!(error.to_string().contains("a space between the name and the value"), "{error}");
    let error = parse(b"up\n", &given, 0).unwrap_err();
    assert!(error.what().ends_with("a sample without its value"), "{error}");
}

#[test]
fn an_error_quotes_the_start_of_a_long_line() {
    let line = format!("up{{path=\"{}", "é".repeat(100));
    let error = parse(line.as_bytes(), &Labels::new(), 0).unwrap_err();
    let quoted = error.to_string().split('`').nth(1).unwrap().to_owned();
    assert!(quoted.ends_with('…') && quoted.len() <= 64 + '…'.len_utf8(), "{quoted}");
}

// contract: MET-15
#[test]
fn a_text_past_one_ingest_is_written_whole_and_one_past_one_text_is_a_limit() {
    let lines = |count: usize| {
        (0..count)
            .map(|index| {
                format!(
                    "up{{shard=\"{index}\"}} 1
"
                )
            })
            .collect::<String>()
    };
    let f = fixture();
    // more than one ingest carries, one series a line, in one write
    f.metrics().prometheus().ingest(lines(12_000)).unwrap();
    assert_eq!(f.rows("series"), 12_000);
    assert_eq!(parse(lines(100_000).as_bytes(), &Labels::new(), 0).unwrap().len(), 100_000);
    let error = parse(lines(100_001).as_bytes(), &Labels::new(), 0).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert!(error.to_string().starts_with("metrics: line 100001, "), "{error}");
    assert_eq!((error.fact("limit"), error.fact("bound")), (Some("one text"), Some("100000")));
}

// contract: MET-15, MET-03
#[test]
fn a_text_one_sample_of_which_its_write_refuses_writes_nothing() {
    let f = fixture();
    f.metrics().ingest(&[f.gauge("load", 481, 15 * SECOND, |index| index as f64)]).unwrap();
    crate::metrics::maintain(&f.store).unwrap();
    let packed = crate::unix_millis(f.now() - 60 * MINUTE);
    let mut text: String = (0..12_000)
        .map(|index| {
            format!(
                "fresh{{shard=\"{index}\"}} 1
"
            )
        })
        .collect();
    text.push_str(&format!(
        "load 99 {packed}
"
    ));
    let error = f.metrics().prometheus().ingest(text).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
    assert_eq!(f.rows("series"), 1, "none of the text's 12,000 new series");
}

#[test]
fn a_text_without_samples_writes_nothing() {
    let f = fixture();
    f.metrics().prometheus().ingest("# HELP up Whether it is up.\n# TYPE up gauge\n# EOF\n").unwrap();
    f.metrics().prometheus().ingest("").unwrap();
    assert_eq!(f.rows("series"), 0);
}

// contract: MET-15
#[test]
fn a_familys_help_goes_to_each_of_its_names_and_its_unit_to_those_whose_values_are_in_it() {
    let f = fixture();
    let text = "# HELP rpc_seconds How long a call took, \\\\ and \\\"quoted\\\"\\nover two lines.\n\
                # TYPE rpc_seconds histogram\n\
                # UNIT rpc_seconds seconds\n\
                rpc_seconds_bucket{le=\"+Inf\"} 3\nrpc_seconds_sum 1.5\nrpc_seconds_count 3\n\
                # HELP up Whether the target answered.\n\
                up 1\n\
                # HELP quiet\n\
                quiet 0\n";
    f.metrics().prometheus().ingest(text).unwrap();
    let help = "How long a call took, \\ and \"quoted\"\nover two lines.";
    // the sum is in seconds; the buckets and the count count observations
    for (name, unit) in [("rpc_seconds_bucket", ""), ("rpc_seconds_sum", "seconds"), ("rpc_seconds_count", "")] {
        let described = f.metrics().description(name).unwrap();
        assert_eq!(described, Description { unit: unit.into(), help: help.into() }, "{name}");
    }
    assert_eq!(f.metrics().description("up").unwrap().help, "Whether the target answered.");
    assert_eq!(f.metrics().description("quiet").unwrap(), Description::default());
}

// contract: MET-15
#[test]
fn what_describe_gave_a_name_stays_and_a_scrape_fills_only_what_it_left_empty() {
    let f = fixture();
    f.metrics().describe("load", Description { unit: "%".into(), help: String::new() }).unwrap();
    let text = "# HELP load How busy the host is.\n# UNIT load ratio\n# TYPE load gauge\nload 0.4\n";
    f.metrics().prometheus().ingest(text).unwrap();
    let kept = Description { unit: "%".into(), help: "How busy the host is.".into() };
    assert_eq!(f.metrics().description("load").unwrap(), kept);
    // a later help in the text, and a describe after it
    f.metrics().prometheus().ingest(text.replace("How busy", "How loaded")).unwrap();
    assert_eq!(f.metrics().description("load").unwrap(), kept);
    f.metrics().describe("load", Description { unit: "%".into(), help: "Load.".into() }).unwrap();
    f.metrics().prometheus().ingest(text).unwrap();
    assert_eq!(f.metrics().description("load").unwrap().help, "Load.");
}

#[test]
fn a_help_past_what_a_name_keeps_or_one_that_does_not_read_is_left_out_and_the_text_written() {
    let f = fixture();
    let long = "x".repeat(1025);
    let text = format!(
        "# HELP long {long}\n# UNIT long a very long unit name past thirty two bytes\nlong 1\n# HELP \"open 2\nopen 2\n"
    );
    f.metrics().prometheus().ingest(text).unwrap();
    assert_eq!(f.metrics().description("long").unwrap(), Description::default());
    assert_eq!(f.rows("series"), 2);
}

/// What a text of 100,000 series holds the file's writer for, the first
/// scrape, which registers every series, and the next, against ten texts of
/// 10,000.
///
/// `cargo test -p tinystore --release --lib metrics::prometheus::tests::timing -- --ignored --nocapture`
#[test]
#[ignore = "a timing, run by hand"]
fn timing_one_text_of_a_hundred_thousand_series() {
    let lines = |from: usize, count: usize, value: usize| {
        (from..from + count).map(|index| format!("up{{shard=\"{index}\"}} {value}\n")).collect::<String>()
    };
    let f = fixture();
    for scrape in 0..3 {
        f.pass(std::time::Duration::from_secs(15));
        let text = lines(0, 99_999, scrape);
        let started = std::time::Instant::now();
        f.metrics().prometheus().ingest(&text).unwrap();
        println!("one text, scrape {scrape}: {:?}", started.elapsed());
    }
    let g = fixture();
    for scrape in 0..3 {
        g.pass(std::time::Duration::from_secs(15));
        let parts: Vec<String> = (0..10).map(|part| lines(part * 10_000, 9_999, scrape)).collect();
        let started = std::time::Instant::now();
        let mut longest = std::time::Duration::ZERO;
        for part in &parts {
            let one = std::time::Instant::now();
            g.metrics().prometheus().ingest(part).unwrap();
            longest = longest.max(one.elapsed());
        }
        println!("ten texts, scrape {scrape}: {:?}, the longest {longest:?}", started.elapsed());
    }
}
