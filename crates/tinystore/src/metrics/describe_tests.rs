use std::time::Duration;

use super::*;
use crate::ErrorKind;
use crate::metrics::Series;
use crate::metrics::fixture::fixture;

fn description(unit: &str, help: &str) -> Description {
    Description { unit: unit.to_owned(), help: help.to_owned() }
}

#[test]
fn a_description_is_kept_by_name_replaced_by_the_next_and_removed_by_an_empty_one() {
    let f = fixture();
    let stats = f.metrics();
    assert_eq!(stats.description("cpu").unwrap(), Description::default(), "nothing described it");
    stats.describe("cpu", description("%", "Busy time of a core.")).unwrap();
    assert_eq!(stats.description("cpu").unwrap(), description("%", "Busy time of a core."));
    stats.describe("cpu", Description { help: "Busy time.".into(), ..Description::default() }).unwrap();
    assert_eq!(stats.description("cpu").unwrap(), description("", "Busy time."), "the next replaces both");
    // the name's, through its series, their deletion and a restart
    stats.ingest(&[Series::gauge("cpu").label("host", "a").sample(f.now(), 1.0)]).unwrap();
    assert_eq!(stats.names().page().unwrap().names[0].help, "Busy time.");
    assert!(stats.delete("cpu", [("host", "a")]).unwrap());
    let f = f.reopen();
    assert_eq!(f.metrics().description("cpu").unwrap(), description("", "Busy time."));
    f.metrics().describe("cpu", Description::default()).unwrap();
    assert_eq!(f.metrics().description("cpu").unwrap(), Description::default());
}

#[test]
fn an_instrument_describes_the_names_it_writes_at_its_next_write_and_describe_replaces_it() {
    let f = fixture();
    let stats = f.metrics();
    let latency = stats.timer("http_request_ms").help("How long a request took.").open().unwrap();
    latency.record(Duration::from_millis(5));
    assert_eq!(stats.description("http_request_ms_sum").unwrap(), Description::default(), "at its next write");
    stats.flush().unwrap();
    assert_eq!(stats.description("http_request_ms_sum").unwrap(), description("ms", "How long a request took."));
    assert_eq!(stats.description("http_request_ms_max").unwrap(), description("ms", "How long a request took."));
    assert_eq!(stats.description("http_request_ms_count").unwrap(), description("", "How long a request took."));
    stats.describe("http_request_ms_sum", description("ms", "Time spent.")).unwrap();
    latency.record(Duration::from_millis(7));
    stats.flush().unwrap();
    assert_eq!(stats.description("http_request_ms_sum").unwrap(), description("ms", "Time spent."));
}

#[test]
fn a_description_past_its_bounds_is_refused_naming_its_name() {
    let f = fixture();
    let stats = f.metrics();
    let refused = stats.describe("cpu", description(&"u".repeat(33), "")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(refused.to_string().starts_with("metrics cpu: a unit of 32 bytes at most"), "{refused}");
    assert_eq!(stats.describe("cpu", description("", &"h".repeat(1025))).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.describe("", Description::default()).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.description("").unwrap_err().kind(), ErrorKind::Invalid);
    stats.describe("cpu", description(&"u".repeat(32), &"h".repeat(1024))).unwrap();
}
