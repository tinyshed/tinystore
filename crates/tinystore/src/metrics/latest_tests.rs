use std::time::{Duration, SystemTime};

use crate::metrics::budget::Budget;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::snapshot::Want;
use crate::metrics::{Query, Series, maintain};

/// Each series' newest sample as its host, its time and its value, sorted.
fn newest(query: Query) -> Vec<(String, SystemTime, f64)> {
    let mut newest: Vec<_> = query
        .latest()
        .unwrap()
        .into_iter()
        .map(|latest| (latest.labels["host"].clone(), latest.at, latest.value))
        .collect();
    newest.sort_by(|a, b| a.0.cmp(&b.0));
    newest
}

/// The samples `latest` decodes for a query.
fn decoded(f: &Fixture, query: Query) -> usize {
    let mut budget = Budget::default();
    f.engine().take(&query.asked().unwrap(), Want::Newest, &mut budget).unwrap();
    budget.decoded
}

#[test]
fn latest_is_the_newest_sample_of_each_series_that_has_one_in_the_range() {
    let f = fixture();
    let now = f.now();
    let cpu = |host: &str| Series::gauge("cpu").label("host", host);
    let written = [
        cpu("up").sample(now - 2 * MINUTE, 1.0).sample(now - MINUTE, 2.0).sample(now, 3.0),
        // a host that went down half an hour ago
        cpu("down").sample(now - 30 * MINUTE, 9.0),
    ];
    f.metrics().ingest(&written).unwrap();
    let query = || f.metrics().query().name("cpu");
    assert_eq!(newest(query().since(5 * MINUTE)), [("up".to_owned(), now, 3.0)], "the range bounds how stale");
    let hour = newest(query().since(HOUR));
    assert_eq!(hour, [("down".to_owned(), now - 30 * MINUTE, 9.0), ("up".to_owned(), now, 3.0)]);
    assert_eq!(
        newest(query().to(now)),
        [hour[0].clone(), ("up".to_owned(), now - MINUTE, 2.0)],
        "its end is not in it"
    );
    assert_eq!(newest(query().from(now - 100 * SECOND).to(now - 70 * SECOND)), [], "between two samples");
    let latest = query().filter("host", "up").latest().unwrap();
    assert_eq!((latest[0].name.as_str(), latest.len()), ("cpu", 1));
    assert!(query().name("nothing").latest().unwrap().is_empty());
}

#[test]
fn latest_reads_one_small_piece_of_each_series_however_long_the_range() {
    let f = fixture();
    let now = f.now();
    // eight hours of a sample every 15 seconds, the last at the clock
    let written = f.gauge("cpu", 1921, 15 * SECOND, |index| index as f64).label("host", "a");
    f.metrics().ingest(&[written]).unwrap();
    let query = || f.metrics().query().name("cpu");
    assert_eq!(newest(query().since(DAY)), [("a".to_owned(), now, 1920.0)]);
    assert_eq!(decoded(&f, query().since(DAY)), 1, "the head's newest chunk of its nine");
    assert_eq!(decoded(&f, query().to(now - 3 * HOUR)), 240, "the chunk the range ends in");
    assert_eq!(maintain(&f.store).unwrap().blocks, 8);
    assert_eq!(newest(query().since(DAY)), [("a".to_owned(), now, 1920.0)]);
    assert_eq!(decoded(&f, query().since(DAY)), 1, "the one sample not packed yet");
    // a range that ends where a block does takes its last sample from the directory
    assert_eq!(newest(query().to(now)), [("a".to_owned(), now - 15 * SECOND, 1919.0)]);
    assert_eq!(decoded(&f, query().to(now)), 0);
    assert_eq!(newest(query().to(now - 3 * HOUR)), [("a".to_owned(), now - 3 * HOUR - 15 * SECOND, 1199.0)]);
    assert_eq!(decoded(&f, query().to(now - 3 * HOUR)), 0);
    // one that ends inside a block decodes that block, and no other
    let inside = now - 30 * MINUTE;
    assert_eq!(newest(query().to(inside)), [("a".to_owned(), inside - 15 * SECOND, 1799.0)]);
    assert_eq!(decoded(&f, query().to(inside)), 240);
    assert_eq!(newest(query().from(inside + SECOND).to(inside + Duration::from_secs(10))), []);
    // one that begins after the series' last block ended costs no directory
    f.pass(DAY);
    let mut budget = Budget::default();
    let stale = f.metrics().query().name("cpu").since(HOUR).asked().unwrap();
    assert!(f.engine().take(&stale, Want::Newest, &mut budget).unwrap()[0].blocks.is_empty());
    assert!(budget.bytes < 64, "its labels alone: {budget:?}");
}
