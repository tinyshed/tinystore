use std::time::Duration;

use crate::ErrorKind;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Buckets, Description, Kind, LabelPage, NameInfo, Series, ValuePage, maintain};

fn names(f: &Fixture) -> Vec<String> {
    f.metrics().names().page().unwrap().names.into_iter().map(|name| name.name).collect()
}

fn named(f: &Fixture, name: &str) -> NameInfo {
    let mut found = f.metrics().names().prefix(name).page().unwrap().names;
    found.retain(|info| info.name == name);
    found.pop().unwrap()
}

/// A page's labels and its `next`.
fn labels(page: LabelPage) -> (Vec<String>, Option<String>) {
    (page.labels, page.next)
}

fn values(page: ValuePage) -> (Vec<String>, Option<String>) {
    (page.values, page.next)
}

fn text(texts: &[&str], next: Option<&str>) -> (Vec<String>, Option<String>) {
    (texts.iter().map(|text| (*text).to_owned()).collect(), next.map(str::to_owned))
}

#[test]
fn names_give_each_name_with_samples_its_kind_series_description_keep_and_times() {
    let f = fixture();
    let (stats, now) = (f.metrics(), f.now());
    let requests = |route: &str| Series::counter("http_requests_total").label("route", route);
    stats
        .ingest(&[
            requests("/a").sample(now - HOUR, 1.0).sample(now, 5.0),
            requests("/b").sample(now - MINUTE, 2.0).keep(7 * DAY),
            Series::gauge("cpu").label("host", "web-1").sample(now - 2 * HOUR, 0.5),
            Series::gauge("http_in_flight").sample(now - SECOND, 3.0),
        ])
        .unwrap();
    let served = Description { unit: String::new(), help: "Requests served.".into() };
    stats.describe("http_requests_total", served).unwrap();
    // a name described and never written holds no samples
    stats.describe("disk_bytes", Description { unit: "bytes".into(), help: String::new() }).unwrap();
    assert_eq!(names(&f), ["cpu", "http_in_flight", "http_requests_total"]);
    let total = named(&f, "http_requests_total");
    assert_eq!(
        (total.kind, total.series, total.help.as_str(), total.keep),
        (Kind::Counter, 2, "Requests served.", 7 * DAY)
    );
    assert_eq!((total.oldest, total.newest), (now - HOUR, now));
    let cpu = named(&f, "cpu");
    assert_eq!(
        (cpu.kind, cpu.series, cpu.keep, cpu.oldest, cpu.newest),
        (Kind::Gauge, 1, 30 * DAY, now - 2 * HOUR, now - 2 * HOUR)
    );
    let http = stats.names().prefix("http_").page().unwrap();
    let http: Vec<&str> = http.names.iter().map(|name| name.name.as_str()).collect();
    assert_eq!(http, ["http_in_flight", "http_requests_total"]);
    // a page that holds them all says there is no more
    let first = stats.names().limit(2).page().unwrap();
    assert_eq!((first.names.len(), first.next.as_deref()), (2, Some("http_in_flight")));
    let rest = stats.names().limit(2).after(first.next.unwrap()).page().unwrap();
    assert_eq!((rest.names[0].name.as_str(), rest.next), ("http_requests_total", None));
    assert_eq!(stats.names().limit(3).page().unwrap().next, None);
}

#[test]
fn a_names_oldest_is_its_oldest_sample_and_where_its_keep_begins_while_it_holds_older() {
    let f = fixture();
    let (stats, start) = (f.metrics(), f.now());
    stats.ingest(&[f.gauge("cpu", 841, 15 * SECOND, |index| index as f64).label("host", "a")]).unwrap();
    let first = start - 840 * 15 * SECOND;
    assert_eq!(named(&f, "cpu").oldest, first);
    assert_eq!(maintain(&f.store).unwrap().packed, 840);
    assert_eq!(named(&f, "cpu").oldest, first, "packing moves samples, never their times");
    // a sample before every other moves it back
    stats.ingest(&[Series::gauge("cpu").label("host", "b").sample(start - DAY, 1.0)]).unwrap();
    assert_eq!((named(&f, "cpu").oldest, named(&f, "cpu").series), (start - DAY, 2));
    // past what the name keeps, a read finds nothing before its keep begins
    f.pass(30 * DAY - 12 * HOUR);
    assert_eq!(named(&f, "cpu").oldest, f.now() - 30 * DAY);
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.reclaimed, 1, "b's one sample expired");
    assert_eq!((named(&f, "cpu").oldest, named(&f, "cpu").series), (first, 1));
    // once its newest sample is past what it keeps, a name holds nothing a read finds
    f.pass(DAY);
    assert!(names(&f).is_empty(), "before expiry removes them");
    maintain(&f.store).unwrap();
    assert!(names(&f).is_empty());
    assert_eq!(f.value("select count(*) from names where kind is not null or newest is not null"), 0);
}

#[test]
fn a_names_labels_and_a_labels_values_come_in_order_by_prefix_and_page() {
    let f = fixture();
    let (stats, now) = (f.metrics(), f.now());
    let request = |route: &str, status: &str| {
        Series::counter("http_requests_total").label("route", route).label("status", status).sample(now, 1.0)
    };
    let requests =
        [request("/api/notes", "200"), request("/api/users", "200"), request("/", "500"), request("/api/notes", "500")];
    stats.ingest(&requests).unwrap();
    stats.ingest(&[Series::gauge("cpu").label("zone", "a").label("host", "web-1").sample(now, 1.0)]).unwrap();
    let names = |name: &str| stats.label_names(name);
    let routes = || stats.label_values("http_requests_total", "route");
    assert_eq!(labels(names("http_requests_total").page().unwrap()), text(&["route", "status"], None));
    assert_eq!(labels(names("cpu").page().unwrap()), text(&["host", "zone"], None));
    assert_eq!(labels(names("http_requests_total").prefix("s").page().unwrap()), text(&["status"], None));
    assert_eq!(labels(names("http_requests_total").limit(1).page().unwrap()), text(&["route"], Some("route")));
    assert_eq!(labels(names("http_requests_total").after("route").page().unwrap()), text(&["status"], None));
    assert_eq!(values(routes().page().unwrap()), text(&["/", "/api/notes", "/api/users"], None));
    assert_eq!(values(routes().prefix("/api").page().unwrap()), text(&["/api/notes", "/api/users"], None));
    assert_eq!(values(routes().limit(1).after("/").page().unwrap()), text(&["/api/notes"], Some("/api/notes")));
    assert!(stats.label_values("cpu", "route").page().unwrap().values.is_empty(), "another name's label");
    assert!(names("nothing").page().unwrap().labels.is_empty());
    // a value goes with the last series that held it, at once
    assert!(stats.delete("http_requests_total", [("route", "/"), ("status", "500")]).unwrap());
    assert_eq!(values(routes().page().unwrap()), text(&["/api/notes", "/api/users"], None));
    let statuses = stats.label_values("http_requests_total", "status").page().unwrap();
    assert_eq!(values(statuses), text(&["200", "500"], None), "/api/notes still holds 500");
}

#[test]
fn the_stores_own_labels_are_not_among_a_names_labels() {
    let f = fixture();
    let stats = f.metrics();
    let latency = stats.timer("db_ms").buckets(Buckets::Exponential).open().unwrap();
    latency.with(&[("query", "notes")]).record(Duration::from_millis(3));
    stats.flush().unwrap();
    assert_eq!(stats.label_names("db_ms_bucket").page().unwrap().labels, ["query"]);
    let refused = stats.label_values("db_ms_bucket", "__bin").page().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(refused.to_string().contains("metrics db_ms_bucket: label \"__bin\""), "{refused}");
}

#[test]
fn a_page_past_its_bounds_is_refused() {
    let f = fixture();
    let stats = f.metrics();
    assert_eq!(stats.names().limit(0).page().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.names().limit(10_001).page().unwrap_err().kind(), ErrorKind::Invalid);
    stats.names().limit(10_000).page().unwrap();
    assert_eq!(stats.names().prefix("n".repeat(4097)).page().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.label_names("").page().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.label_values("cpu", "").page().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(stats.label_values("cpu", "l".repeat(257)).page().unwrap_err().kind(), ErrorKind::Invalid);
}
