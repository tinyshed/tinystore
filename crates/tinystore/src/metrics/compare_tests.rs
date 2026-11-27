use std::time::{Duration, SystemTime};

use super::*;
use crate::ErrorKind;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Series, maintain};

const WINDOW: Duration = Duration::from_mins(15);
const WEEK: Duration = Duration::from_hours(7 * 24);

/// A gauge of the call's time at `server`, a sample a minute from `first`.
fn calls(first: SystemTime, values: impl IntoIterator<Item = f64>) -> Series {
    let mut series = Series::gauge("call_ms").label("source", "server");
    series.extend(values.into_iter().zip(0_u32..).map(|(value, minute)| (first + MINUTE * minute, value)));
    series
}

fn compare(f: &Fixture, op: Op, window: Duration) -> Compare {
    f.metrics().query().name("call_ms").filter("source", "server").compare(op, window)
}

// contract: MET-33, MET-06
#[test]
fn the_usual_is_the_median_of_the_same_hours_of_the_days_before() {
    let f = fixture();
    let now = f.now();
    let mut series = Vec::new();
    for day in 1..=7_u32 {
        // the same quarter hour of each day, 310 to 370, and a spike six hours later
        let at = now - DAY * day - WINDOW;
        series.push(calls(at, (0..15).map(|_| 300.0 + 10.0 * f64::from(day))));
        series.push(calls(at + 6 * HOUR, (0..15).map(|_| 10_000.0)));
    }
    series.push(calls(now - WINDOW, (0..15).map(|_| 840.0)));
    for one in &series {
        f.metrics().ingest(std::slice::from_ref(one)).unwrap();
    }
    for packed in [false, true] {
        if packed {
            assert!(maintain(&f.store).unwrap().blocks > 0, "the days before are packed");
        }
        let now = compare(&f, Op::Percentile(50.0), WINDOW).against(WEEK).same_hour().run().unwrap();
        let expected =
            Compared { value: Some(840.0), usual: Some(340.0), ratio: Some(840.0 / 340.0), samples: 15, windows: 7 };
        assert_eq!(now, expected, "packed: {packed}");
        let avg = compare(&f, Op::Avg, WINDOW).against(WEEK).same_hour().run().unwrap();
        assert_eq!((avg.value, avg.usual), (Some(840.0), Some(340.0)), "a whole block answers from its summary");
    }
}

// contract: MET-33
#[test]
fn without_same_hour_every_window_before_counts_its_bounds_exact_and_an_empty_one_left_out() {
    let f = fixture();
    let now = f.now();
    let ingested = [
        // the newest window begins at now − 15m, which is its own and not the one before's
        calls(now - WINDOW, [50.0]),
        calls(now - WINDOW - SECOND, [100.0]),
        // [now − 30m, now − 45m) is empty; then [now − 60m, now − 45m)
        calls(now - 4 * WINDOW, [200.0]),
        // before what `against` reaches back to
        calls(now - 4 * WINDOW - SECOND, [10_000.0]),
    ];
    f.metrics().ingest(&ingested).unwrap();
    let now = compare(&f, Op::Avg, WINDOW).against(HOUR).run().unwrap();
    let expected =
        Compared { value: Some(50.0), usual: Some(150.0), ratio: Some(50.0 / 150.0), samples: 1, windows: 2 };
    assert_eq!(now, expected);
}

// contract: MET-33
#[test]
fn a_window_without_samples_is_absent_rather_than_zero() {
    let f = fixture();
    let now = f.now();
    f.metrics().ingest(&[calls(now - 2 * MINUTE, [5.0, 7.0])]).unwrap();
    let alone = compare(&f, Op::Max, WINDOW).against(WEEK).same_hour().run().unwrap();
    let expected = Compared { value: Some(7.0), usual: None, ratio: None, samples: 2, windows: 0 };
    assert_eq!(alone, expected, "no window before holds a sample");
    f.pass(HOUR);
    let quiet = compare(&f, Op::Max, WINDOW).against(2 * HOUR).run().unwrap();
    let expected = Compared { value: None, usual: Some(7.0), ratio: None, samples: 0, windows: 1 };
    assert_eq!(quiet, expected, "the newest window holds none");
    f.metrics().ingest(&[calls(f.now() - MINUTE, [0.0])]).unwrap();
    f.pass(WINDOW);
    f.metrics().ingest(&[calls(f.now() - MINUTE, [3.0])]).unwrap();
    let over_zero = compare(&f, Op::Max, WINDOW).against(2 * WINDOW).run().unwrap();
    let expected = Compared { value: Some(3.0), usual: Some(0.0), ratio: None, samples: 1, windows: 1 };
    assert_eq!(over_zero, expected, "no ratio over a usual of zero");
}

// contract: MET-33
#[test]
fn what_compare_cannot_answer_is_refused() {
    let f = fixture();
    f.metrics().ingest(&[calls(f.now() - MINUTE, [5.0])]).unwrap();
    let refused = |compare: Compare| {
        let error = compare.run().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        error.to_string()
    };
    let gauge = |op| compare(&f, op, MINUTE);
    assert!(gauge(Op::Avg).against(MINUTE * 1001).run().is_ok(), "1000 windows");
    let many = refused(gauge(Op::Avg).against(MINUTE * 1002));
    assert!(many.contains("1001 windows to compare with, 1000 at most: a wider window"), "{many}");
    assert!(refused(gauge(Op::Avg)).contains("`against` says how far back"));
    assert!(refused(gauge(Op::Avg).against(MINUTE)).contains("reaches back two windows at least"));
    assert!(refused(gauge(Op::Avg).against(HOUR).same_hour()).contains("reaches back a day at least"));
    assert!(refused(compare(&f, Op::Avg, 2 * DAY).against(WEEK).same_hour()).contains("a window of a day at most"));
    assert!(refused(gauge(Op::Increase).against(HOUR)).contains("increase of a gauge"));
    let ranged = f.metrics().query().name("call_ms").since(HOUR).compare(Op::Avg, MINUTE).against(HOUR);
    assert!(refused(ranged).contains("compare chooses its windows"));
    let unnamed = f.metrics().query().filter("source", "server").compare(Op::Avg, MINUTE).against(HOUR);
    assert!(refused(unnamed).contains("the query gives `name`"));
}

// contract: MET-33
#[test]
fn a_timers_percentile_compares_its_estimate_only_when_asked() {
    let f = fixture();
    let now = f.now();
    let bucket = |le: &str, at, count| Series::histogram("rpc_ms_bucket").label("le", le).sample(at, count);
    let counted = |name: &str, at, count| Series::counter(name).sample(at, count);
    let mut series = Vec::new();
    for (at, count) in [(now - DAY - 20 * MINUTE, 0.0), (now - DAY - 5 * MINUTE, 4.0), (now - 2 * MINUTE, 8.0)] {
        series.extend([bucket("10", at, count), bucket("+Inf", at, count)]);
        series.extend([counted("rpc_ms_count", at, count), counted("rpc_ms_sum", at, count)]);
    }
    f.metrics().ingest(&series).unwrap();
    let query = || f.metrics().query().name("rpc_ms");
    let error = query().compare(Op::Percentile(50.0), WINDOW).against(DAY).same_hour().run().unwrap_err();
    assert!(error.to_string().contains("lies between 0 and 10, no one number: ask `estimate`"), "{error}");
    let estimated = query().estimate().compare(Op::Percentile(50.0), WINDOW).against(DAY).same_hour().run().unwrap();
    assert_eq!((estimated.value, estimated.usual, estimated.windows), (Some(5.0), Some(5.0), 1), "{estimated:?}");
}

// contract: MET-33, MET-07
#[test]
fn every_bound_of_a_query_holds_over_all_its_windows() {
    let f = fixture();
    let at = f.now() - MINUTE;
    let many: Vec<Series> = (0..1001).map(|n| Series::gauge("call_ms").label("n", n).sample(at, 1.0)).collect();
    f.metrics().ingest(&many).unwrap();
    let error = f.metrics().query().name("call_ms").compare(Op::Avg, WINDOW).against(HOUR).run().unwrap_err();
    assert_eq!((error.kind(), error.fact("limit")), (ErrorKind::Limit, Some("series")), "{error}");
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_middle_two() {
    assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), Some(2.5));
    assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(&mut []), None);
}

#[test]
fn the_windows_are_the_same_hours_of_each_day_or_every_one_before() {
    let windows = Windows::of(WINDOW, Some(WEEK), true).unwrap();
    let (now, day, window) = (1_000_000_000_000, 86_400_000, 900_000);
    let ranges = windows.ranges(now);
    assert_eq!(ranges.len(), 8);
    assert_eq!(ranges[0], (now - window, now));
    assert_eq!(ranges[7], (now - 7 * day - window, now - 7 * day));
    let ranges = Windows::of(WINDOW, Some(HOUR), false).unwrap().ranges(now);
    assert_eq!(
        ranges,
        [
            (now - window, now),
            (now - 2 * window, now - window),
            (now - 3 * window, now - 2 * window),
            (now - 4 * window, now - 3 * window)
        ]
    );
}

/// The program's old way against `compare`, on one sample a second for a
/// week of ten series: one aggregate of the week by 15 minutes and one of
/// now, against one compare. Run with `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn compare_against_the_week_bucketed() {
    let f = fixture();
    let now = f.now();
    let seconds = 7 * 24 * 3600_u32;
    for chunk in (0..seconds).collect::<Vec<_>>().chunks(3600) {
        let series: Vec<Series> = (0..10)
            .map(|n| {
                let mut one = Series::gauge("call_ms").label("n", n);
                one.extend(chunk.iter().map(|s| (now - SECOND * (seconds - s), f64::from(s % 997))));
                one
            })
            .collect();
        f.metrics().ingest(&series).unwrap();
    }
    maintain(&f.store).unwrap();
    let passes = 7;
    let time = |run: &dyn Fn()| {
        let mut taken: Vec<Duration> = (0..passes)
            .map(|_| {
                let started = std::time::Instant::now();
                run();
                started.elapsed()
            })
            .collect();
        taken.sort();
        taken[passes / 2]
    };
    let query = || f.metrics().query().name("call_ms");
    let compared = time(&|| {
        query().compare(Op::Percentile(50.0), WINDOW).against(WEEK).same_hour().run().unwrap();
    });
    let bucketed = time(&|| {
        query().since(WEEK + WINDOW).every(WINDOW).aggregate(Op::Percentile(50.0)).unwrap();
        query().since(WINDOW).aggregate(Op::Percentile(50.0)).unwrap();
    });
    println!(
        "compare p50 15m against 7d same hour: {compared:?}; a week by 15m and now: {bucketed:?}; median of {passes}"
    );
}
