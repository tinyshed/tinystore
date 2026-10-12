use std::time::Duration;

use crate::ErrorKind;
use crate::metrics::fixture::{Fixture, MINUTE, SECOND, bits, fixture};
use crate::metrics::{Buckets, Labels, Series, maintain};

/// Values a block cannot keep inline, so that packing gives them payloads.
fn noisy(index: usize) -> f64 {
    (index * 7919 % 1009) as f64 * 0.37
}

/// Every row a series leaves behind in the file, by table.
fn left(f: &Fixture) -> Vec<i64> {
    ["series", "series_state", "heads", "groups", "payloads", "clocks", "postings", "label_values", "name_labels"]
        .iter()
        .map(|table| f.rows(table))
        .collect()
}

#[test]
fn a_series_is_deleted_whole_and_its_neighbours_keep_theirs() {
    let f = fixture();
    let stats = f.metrics();
    let gone = f.gauge("cpu", 841, 15 * SECOND, noisy).label("host", "a").label("zone", "eu");
    let stays = f.gauge("cpu", 841, 15 * SECOND, |index| noisy(index + 1)).label("host", "b").label("zone", "eu");
    stats.ingest(&[gone, stays.clone()]).unwrap();
    assert_eq!(maintain(&f.store).unwrap().blocks, 8);
    assert!(f.rows("payloads") > 0);
    assert!(stats.delete("cpu", [("host", "a"), ("zone", "eu")]).unwrap());
    assert!(!stats.delete("cpu", [("zone", "eu"), ("host", "a")]).unwrap(), "nothing left of it");
    assert!(!stats.delete("cpu", [("host", "b")]).unwrap(), "exactly these labels, no fewer");
    let read = stats.query().name("cpu").read().unwrap();
    assert_eq!((read.len(), bits(&read[0])), (1, bits(&stays)));
    assert_eq!(stats.label_values("cpu", "host").page().unwrap().values, ["b"]);
    assert_eq!(stats.names().page().unwrap().names[0].series_count, 1);
    // the last series takes everything with it, the name's kind among them
    assert!(stats.delete("cpu", stays.labels()).unwrap());
    assert_eq!(left(&f), [0; 9]);
    assert_eq!(f.value("select series_count from store_state"), 0);
    assert!(stats.names().page().unwrap().names.is_empty());
    stats.ingest(&[Series::counter("cpu").sample(f.now(), 1.0)]).unwrap();
}

#[test]
fn a_name_whose_newest_series_is_deleted_finds_its_newest_among_the_rest() {
    let f = fixture();
    let (stats, now) = (f.metrics(), f.now());
    let host = |host: &str, at| Series::gauge("cpu").label("host", host).sample(at, 1.0);
    stats.ingest(&[host("a", now), host("b", now - MINUTE), host("c", now - 2 * MINUTE)]).unwrap();
    assert!(stats.delete("cpu", [("host", "b")]).unwrap());
    assert_eq!(stats.names().page().unwrap().names[0].newest, now, "another series' time");
    assert!(stats.delete("cpu", [("host", "a")]).unwrap());
    assert_eq!(stats.names().page().unwrap().names[0].newest, now - 2 * MINUTE);
}

#[test]
fn a_series_of_the_stores_own_labels_and_one_an_instrument_writes_again_are_deleted() {
    let f = fixture();
    let stats = f.metrics();
    let latency = stats.timer("db_ms").buckets(Buckets::Exponential).open().unwrap();
    latency.record(Duration::from_millis(1));
    stats.flush().unwrap();
    let bins = stats.query().name("db_ms_bucket").read().unwrap();
    assert_eq!(bins.len(), 1);
    assert!(stats.delete("db_ms_bucket", bins[0].labels()).unwrap());
    assert!(stats.query().name("db_ms_bucket").read().unwrap().is_empty());
    // the instrument's next write starts a new series
    latency.record(Duration::from_millis(1));
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(stats.query().name("db_ms_bucket").read().unwrap().len(), 1);
}

#[test]
fn a_delete_names_its_series_and_refuses_what_no_series_is() {
    let f = fixture();
    let stats = f.metrics();
    let refused = stats.delete("cpu", [("host", "a"), ("__name__", "cpu")]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(
        refused.to_string().starts_with("metrics cpu{__name__=\"cpu\",host=\"a\"}: label \"__name__\" twice"),
        "{refused}"
    );
    assert_eq!(stats.delete("", Labels::new()).unwrap_err().kind(), ErrorKind::Invalid);
    assert!(!stats.delete("cpu", Labels::new()).unwrap());
}
