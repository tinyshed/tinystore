use std::time::{Duration, SystemTime};

use super::*;
use crate::ErrorKind;
use crate::metrics::fixture::{HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Aggregate, Query, Series, Value, maintain};

fn ms(taken: u64) -> Duration {
    Duration::from_millis(taken)
}

/// Each bucket of the one group an aggregate answered, as its number or
/// its bounds, and its samples.
fn answered(answer: &[Aggregate]) -> Vec<(Value, u64)> {
    assert_eq!(answer.len(), 1, "{answer:?}");
    answer[0].buckets.iter().map(|bucket| (bucket.value, bucket.samples)).collect()
}

fn between(lower: f64, upper: f64) -> Value {
    Value::Between { lower, upper, estimate: None }
}

#[test]
fn a_series_is_a_part_of_the_timer_a_query_named_by_the_end_of_its_name() {
    let timer = Some("http_request_ms");
    assert_eq!(Part::of("http_request_ms_count", timer), Part::Count);
    assert_eq!(Part::of("http_request_ms_max", timer), Part::Longest);
    assert_eq!(Part::of("http_request_ms_bucket", timer), Part::Buckets);
    assert_eq!(Part::of("http_request_ms", timer), Part::Own);
    assert_eq!(Part::of("http_request_ms_bucket", None), Part::Own, "a query without a name reads series alone");
    assert_eq!(Part::Sum.name("db_ms"), "db_ms_sum");
}

#[test]
fn a_timer_is_read_by_its_own_name() {
    let f = fixture();
    let stats = f.metrics();
    let start = f.now();
    let latency = stats.timer("http_request_ms").buckets(&[10.0, 100.0]).open().unwrap();
    let users = latency.with(&[("route", "/users")]);
    [4, 8, 50].into_iter().for_each(|taken| users.record(ms(taken)));
    latency.with(&[("route", "/notes")]).record(ms(200));
    stats.flush().unwrap();
    f.pass(MINUTE);
    users.record(ms(20));
    stats.flush().unwrap();
    // a minute that measured nothing: the count and the sum are written, the longest is not
    f.pass(MINUTE);
    stats.flush().unwrap();
    let minutes = || f.metrics().query().name("http_request_ms").from(start).to(start + 3 * MINUTE).every(MINUTE);
    let numbers = |op| answered(&minutes().aggregate(op).unwrap());
    let exact = |values: &[(f64, u64)]| -> Vec<(Value, u64)> {
        values.iter().map(|(value, samples)| (Value::Exact(*value), *samples)).collect()
    };
    // the samples of each bucket are how many durations
    assert_eq!(numbers(Op::Count), exact(&[(4.0, 4), (1.0, 1), (0.0, 0)]));
    assert_eq!(numbers(Op::Sum), exact(&[(262.0, 4), (20.0, 1), (0.0, 0)]));
    assert_eq!(numbers(Op::Rate), exact(&[(4.0 / 60.0, 4), (1.0 / 60.0, 1), (0.0, 0)]));
    // a minute that measured nothing has no mean, no longest and no percentile
    assert_eq!(numbers(Op::Avg), exact(&[(65.5, 4), (20.0, 1)]));
    assert_eq!(numbers(Op::Max), exact(&[(200.0, 4), (20.0, 1)]));
    assert_eq!(numbers(Op::Percentile(50.0)), [(between(0.0, 10.0), 4), (between(10.0, 100.0), 1)]);
    assert_eq!(numbers(Op::Percentile(95.0))[0], (between(100.0, f64::INFINITY), 4));
    let routes = minutes().by(&["route"]).aggregate(Op::Avg).unwrap();
    let names: Vec<(&str, &str)> =
        routes.iter().map(|group| (group.name.as_str(), group.labels["route"].as_str())).collect();
    assert_eq!(names, [("http_request_ms", "/notes"), ("http_request_ms", "/users")], "a group bears the timer's name");
    assert_eq!(routes[1].buckets[0].value, Value::Exact(62.0 / 3.0));
    // a read and the newest values give the series it writes, each by its own name
    let read = f.metrics().query().name("http_request_ms").since(HOUR).read().unwrap().series;
    let mut written: Vec<String> = read.iter().map(|series| series.name().to_owned()).collect();
    written.sort_unstable();
    written.dedup();
    assert_eq!(
        written,
        ["http_request_ms_bucket", "http_request_ms_count", "http_request_ms_max", "http_request_ms_sum"]
    );
    assert_eq!(read.len(), 12, "two routes, each three buckets, a count, a sum and a longest");
    assert_eq!(f.metrics().query().name("http_request_ms").since(HOUR).latest().unwrap().len(), 12);
}

#[test]
fn an_op_a_timer_has_not_and_a_part_it_keeps_none_of_are_invalid() {
    let f = fixture();
    let stats = f.metrics();
    stats.timer("db_ms").open().unwrap().record(ms(3));
    stats.timer("api_ms").buckets(&[10.0]).open().unwrap().record(ms(3));
    stats.flush().unwrap();
    // a histogram a scrape brought keeps no longest
    let now = f.now();
    let counted = |name: &str| Series::counter(name).sample(now - MINUTE, 0.0).sample(now, 4.0);
    let bucket =
        |le: &str| Series::histogram("rpc_ms_bucket").label("le", le).sample(now - MINUTE, 0.0).sample(now, 4.0);
    f.metrics().ingest(&[counted("rpc_ms_count"), counted("rpc_ms_sum"), bucket("10"), bucket("+Inf")]).unwrap();
    let refused = |name: &str, query: fn(Query) -> Query, op: Op| {
        let error = query(f.metrics().query().name(name).since(HOUR)).aggregate(op).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        error.to_string()
    };
    let same = |query: Query| query;
    for op in [Op::Min, Op::Increase, Op::Delta, Op::First, Op::Last] {
        assert!(refused("db_ms", same, op).contains(&format!("metrics: db_ms: {op} of a timer")), "{op}");
    }
    assert!(refused("db_ms", same, Op::Percentile(99.0)).contains("a timer without buckets has no percentile"));
    assert!(refused("rpc_ms", same, Op::Max).contains("max of a timer that keeps no rpc_ms_max"));
    let by_bound = refused("api_ms", |query| query.by(&["le"]), Op::Percentile(50.0));
    assert!(by_bound.contains("a percentile ranks a timer's buckets together"), "{by_bound}");
    let estimated = refused("api_ms", Query::estimate, Op::Avg);
    assert!(estimated.contains("an estimate of avg: only a percentile has one"), "{estimated}");
    let answer = f.metrics().query().name("rpc_ms").since(HOUR).estimate().aggregate(Op::Percentile(50.0)).unwrap();
    let estimate = Value::Between { lower: 0.0, upper: 10.0, estimate: Some(5.0) };
    assert_eq!(answered(&answer), [(estimate, 4)], "a scrape's histogram reads as a timer");
    // the buckets of a histogram read by their own name are a counter each
    let heat = f.metrics().query().name("rpc_ms_bucket").since(HOUR).by(&["le"]).aggregate(Op::Increase).unwrap();
    assert_eq!(heat.len(), 2);
}

/// A histogram as a scrape brings it, a sample every 15 seconds from `first`
/// for `count` of them, each adding 1 below 10, 2 above 10 and at most 20, 1
/// above 20 and at most 50, and 1 above 100.
fn scraped(first: SystemTime, count: u32) -> Vec<Series> {
    let at = |index: u32| first + 15 * SECOND * index;
    let series = |series: Series, step: f64| {
        let mut series = series;
        series.extend((0..count).map(|index| (at(index), f64::from(index) * step)));
        series
    };
    let mut scraped = vec![series(Series::counter("rpc_ms_count"), 5.0), series(Series::counter("rpc_ms_sum"), 300.0)];
    for (le, step) in [("10", 1.0), ("20", 3.0), ("50", 4.0), ("100", 4.0), ("+Inf", 5.0)] {
        scraped.push(series(Series::histogram("rpc_ms_bucket").label("le", le), step));
    }
    scraped
}

// contract: MET-08
#[test]
fn a_timers_percentile_lies_in_the_bucket_that_holds_its_rank_and_whole_blocks_answer_from_summaries() {
    let f = fixture();
    let now = f.now();
    // three hours, and the sample before them a first step begins at
    f.metrics().ingest(&scraped(now - 3 * HOUR - 15 * SECOND, 721)).unwrap();
    let hours = || f.metrics().query().name("rpc_ms").from(now - 3 * HOUR).to(now).every(HOUR);
    let ask = |percent: f64| answered(&hours().estimate().aggregate(Op::Percentile(percent)).unwrap());
    let ranked = |(lower, upper, estimate): (f64, f64, Option<f64>)| (Value::Between { lower, upper, estimate }, 1200);
    let from_the_head = [ask(50.0), ask(80.0), ask(95.0)];
    // each hour 1200 values: 240 at most 10, 720 at most 20, 960 at most 50 and 100
    assert_eq!(from_the_head[0], [ranked((10.0, 20.0, Some(17.5))); 3], "the 600th lies above 10 and at most 20");
    assert_eq!(from_the_head[1], [ranked((20.0, 50.0, Some(50.0))); 3], "the 960th, the last at most 50");
    assert_eq!(from_the_head[2], [ranked((100.0, f64::INFINITY, Some(100.0))); 3], "past the last bound");
    let before = hours().explain(Op::Percentile(95.0)).unwrap();
    assert_eq!(maintain(&f.store).unwrap().blocks, 3 * 7, "the hour before and two whole hours, each series");
    let after = hours().explain(Op::Percentile(95.0)).unwrap();
    assert_eq!(after.decoded, 6 * 240, "only the hour not packed yet decodes, of the count and five bounds");
    assert!(before.decoded > after.decoded, "{before:?}");
    assert_eq!([ask(50.0), ask(80.0), ask(95.0)], from_the_head, "the same from blocks' summaries");
}

#[test]
fn a_percentile_uses_the_bounds_two_sets_share_where_they_meet() {
    let f = fixture();
    let stats = f.metrics();
    let start = f.now();
    let old = stats.histogram("job_ms").buckets(&[10.0, 20.0, 50.0, 100.0]).open().unwrap();
    [5.0, 15.0, 60.0].into_iter().for_each(|value| old.record(value));
    stats.flush().unwrap();
    // a restart with other bounds: what both share is +Inf alone
    f.pass(30 * SECOND);
    let f = f.reopen();
    let stats = f.metrics();
    let new = stats.histogram("job_ms").buckets(&[5.0, 25.0, 75.0, 150.0]).open().unwrap();
    f.pass(2 * MINUTE);
    [1.0, 30.0].into_iter().for_each(|value| new.record(value));
    stats.flush().unwrap();
    let range = |every: Duration| f.metrics().query().name("job_ms").from(start).to(start + 3 * MINUTE).every(every);
    let p50 = |every| answered(&range(every).aggregate(Op::Percentile(50.0)).unwrap());
    assert_eq!(p50(HOUR), [(between(0.0, f64::INFINITY), 5)], "the hour of the change: neither set's bounds");
    let estimated = answered(&range(HOUR).estimate().aggregate(Op::Percentile(50.0)).unwrap());
    assert_eq!(estimated[0].0, between(0.0, f64::INFINITY), "no bound to estimate toward");
    // a minute of one set keeps its bounds
    assert_eq!(p50(MINUTE), [(between(0.0, f64::INFINITY), 3), (between(0.0, 5.0), 2)]);
    // two processes with other bounds: the bounds both have
    let now = f.now();
    let bucket = |instance: &str, le: &str, count: f64| {
        Series::histogram("rpc_ms_bucket")
            .label("instance", instance)
            .label("le", le)
            .sample(now - MINUTE, 0.0)
            .sample(now, count)
    };
    let counted = |instance: &str, count: f64| {
        Series::counter("rpc_ms_count").label("instance", instance).sample(now - MINUTE, 0.0).sample(now, count)
    };
    let written = [
        counted("a", 10.0),
        bucket("a", "10", 4.0),
        bucket("a", "20", 8.0),
        bucket("a", "+Inf", 10.0),
        counted("b", 10.0),
        bucket("b", "20", 2.0),
        bucket("b", "40", 9.0),
        bucket("b", "+Inf", 10.0),
    ];
    f.metrics().ingest(&written).unwrap();
    let rank =
        |percent| answered(&f.metrics().query().name("rpc_ms").since(HOUR).aggregate(Op::Percentile(percent)).unwrap());
    assert_eq!(rank(25.0), [(between(0.0, 20.0), 20)], "10 is a's alone");
    assert_eq!(rank(75.0), [(between(20.0, f64::INFINITY), 20)], "and 40 b's");
    // a process that measured nothing there narrows no other's bounds
    let idle = [counted("c", 0.0), bucket("c", "30", 0.0), bucket("c", "+Inf", 0.0)];
    f.metrics().ingest(&idle).unwrap();
    assert_eq!(rank(25.0), [(between(0.0, 20.0), 20)]);
    let apart = f.metrics().query().name("rpc_ms").since(HOUR).without(&[]).aggregate(Op::Percentile(25.0)).unwrap();
    let alone: Vec<Value> = apart.iter().map(|group| group.buckets[0].value).collect();
    assert_eq!(alone, [between(0.0, 10.0), between(20.0, 40.0)], "each process alone keeps its own");
}

#[test]
fn a_rank_past_the_last_bound_has_no_upper_bound() {
    let f = fixture();
    let stats = f.metrics();
    let latency = stats.timer("slow_ms").buckets(&[10.0]).open().unwrap();
    [3, 5, 50, 60].into_iter().for_each(|taken| latency.record(ms(taken)));
    stats.flush().unwrap();
    let ask = |percent| {
        let answer = f.metrics().query().name("slow_ms").since(HOUR).estimate().aggregate(Op::Percentile(percent));
        answered(&answer.unwrap())[0].0
    };
    // past the last bound the estimate is that bound, as Prometheus' histogram_quantile gives it
    assert_eq!(ask(95.0), Value::Between { lower: 10.0, upper: f64::INFINITY, estimate: Some(10.0) });
    assert_eq!(ask(25.0), Value::Between { lower: 0.0, upper: 10.0, estimate: Some(5.0) });
}
