//! An aggregate of series a program holds answers what a query of the same
//! samples in a store answers, bucket for bucket, and refuses what a query
//! would never be given.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::metrics::Value;
use crate::{ErrorKind, Options, Store, TestClock};

const MINUTE: Duration = Duration::from_mins(1);
const HOUR: Duration = Duration::from_hours(1);

/// A store whose clock is this hour's start, which the samples lie behind:
/// a held aggregate reads the system's clock, and both clocks are past every
/// bucket the tests ask for.
fn store() -> (tempfile::TempDir, Store, SystemTime) {
    let dir = tempfile::tempdir().unwrap();
    let hour = unix_millis(SystemTime::now()) / 3_600_000 * 3_600;
    let now = UNIX_EPOCH + Duration::from_secs(hour as u64);
    let clock = std::sync::Arc::new(TestClock::new(now));
    let store = Store::open(dir.path(), Options { clock: Some(clock), background: false, ..Options::default() });
    (dir, store.unwrap(), now)
}

fn run(mut series: Series, first: SystemTime, step: Duration, values: impl IntoIterator<Item = f64>) -> Series {
    series.extend(values.into_iter().zip(0_u32..).map(|(value, index)| (first + step * index, value)));
    series
}

/// The groups a test asks of, by name: a gauge of tenths, two counters each
/// reset once, and a histogram's buckets.
fn series(start: SystemTime) -> Vec<(&'static str, Vec<Series>)> {
    let step = 7 * Duration::from_secs(1);
    let tenths = (0..1500).map(|n| 0.1 * f64::from(n % 17));
    let counted = |from: u32| (0..1500).map(move |n| f64::from(if n < 900 { from + n * 3 } else { n - 900 }));
    let bucket = |le: &str, per: u32| {
        let values = (0..1500).map(move |n| f64::from(n * per / 10));
        run(Series::histogram("latency_bucket").label("le", le), start, step, values)
    };
    vec![
        ("reading", vec![run(Series::gauge("reading").label("room", "hall"), start, step, tenths)]),
        (
            "requests",
            vec![
                run(Series::counter("requests").label("host", "a"), start, step, counted(100)),
                run(Series::counter("requests").label("host", "b"), start + step / 2, step, counted(7)),
            ],
        ),
        ("latency_bucket", vec![bucket("1", 4), bucket("5", 9), bucket("+Inf", 10)]),
    ]
}

// contract: MET-18
#[test]
fn an_aggregate_of_held_series_answers_what_a_query_of_the_same_samples_answers() {
    let (_dir, store, now) = store();
    let start = now - 3 * HOUR;
    let groups = series(start);
    let stats = store.metrics().unwrap();
    stats.ingest(&groups.iter().flat_map(|(_, members)| members.clone()).collect::<Vec<_>>()).unwrap();
    // blocks a query answers whole from their summaries, beside the head
    assert!(crate::metrics::maintain(&store).unwrap().blocks > 0);
    let ranges = [(start + 90 * MINUTE + Duration::from_secs(13), now - 10 * MINUTE), (start - HOUR, now)];
    let ops = |name: &str| match name {
        "reading" => vec![Op::Count, Op::Sum, Op::Avg, Op::Min, Op::Max, Op::Delta, Op::First, Op::Last],
        "requests" => vec![Op::Count, Op::Increase, Op::Rate, Op::First, Op::Last],
        _ => vec![Op::Percentile(95.0), Op::Percentile(50.0)],
    };
    for (name, members) in &groups {
        for op in ops(name).into_iter().chain((*name == "reading").then_some(Op::Percentile(90.0))) {
            for (from, to) in ranges {
                for buckets in 0..4 {
                    let ranks = matches!(op, Op::Percentile(_));
                    let queried = stats.query().name(*name).from(from).to(to);
                    let queried = if ranks { queried.estimate() } else { queried };
                    let queried = match buckets {
                        0 => queried,
                        1 => queried.every(MINUTE),
                        2 => queried.every(HOUR),
                        _ => queried.calendar(Calendar::Day, "Asia/Kolkata"),
                    };
                    let held = super::query(members).from(from).to(to);
                    let held = if ranks { held.estimate() } else { held };
                    let held = match buckets {
                        0 => held,
                        1 => held.every(MINUTE),
                        2 => held.every(HOUR),
                        _ => held.calendar(Calendar::Day, "Asia/Kolkata"),
                    };
                    let (queried, held) = (queried.aggregate(op), held.aggregate(op));
                    let case = format!("{name}, {op}, buckets {buckets}, from {from:?}");
                    let queried = queried.unwrap_or_else(|error| panic!("{case}: {error}")).groups;
                    let held = held.unwrap_or_else(|error| panic!("{case}: {error}"));
                    assert_eq!(queried.len(), 1, "{case}");
                    assert!(!held.buckets.is_empty(), "{case}");
                    assert_eq!(held.buckets, queried[0].buckets, "{case}");
                }
            }
        }
    }
}

#[test]
fn a_sum_and_an_increase_are_exact_where_a_sum_by_hand_is_not() {
    let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let gauge = run(Series::gauge("reading"), start, 5 * Duration::from_secs(1), [0.1; 10]);
    let counter =
        run(Series::counter("requests"), start, 10 * Duration::from_secs(1), [100.0, 150.0, 200.0, 20.0, 70.0]);
    assert_eq!(gauge.samples().iter().map(|sample| sample.value).sum::<f64>(), 0.9999999999999999, "by hand");

    let sum = gauge.query().from(start).to(at(60)).aggregate(Op::Sum).unwrap();
    assert_eq!((sum.name.as_str(), sum.buckets[0].value, sum.buckets[0].samples), ("reading", Value::Exact(1.0), 10));
    let increase = counter.query().from(start).to(at(60)).aggregate(Op::Increase).unwrap();
    assert_eq!((increase.buckets[0].value, increase.buckets[0].resets), (Value::Exact(170.0), 1));
    let rate = counter.query().from(start).to(at(60)).aggregate(Op::Rate).unwrap();
    assert_eq!(rate.buckets[0].value, Value::Exact(170.0 / 60.0), "over the bucket's own seconds");
    assert!(!rate.buckets[0].partial, "a range in the past, which it spans whole");
}

#[test]
fn without_a_range_the_samples_are_the_range_and_their_order_is_their_times() {
    let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let shuffled = Series::counter("requests").sample(at(20), 9.0).sample(at(0), 1.0).sample(at(10), 4.0);
    let increase = shuffled.query().aggregate(Op::Increase).unwrap();
    assert_eq!((increase.buckets[0].at, increase.buckets[0].value), (at(0), Value::Exact(8.0)));
    let again = shuffled.clone().sample(at(10), 4.0);
    assert_eq!(again.query().aggregate(Op::Count).unwrap().buckets[0].value, Value::Exact(3.0), "a sample sent again");
}

#[test]
fn a_group_shares_a_name_and_its_labels_are_those_every_series_has() {
    let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let web = |host: &str| Series::gauge("cpu").label("host", host).label("zone", "eu").sample(at, 0.5);
    let group = super::query(&[web("a"), web("b")]).aggregate(Op::Sum).unwrap();
    assert_eq!((group.labels.len(), group.labels["zone"].as_str()), (1, "eu"));
    assert_eq!(group.buckets[0].value, Value::Exact(1.0));

    let mixed = [web("a"), Series::gauge("memory").sample(at, 1.0)];
    assert_eq!(super::query(&mixed).aggregate(Op::Sum).unwrap_err().kind(), ErrorKind::Invalid);
    let counters = [Series::counter("cpu").sample(at, 1.0)];
    assert_eq!(super::query(&counters).aggregate(Op::Sum).unwrap_err().kind(), ErrorKind::Invalid, "sum of a counter");
}

#[test]
fn what_a_query_is_never_given_is_refused() {
    let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let twice = Series::gauge("cpu").sample(at, 0.5).sample(at + Duration::from_micros(10), 0.6);
    let error = twice.query().aggregate(Op::Sum).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("one millisecond"), "{error}");
    let nan = Series::gauge("cpu").sample(at, f64::NAN);
    assert_eq!(nan.query().aggregate(Op::Sum).unwrap_err().kind(), ErrorKind::Invalid);
    let one = Series::gauge("cpu").sample(at, 1.0);
    let inverted = one.query().from(at).to(at - Duration::from_secs(1)).aggregate(Op::Sum);
    assert_eq!(inverted.unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(super::query(&[]).aggregate(Op::Sum).unwrap_err().kind(), ErrorKind::Invalid, "no series");
}
