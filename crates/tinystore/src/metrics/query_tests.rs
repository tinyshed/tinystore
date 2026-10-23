use std::time::Duration;

use crate::ErrorKind;
use crate::metrics::fixture::{HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Not, Prefix, Series};

/// Each series read as its name and host, sorted: series come back in the
/// order of their ids, which a call gives them by their identities.
fn hosts(read: &[Series]) -> Vec<String> {
    let mut hosts: Vec<String> = read
        .iter()
        .map(|series| format!("{}/{}", series.name(), series.labels().get("host").map_or("-", |host| host)))
        .collect();
    hosts.sort();
    hosts
}

fn written(f: &crate::metrics::fixture::Fixture) {
    let now = f.now();
    let series = [
        ("http_requests_total", "api-1", "500", "prod"),
        ("http_requests_total", "api-2", "502", "dev"),
        ("http_requests_total", "web-1", "200", "prod"),
        ("http_requests_total", "api-3", "200", "test"),
        ("cpu", "api-1", "", "prod"),
    ];
    let series: Vec<Series> = series
        .iter()
        .map(|(name, host, status, env)| {
            let one = Series::counter(*name).label("host", host).label("env", env).sample(now, 1.0);
            if status.is_empty() { one } else { one.label("status", status) }
        })
        .collect();
    f.metrics().ingest(&series).unwrap();
}

#[test]
fn a_condition_finds_series_beyond_equality() {
    let f = fixture();
    written(&f);
    let query = || f.metrics().query().since(HOUR);
    let failing = query().name("http_requests_total").filter("status", ["500", "502"]).read().unwrap();
    assert_eq!(hosts(&failing), ["http_requests_total/api-1", "http_requests_total/api-2"]);
    let not_dev = query().name("http_requests_total").filter("env", Not("dev")).read().unwrap();
    assert_eq!(
        hosts(&not_dev),
        ["http_requests_total/api-1", "http_requests_total/api-3", "http_requests_total/web-1"]
    );
    let api = query().filter("host", Prefix("api-")).filter("env", Not(["dev", "test"])).read().unwrap();
    assert_eq!(hosts(&api), ["cpu/api-1", "http_requests_total/api-1"]);
    let one_host = query().filter("host", "api-1").read().unwrap();
    assert_eq!(hosts(&one_host), ["cpu/api-1", "http_requests_total/api-1"]);
    let a_number = query().filter("status", 200).read().unwrap();
    assert_eq!(hosts(&a_number), ["http_requests_total/api-3", "http_requests_total/web-1"]);
    // a series without the label is not one of a list, and is none of it
    let without = query().filter("env", "prod").filter("status", Not("500")).read().unwrap();
    assert_eq!(hosts(&without), ["cpu/api-1", "http_requests_total/web-1"]);
    assert!(query().name("nothing").read().unwrap().is_empty());
    assert!(query().filter("host", Prefix("zzz")).read().unwrap().is_empty());
    assert!(query().name("cpu").filter("status", ["500"]).read().unwrap().is_empty());
}

#[test]
fn a_condition_that_finds_nothing_or_cannot_be_taken_is_refused() {
    let f = fixture();
    written(&f);
    let refused = |query: crate::metrics::Query| query.read().unwrap_err();
    let error = refused(f.metrics().query().filter("env", Not("dev")));
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains("`Not` alone"), "{error}");
    assert_eq!(refused(f.metrics().query()).kind(), ErrorKind::Invalid);
    assert_eq!(refused(f.metrics().query().filter("__name__", "cpu")).kind(), ErrorKind::Invalid);
    assert_eq!(refused(f.metrics().query().filter("host", Vec::<String>::new())).kind(), ErrorKind::Invalid);
    assert_eq!(refused(f.metrics().query().filter("host", Prefix(""))).kind(), ErrorKind::Invalid);
    let both = f.metrics().query().name("cpu").since(HOUR).from(f.now());
    assert_eq!(refused(both).kind(), ErrorKind::Invalid);
    let inverted = f.metrics().query().name("cpu").from(f.now()).to(f.now() - SECOND);
    assert_eq!(refused(inverted).kind(), ErrorKind::Invalid);
}

#[test]
fn a_range_is_its_start_to_its_end_excluded_or_now_included() {
    let f = fixture();
    let now = f.now();
    let mut minutes = Series::gauge("m");
    minutes.extend((0..10_u32).map(|n| (now - MINUTE * (9 - n), f64::from(n))));
    f.metrics().ingest(&[minutes]).unwrap();
    let values = |read: Vec<Series>| {
        read.first().map_or(Vec::new(), |series| series.samples().iter().map(|s| s.value).collect::<Vec<_>>())
    };
    let query = || f.metrics().query().name("m");
    assert_eq!(values(query().since(3 * MINUTE).read().unwrap()), [6.0, 7.0, 8.0, 9.0]);
    assert_eq!(values(query().from(now - 5 * MINUTE).to(now - 2 * MINUTE).read().unwrap()), [4.0, 5.0, 6.0]);
    assert_eq!(values(query().to(now - 8 * MINUTE).read().unwrap()), [0.0]);
    assert!(values(query().from(now + SECOND).read().unwrap()).is_empty());
    f.pass(Duration::from_millis(1));
    assert_eq!(values(query().since(Duration::ZERO).read().unwrap()), Vec::<f64>::new());
}

// contract: ERR-01, ERR-05, MET-07
#[test]
fn a_read_past_a_bound_is_a_limit_naming_it() {
    let f = fixture();
    let now = f.now();
    let series: Vec<Series> = (0..1001).map(|n| Series::gauge("wide").label("n", n).sample(now, 1.0)).collect();
    f.metrics().ingest(&series).unwrap();
    let error = f.metrics().query().name("wide").read().unwrap_err();
    assert_eq!(
        (error.kind(), error.fact("limit"), error.fact("bound")),
        (ErrorKind::Limit, Some("series"), Some("1000"))
    );
    let fits = f.metrics().query().name("wide").filter("n", Not("0")).read().unwrap();
    assert_eq!(fits.len(), 1000);
}

#[test]
fn a_read_or_an_aggregate_whose_token_was_cancelled_fails_cancelled() {
    // contract: STORE-09
    let f = fixture();
    written(&f);
    let token = crate::CancellationToken::new();
    token.cancel();
    let query = || f.metrics().query().name("http_requests_total").since(HOUR);
    let read = token.run(|| query().read()).unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Cancelled, "{read}");
    assert!(read.to_string().starts_with("metrics: "), "{read}");
    let summed = token.run(|| query().aggregate(crate::metrics::Op::Sum)).unwrap_err();
    assert_eq!(summed.kind(), ErrorKind::Cancelled, "{summed}");
    assert_eq!(query().read().unwrap().len(), 4, "a read without the token answers");
}
