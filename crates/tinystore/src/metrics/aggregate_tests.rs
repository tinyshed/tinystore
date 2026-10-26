use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::labels::Kept;
use crate::metrics::{Query, Series, maintain};
use crate::{Calendar, ErrorKind};

/// `series` with a sample of each value, a `step` apart from `first`.
fn run(mut series: Series, first: SystemTime, step: Duration, values: impl IntoIterator<Item = f64>) -> Series {
    series.extend(values.into_iter().zip(0_u32..).map(|(value, index)| (first + step * index, value)));
    series
}

/// A bucket's number, which every `op` but a timer's percentile gives.
pub(super) fn number(bucket: &Bucket) -> f64 {
    bucket.value.exact().unwrap_or_else(|| panic!("a number, where {bucket:?}"))
}

/// The values of the one group an aggregate answered.
fn values(answer: &[Aggregate]) -> Vec<f64> {
    assert_eq!(answer.len(), 1, "{answer:?}");
    answer[0].buckets.iter().map(number).collect()
}

/// Each group as its name and labels, and its buckets' values.
fn groups(answer: &[Aggregate]) -> Vec<(String, Vec<f64>)> {
    let group = |group: &Aggregate| {
        let labels: Vec<String> = group.labels.iter().map(|(label, value)| format!("{label}={value}")).collect();
        let values = group.buckets.iter().map(number).collect();
        (format!("{}{{{}}}", group.name, labels.join(",")), values)
    };
    answer.iter().map(group).collect()
}

// contract: MET-04
#[test]
fn buckets_of_any_width_add_up_to_the_range() {
    let f = fixture();
    let (ten, eleven) = (f.now() - HOUR, f.now());
    // one that starts at ten and grows by one a second, and one already running
    let fresh = Series::counter("requests").label("run", "fresh");
    let running = Series::counter("requests").label("run", "running");
    let counted = [
        run(fresh.clone(), ten, 15 * SECOND, (0..240).map(|n| f64::from(n * 15))),
        run(running.clone(), ten - 15 * SECOND, 15 * SECOND, (0..241).map(|n| f64::from(1000 + n * 15))),
    ];
    f.metrics().ingest(&counted).unwrap();
    // a sample at eleven each, behind which packing takes the hour
    f.metrics().ingest(&[fresh.sample(eleven, 3600.0), running.sample(eleven, 4615.0)]).unwrap();
    for packed in [false, true] {
        if packed {
            assert_eq!(maintain(&f.store).unwrap().blocks, 3, "an hour each, and the sample before ten");
        }
        let query = |run: &str| f.metrics().query().name("requests").filter("run", run);
        let increase = |run: &str, every| {
            values(&query(run).from(ten).to(eleven).every(every).aggregate(Op::Increase).unwrap().groups)
        };
        assert_eq!(increase("fresh", HOUR), [3585.0], "packed: {packed}");
        let five = increase("fresh", 5 * MINUTE);
        assert_eq!((five[0], &five[1..]), (285.0, &[300.0; 11][..]));
        let one = increase("fresh", MINUTE);
        assert_eq!((one[0], one.len(), one.iter().sum::<f64>()), (45.0, 60, 3585.0));
        // the first bucket takes its step from the sample at 09:59:45
        assert_eq!(increase("running", HOUR), [3600.0], "packed: {packed}");
        assert_eq!(increase("running", 5 * MINUTE), [300.0; 12]);
        let cut = ten + 20 * MINUTE + 7 * SECOND;
        let before = values(&query("fresh").from(ten).to(cut).aggregate(Op::Increase).unwrap().groups);
        let after = values(&query("fresh").from(cut).to(eleven).aggregate(Op::Increase).unwrap().groups);
        assert_eq!(before[0] + after[0], 3585.0, "two adjacent ranges add up to the range they make");
    }
}

// contract: MET-04
#[test]
fn a_restart_counts_the_same_across_blocks() {
    // 2, then a run that began at 0 and reached 5: minutes apart in one block, days apart in three
    for (step, packed, blocks) in [(MINUTE, false, 0), (MINUTE, true, 1), (DAY, false, 0), (DAY, true, 3)] {
        let f = fixture();
        let first = f.now() - 3 * step;
        f.metrics().ingest(&[run(Series::counter("jobs_total"), first, step, [2.0, 0.0, 5.0, 5.0])]).unwrap();
        if packed {
            assert_eq!(maintain(&f.store).unwrap().blocks, blocks);
        }
        let grew = |from: SystemTime, to: SystemTime| {
            let answer =
                f.metrics().query().name("jobs_total").from(from).to(to).aggregate(Op::Increase).unwrap().groups;
            answer.first().map_or((0.0, 0), |group| (number(&group.buckets[0]), group.buckets[0].resets))
        };
        let end = f.now();
        assert_eq!(grew(first, end), (5.0, 1), "step {step:?}, packed: {packed}");
        // under every range that cuts them, the two parts add up
        for cut in [step / 2, step, step * 3 / 2, step * 2, step * 5 / 2] {
            let (before, after) = (grew(first, first + cut), grew(first + cut, end));
            assert_eq!((before.0 + after.0, before.1 + after.1), (5.0, 1), "cut {cut:?}, packed: {packed}");
        }
    }
}

// contract: MET-05
#[test]
fn only_a_step_across_an_empty_bucket_is_a_gap() {
    let f = fixture();
    let now = f.now();
    let dense = run(Series::counter("dense"), now - HOUR, 15 * SECOND, (0..240).map(f64::from));
    // hourly, but for the fifth hour of eight
    let mut hourly = Series::counter("hourly");
    hourly.extend((0..8_u32).filter(|hour| *hour != 4).map(|hour| (now - (8 - hour) * HOUR, f64::from(hour))));
    // 100 ten days ago, and 300 a minute ago; another that wrote 90 minutes ago
    let silent = Series::counter("silent").sample(now - 10 * DAY, 100.0).sample(now - MINUTE, 300.0);
    let recent = Series::counter("recent").sample(now - 90 * MINUTE, 100.0).sample(now - MINUTE, 300.0);
    f.metrics().ingest(&[dense, hourly, silent, recent]).unwrap();
    let gaps = |name: &str, since: Duration, every: Option<Duration>| -> Vec<bool> {
        let query = f.metrics().query().name(name).from(now - since).to(now);
        let answer = every.map_or(query.clone(), |every| query.every(every)).aggregate(Op::Increase).unwrap().groups;
        answer[0].buckets.iter().map(|bucket| bucket.gap).collect()
    };
    assert_eq!(gaps("dense", HOUR, Some(MINUTE)), [false; 60], "samples every 15 seconds never make one");
    assert_eq!(
        gaps("hourly", 8 * HOUR, Some(HOUR)),
        [false, false, false, false, true, false, false],
        "hourly samples in hourly buckets make one only where an hour is missing"
    );
    assert_eq!(
        gaps("hourly", 8 * HOUR, Some(15 * MINUTE)),
        [false, true, true, true, true, true, true],
        "an `every` finer than the samples marks every bucket a step enters"
    );
    let since = f.metrics().query().name("silent").since(HOUR).aggregate(Op::Increase).unwrap().groups;
    assert_eq!((number(&since[0].buckets[0]), since[0].buckets[0].gap), (200.0, true));
    assert_eq!(gaps("recent", HOUR, None), [false], "its step began in the hour before the range");
    let moved = f.metrics().query().name("silent").since(HOUR).aggregate(Op::Count).unwrap().groups;
    assert!(!moved[0].buckets[0].gap, "a count takes no step");
}

// contract: MET-04, MET-12
#[test]
fn a_bucket_retention_cut_is_partial_and_so_is_a_step_from_a_sample_it_took() {
    // in the head, where retention takes each sample; and packed, the day before in one block that goes whole
    for (packed, taken) in [(false, 6), (true, 0)] {
        let f = fixture();
        let start = f.now() - 20 * HOUR;
        // a name that keeps a day, written hourly from 12:00 the day before
        f.metrics().ingest(&[run(Series::counter("jobs").keep(DAY), start, HOUR, (0..21).map(f64::from))]).unwrap();
        if packed {
            assert_eq!(maintain(&f.store).unwrap().blocks, 1);
        }
        let ask = |name: &str, op: Op| -> Vec<(f64, bool)> {
            let query = f.metrics().query().name(name).from(start - DAY).to(f.now() + MINUTE).every(6 * HOUR);
            query.aggregate(op).unwrap().groups[0]
                .buckets
                .iter()
                .map(|bucket| (number(bucket), bucket.partial))
                .collect()
        };
        // six-hour buckets begin at 12:00 and 18:00; the name keeps from 08:00 the day before, and cuts none:
        // the last is the one the clock has not finished
        assert_eq!(ask("jobs", Op::Increase), [(5.0, false), (6.0, false), (6.0, false), (3.0, true)]);
        f.pass(5 * HOUR);
        // it keeps from 13:00 now: the bucket at 12:00 is cut, and 12:00 read no longer though still in the file
        assert_eq!(ask("jobs", Op::Increase)[..2], [(4.0, true), (6.0, false)], "packed: {packed}");
        assert_eq!(ask("jobs", Op::Count)[..2], [(5.0, true), (6.0, false)]);
        f.pass(5 * HOUR);
        assert_eq!(maintain(&f.store).unwrap().expired, taken, "18:00 is the oldest the name keeps");
        // no bucket is cut, but the step into 18:00 began at a sample the name keeps no longer
        assert_eq!(ask("jobs", Op::Increase)[..2], [(5.0, true), (6.0, false)], "packed: {packed}");
        assert_eq!(ask("jobs", Op::Count)[..2], [(6.0, false), (6.0, false)], "a count takes no step");
        // a series that begins there lost none
        let fresh = run(Series::counter("fresh").keep(DAY), f.now() - DAY, HOUR, (0..6).map(f64::from));
        f.metrics().ingest(&[fresh]).unwrap();
        assert_eq!(ask("fresh", Op::Increase), [(5.0, false)]);
    }
}

// contract: MET-05
#[test]
fn a_bucket_the_range_cut_or_the_clock_has_not_finished_is_partial() {
    let f = fixture();
    let (seven, eight) = (f.now() - HOUR, f.now());
    // 15 more every 15 seconds from seven, the last at the clock, eight
    let counted = run(Series::counter("requests"), seven, 15 * SECOND, (0..241).map(|n| f64::from(n * 15)));
    f.metrics().ingest(&[counted]).unwrap();
    let query = || f.metrics().query().name("requests");
    let buckets = |query: crate::metrics::Query, op: Op| -> Vec<(f64, bool)> {
        query.aggregate(op).unwrap().groups[0].buckets.iter().map(|bucket| (number(bucket), bucket.partial)).collect()
    };
    // the hour before the clock, whole, and the millisecond of the next that holds the sample taken now
    let hour = buckets(query().since(HOUR).every(5 * MINUTE), Op::Rate);
    assert_eq!(hour[..12], [(0.95, false), (1.0, false)].iter().copied().chain([(1.0, false); 10]).collect::<Vec<_>>());
    assert_eq!(hour[12], (15_000.0, true), "15 counted over a millisecond: no speed it ran at, and said so");
    // a range that cuts its first and its last bucket
    let cut = buckets(query().from(seven + 2 * MINUTE).to(eight - 2 * MINUTE).every(5 * MINUTE), Op::Count);
    assert_eq!((cut[0], cut[1], cut[10], cut[11]), ((12.0, true), (20.0, false), (20.0, false), (12.0, true)));
    // one that runs ahead of the clock: the hour at eight has only begun
    let ahead = buckets(query().from(seven).to(eight + HOUR).every(HOUR), Op::Increase);
    assert_eq!(ahead, [(3585.0, false), (15.0, true)]);
    // a range that is one bucket is whole, unless it runs ahead of the clock
    assert_eq!(buckets(query().since(HOUR), Op::Count), [(241.0, false)]);
    assert_eq!(buckets(query().from(seven).to(eight + HOUR), Op::Count), [(241.0, true)]);
    // at a bucket's last millisecond the clock has finished it
    f.pass(5 * MINUTE - Duration::from_millis(1));
    assert_eq!(buckets(query().since(5 * MINUTE).every(5 * MINUTE), Op::Count), [(1.0, false)]);
}

#[test]
fn a_group_is_the_series_of_a_name_that_share_the_labels_kept() {
    let f = fixture();
    let first = f.now() - 2 * MINUTE;
    let cpu =
        |host: &str, zone: &str| Series::gauge("cpu").label("env", "prod").label("host", host).label("zone", zone);
    let written = [
        run(cpu("a", "x"), first, MINUTE, [1.0, 2.0, 3.0]),
        run(cpu("b", "x"), first, MINUTE, [10.0, 20.0]),
        run(cpu("c", "y"), first, MINUTE, [100.0]),
        run(Series::gauge("mem").label("env", "prod").label("host", "a"), first, MINUTE, [7.0]),
    ];
    f.metrics().ingest(&written).unwrap();
    let hour = || f.metrics().query().filter("env", "prod").since(HOUR);
    // without `by`, every series of a name is one group, and never two names
    let sums = groups(&hour().aggregate(Op::Sum).unwrap().groups);
    assert_eq!(sums, [("cpu{}".to_owned(), vec![136.0]), ("mem{}".to_owned(), vec![7.0])]);
    let by_zone = groups(&hour().by(&["zone"]).aggregate(Op::Avg).unwrap().groups);
    // every sample's sum over every sample's count: 36 / 5, not the mean of 2 and 15
    assert_eq!(by_zone[0], ("cpu{zone=x}".to_owned(), vec![7.2]));
    assert_eq!(by_zone[1..], [("cpu{zone=y}".to_owned(), vec![100.0]), ("mem{}".to_owned(), vec![7.0])]);
    let x = || hour().name("cpu").filter("zone", "x");
    let of = |op| values(&x().aggregate(op).unwrap().groups)[0];
    assert_eq!((of(Op::Count), of(Op::Min), of(Op::Max), of(Op::Sum)), (5.0, 1.0, 20.0, 36.0));
    let without = groups(&hour().name("cpu").without(&["host"]).aggregate(Op::Count).unwrap().groups);
    assert_eq!(
        without,
        [("cpu{env=prod,zone=x}".to_owned(), vec![5.0]), ("cpu{env=prod,zone=y}".to_owned(), vec![1.0])]
    );
    let apart = hour().name("cpu").without(&[]).aggregate(Op::Count).unwrap().groups;
    assert_eq!(apart.len(), 3, "`without` nothing keeps every series apart");
    assert_eq!(apart[0].labels["host"], "a");
    // a minute a bucket: the group's buckets are the minutes any of its series has
    let minutes = groups(&x().every(MINUTE).aggregate(Op::Sum).unwrap().groups);
    assert_eq!(minutes, [("cpu{}".to_owned(), vec![11.0, 22.0, 3.0])]);
}

#[test]
fn last_adds_only_series_with_a_sample_in_the_bucket() {
    let f = fixture();
    let first = f.now() - 2 * MINUTE;
    let depth = |queue: &str| Series::gauge("queue_depth").label("queue", queue);
    // one queue stops writing after the second minute
    let written = [run(depth("mail"), first, MINUTE, [5.0, 6.0, 7.0]), run(depth("sync"), first, MINUTE, [10.0, 11.0])];
    f.metrics().ingest(&written).unwrap();
    let minutes =
        |op| values(&f.metrics().query().name("queue_depth").since(HOUR).every(MINUTE).aggregate(op).unwrap().groups);
    assert_eq!(minutes(Op::Last), [15.0, 17.0, 7.0], "the queues' total length at each minute's end");
    assert_eq!(minutes(Op::First), [15.0, 17.0, 7.0]);
    let hour = |op| values(&f.metrics().query().name("queue_depth").since(HOUR).aggregate(op).unwrap().groups);
    assert_eq!((hour(Op::First), hour(Op::Last)), (vec![15.0], vec![18.0]));
    // alone, a series' first and last keep their bits
    f.metrics().ingest(&[Series::gauge("zero").sample(first, -0.0).sample(first + MINUTE, 1.5)]).unwrap();
    let first_of = f.metrics().query().name("zero").since(HOUR).aggregate(Op::First).unwrap().groups;
    assert_eq!(number(&first_of[0].buckets[0]).to_bits(), (-0.0_f64).to_bits());
}

// contract: MET-04, MET-06
#[test]
fn a_block_inside_a_bucket_answers_as_its_samples_would() {
    let gauge_ops = [Op::Count, Op::Sum, Op::Avg, Op::Min, Op::Max, Op::Delta, Op::First, Op::Last];
    let counter_ops = [Op::Count, Op::Increase, Op::Rate, Op::First, Op::Last];
    let f = fixture();
    let now = f.now();
    let wave = |index: usize| ((index * 37 % 101) as f64 - 50.0) * 0.1;
    // a counter that starts again every 100 samples
    let total = |index: usize| (index % 100) as f64 * 0.7;
    let mut counter = Series::counter("total");
    counter.extend(f.gauge("total", 961, 15 * SECOND, total).samples().iter().copied());
    f.metrics().ingest(&[f.gauge("wave", 961, 15 * SECOND, wave), counter]).unwrap();
    let ranges = [(4 * HOUR, Duration::ZERO), (3 * HOUR + 10 * MINUTE, 20 * MINUTE), (2 * HOUR, HOUR)];
    let spans = [None, Some(HOUR), Some(30 * MINUTE), Some(7 * MINUTE)];
    let answers = |f: &Fixture| {
        let mut answers = Vec::new();
        for (name, ops) in [("wave", &gauge_ops[..]), ("total", &counter_ops[..])] {
            for (from, to) in ranges {
                for every in spans {
                    let query = f.metrics().query().name(name).from(now - from).to(now - to);
                    let query = every.map_or(query.clone(), |every| query.every(every));
                    answers.extend(ops.iter().map(|op| (name, from, every, *op, query.aggregate(*op).unwrap().groups)));
                }
            }
        }
        answers
    };
    let from_the_head = answers(&f);
    assert_eq!(maintain(&f.store).unwrap().blocks, 8, "four hours of each");
    let from_blocks = answers(&f);
    for (head, blocks) in from_the_head.iter().zip(&from_blocks) {
        let bits = |answer: &[Aggregate]| -> Vec<(u64, u64, u64)> {
            let buckets = answer.iter().flat_map(|group| &group.buckets);
            buckets.map(|bucket| (number(bucket).to_bits(), bucket.samples, bucket.resets)).collect()
        };
        assert_eq!(bits(&head.4), bits(&blocks.4), "{:?}", (head.0, head.1, head.2, head.3));
        assert_eq!(head.4, blocks.4);
    }
    // and as a sum over what a read gives is: the first whole hour of the wave
    let read = f.metrics().query().name("wave").from(now - 4 * HOUR).to(now - 3 * HOUR).read().unwrap().series;
    let mut exact = crate::metrics::exact::Exact::ZERO;
    read[0].samples().iter().for_each(|sample| assert!(exact.add_float(sample.value)));
    let hour = &from_blocks.iter().find(|answer| answer.3 == Op::Sum && answer.2 == Some(HOUR)).unwrap().4;
    assert_eq!((number(&hour[0].buckets[0]), hour[0].buckets[0].samples), (exact.round(), 240));
}

#[test]
fn a_rate_is_a_second_of_what_the_range_leaves_of_a_bucket_and_a_bucket_begins_on_the_grid() {
    let f = fixture();
    let ten = f.now() - HOUR;
    let counted = run(Series::counter("requests"), ten, 15 * SECOND, (0..240).map(|n| f64::from(n * 15)));
    f.metrics().ingest(&[counted]).unwrap();
    let query = || f.metrics().query().name("requests");
    let whole =
        query().from(ten + 5 * MINUTE).to(ten + 10 * MINUTE).every(5 * MINUTE).aggregate(Op::Rate).unwrap().groups;
    assert_eq!(values(&whole), [1.0], "300 in 300 seconds");
    // three minutes left of the bucket: 180 counted over 180 seconds
    let cut =
        query().from(ten + 7 * MINUTE).to(ten + 12 * MINUTE).every(5 * MINUTE).aggregate(Op::Rate).unwrap().groups;
    assert_eq!(values(&cut), [1.0, 1.0]);
    let begins: Vec<SystemTime> = cut[0].buckets.iter().map(|bucket| bucket.at).collect();
    assert_eq!(begins, [ten + 5 * MINUTE, ten + 10 * MINUTE], "where the bucket begins, not where the range cut it");
    let one = query().from(ten + 7 * MINUTE).to(ten + 12 * MINUTE).aggregate(Op::Count).unwrap().groups;
    assert_eq!((one[0].buckets[0].at, number(&one[0].buckets[0])), (ten + 7 * MINUTE, 20.0), "one bucket, the range");
    let all = query().aggregate(Op::Count).unwrap().groups;
    assert_eq!(
        (all[0].buckets[0].at, number(&all[0].buckets[0])),
        (f.now() - 30 * DAY, 240.0),
        "from where the name keeps"
    );
    let empty = query().from(ten - HOUR).to(ten).every(MINUTE).aggregate(Op::Increase).unwrap().groups;
    assert!(empty.is_empty(), "a group without a sample in the range is left out");
}

#[test]
fn an_op_a_kind_has_not_and_a_value_no_aggregate_takes_are_invalid() {
    let f = fixture();
    let now = f.now();
    let written = [
        Series::gauge("temp").label("room", "attic").sample(now - 2 * MINUTE, 20.5).sample(now - MINUTE, f64::NAN),
        Series::gauge("level").sample(now - MINUTE, f64::INFINITY),
        Series::counter("hits").sample(now - 2 * MINUTE, 3.0).sample(now - MINUTE, -1.0),
        run(Series::gauge("drift"), now - 3 * MINUTE, MINUTE, [f64::NAN, 1.0, 4.0]),
    ];
    f.metrics().ingest(&written).unwrap();
    let refused = |name: &str, since: Duration, op: Op| {
        let error = f.metrics().query().name(name).since(since).aggregate(op).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        error.to_string()
    };
    let kind = refused("temp", HOUR, Op::Increase);
    assert!(kind.contains(r#"metrics: temp{room="attic"}: increase of a gauge"#), "{kind}");
    assert!(refused("hits", HOUR, Op::Sum).contains("sum of a counter"));
    assert!(refused("hits", HOUR, Op::Delta).contains("delta of a counter"));
    let nan = refused("temp", HOUR, Op::Avg);
    assert!(nan.contains(r#"temp{room="attic"}: sample at 2027-01-15T07:59:00Z: an aggregate over a NaN"#), "{nan}");
    assert!(refused("level", HOUR, Op::Count).contains("an infinity"));
    assert!(refused("hits", HOUR, Op::Increase).contains("a counter below zero"));
    // a range that does not hold the value answers
    let before = f.metrics().query().name("temp").to(now - MINUTE).aggregate(Op::Max).unwrap().groups;
    assert_eq!(values(&before), [20.5]);
    // of the samples before a range, only the one its first step begins at is taken
    let drift = |from: Duration| f.metrics().query().name("drift").from(now - from).aggregate(Op::Delta);
    assert_eq!(values(&drift(90 * SECOND).unwrap().groups), [3.0], "from 1 to 4, whatever came before 1");
    let from_nan = drift(150 * SECOND).unwrap_err().to_string();
    assert!(from_nan.contains("drift: sample at 2027-01-15T07:57:00Z: an aggregate over a NaN"), "{from_nan}");
    let shape = |query: crate::metrics::Query| query.name("temp").since(HOUR).aggregate(Op::Max).unwrap_err().kind();
    assert_eq!(shape(f.metrics().query().every(Duration::ZERO)), ErrorKind::Invalid);
    assert_eq!(shape(f.metrics().query().every(Duration::from_micros(1500))), ErrorKind::Invalid);
    assert_eq!(shape(f.metrics().query().by(&["room"]).without(&["room"])), ErrorKind::Invalid);
    assert_eq!(shape(f.metrics().query().by(&["__name__"])), ErrorKind::Invalid);
    let read = f.metrics().query().name("temp").since(HOUR).every(MINUTE).read().unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "{read}");
    assert_eq!(f.metrics().query().name("temp").by(&["room"]).latest().unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn an_answer_past_its_buckets_is_a_limit() {
    let f = fixture();
    let first = f.now() - Duration::from_millis(100_000);
    // a sample a millisecond, 100,001 of them, packed as they come
    for batch in 0..11_u32 {
        let from = first + Duration::from_millis(u64::from(batch) * 10_000);
        let count = if batch == 10 { 1 } else { 10_000 };
        let samples = (0..count).map(|index| (from + Duration::from_millis(index), 1.0));
        let mut series = Series::gauge("fast");
        series.extend(samples);
        f.metrics().ingest(&[series]).unwrap();
        maintain(&f.store).unwrap();
    }
    let query = || f.metrics().query().name("fast").every(Duration::from_millis(1));
    let error = query().aggregate(Op::Count).unwrap_err();
    let facts = (error.kind(), error.fact("limit"), error.fact("wanted"), error.fact("bound"));
    assert_eq!(facts, (ErrorKind::Limit, Some("answer"), Some("100001"), Some("100000")));
    let fits = query().from(first + Duration::from_millis(1)).aggregate(Op::Count).unwrap().groups;
    assert_eq!(fits[0].buckets.len(), 100_000);
}

// contract: MET-08
#[test]
fn a_gauges_percentile_is_the_sample_at_its_nearest_rank_among_every_series_of_its_group() {
    let f = fixture();
    let now = f.now();
    // two hosts over the hour before now: a writes 0, 0.5, 1 …, b 1000, 1001 …
    let host = |host: &str, value: fn(usize) -> f64| f.gauge("cpu", 240, 15 * SECOND, value).label("host", host);
    f.metrics().ingest(&[host("a", |index| index as f64 * 0.5), host("b", |index| 1000.0 + index as f64)]).unwrap();
    let hour = || f.metrics().query().name("cpu").from(now - HOUR).to(now + SECOND);
    let rank = |percent| values(&hour().aggregate(Op::Percentile(percent)).unwrap().groups)[0];
    // of 480 samples the 240th is a's greatest, and the 241st b's least
    assert_eq!((rank(50.0), rank(50.1), rank(100.0), rank(0.001)), (119.5, 1000.0, 1239.0, 0.0));
    let by_host = groups(&hour().by(&["host"]).aggregate(Op::Percentile(50.0)).unwrap().groups);
    assert_eq!(by_host, [("cpu{host=a}".to_owned(), vec![59.5]), ("cpu{host=b}".to_owned(), vec![1119.0])]);
    let every = |percent| values(&hour().every(30 * MINUTE).aggregate(Op::Percentile(percent)).unwrap().groups);
    let from_the_head = (every(50.0), every(99.0));
    // a percentile reads every sample, never a summary, and so does its plan
    assert_eq!(maintain(&f.store).unwrap().blocks, 2);
    assert_eq!((every(50.0), every(99.0)), from_the_head);
    assert_eq!(hour().every(HOUR).explain(Op::Percentile(50.0)).unwrap().decoded, 480);
    let samples = hour().aggregate(Op::Percentile(50.0)).unwrap().groups[0].buckets[0].samples;
    assert_eq!(samples, 480);
    // a sample keeps its bits, -0 below +0
    f.metrics().ingest(&[Series::gauge("zero").sample(now - MINUTE, 0.0).sample(now - SECOND, -0.0)]).unwrap();
    let least = f.metrics().query().name("zero").since(HOUR).aggregate(Op::Percentile(50.0)).unwrap().groups;
    assert_eq!(number(&least[0].buckets[0]).to_bits(), (-0.0_f64).to_bits());
    let counter = f.metrics().ingest(&[Series::counter("hits").sample(now, 1.0)]);
    counter.unwrap();
    let error = f.metrics().query().name("hits").since(HOUR).aggregate(Op::Percentile(50.0)).unwrap_err();
    assert!(error.to_string().contains("p50 of a counter"), "{error}");
    for refused in [0.0, 100.5, f64::NAN, 1e-16] {
        let error = hour().aggregate(Op::Percentile(refused)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{refused}: {error}");
    }
    assert_eq!(Op::Percentile(99.9).to_string(), "p99.9");
}

// contract: MET-11
#[test]
fn an_overflowed_set_is_a_group_of_its_own_whatever_a_query_groups_by() {
    let kept = |pairs: &[(&str, &str)]| {
        Kept::read(pairs.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())).collect()).unwrap()
    };
    let routed = kept(&[("__name__", "hits"), ("route", "/notes"), ("status", "200")]);
    let overflowed = kept(&[("__name__", "hits"), ("__overflow", "true")]);
    let labels = |shape: &Shape, kept: &Kept| -> Vec<String> {
        let found = Found { id: 1, kind: Kind::Counter, kept: kept.clone() };
        shape.group_of(&found).unwrap().1.into_keys().collect()
    };
    let by = |by: Option<&[String]>, without: Option<&[String]>| {
        Shape::of(Op::Increase, Asking { by, without, ..Asking::default() }).unwrap()
    };
    let route = ["route".to_owned()];
    assert_eq!(labels(&by(None, None), &routed), Vec::<String>::new());
    assert_eq!(labels(&by(None, None), &overflowed), ["__overflow"]);
    assert_eq!(labels(&by(Some(&route), None), &routed), ["route"]);
    assert_eq!(labels(&by(Some(&route), None), &overflowed), ["__overflow"]);
    assert_eq!(labels(&by(None, Some(&route)), &routed), ["status"]);
    assert_eq!(labels(&by(None, Some(&[])), &routed), ["route", "status"]);
}

/// 2027-01-12 00:00 in Moscow, three days before the fixture's clock.
const MOSCOW_MIDNIGHT: u64 = 1_799_701_200;

// contract: MET-04, MET-05
#[test]
fn calendar_days_begin_at_local_midnight_and_add_up_packed_or_not() {
    let f = fixture();
    let midnight = UNIX_EPOCH + Duration::from_secs(MOSCOW_MIDNIGHT);
    // an order an hour for two Moscow days, as a shop in Moscow takes them
    f.metrics().ingest(&[run(Series::gauge("orders"), midnight, HOUR, [1.0; 48])]).unwrap();
    let days = |query: Query| {
        let answer = query.name("orders").from(midnight).to(midnight + 2 * DAY).aggregate(Op::Sum).unwrap().groups;
        answer[0].buckets.iter().map(|bucket| (bucket.at, number(bucket), bucket.partial)).collect::<Vec<_>>()
    };
    let local = vec![(midnight, 24.0, false), (midnight + DAY, 24.0, false)];
    assert_eq!(days(f.metrics().query().calendar(Calendar::Day, "Europe/Moscow")), local);
    // days of UTC cut at 03:00 in Moscow, as the issue saw them
    let utc: Vec<f64> = days(f.metrics().query().every(DAY)).iter().map(|(_, sum, _)| *sum).collect();
    assert_eq!(utc, [3.0, 24.0, 21.0]);
    // packed into blocks that cross a local midnight, the days are the same
    assert!(maintain(&f.store).unwrap().blocks > 0);
    assert_eq!(days(f.metrics().query().calendar(Calendar::Day, "Europe/Moscow")), local);
}

// contract: MET-04
#[test]
fn a_rate_over_a_calendar_day_counts_the_seconds_the_day_has() {
    let f = fixture();
    // Berlin's 2026-10-25 is 25 hours long; a counter that grows by one a second, a sample an hour
    // from an hour before the day before it, where the first day's first step begins
    let midnight = UNIX_EPOCH + Duration::from_secs(1_792_879_200);
    let counted = Series::counter("ticks_total").keep(Duration::from_hours(24 * 400));
    let hours = (0..1 + 24 + 25 + 24).map(|hour| f64::from(hour * 3600));
    let counted = run(counted, midnight - DAY - HOUR, HOUR, hours);
    f.metrics().ingest(&[counted]).unwrap();
    let rates = f
        .metrics()
        .query()
        .name("ticks_total")
        .from(midnight - DAY)
        .to(midnight + Duration::from_hours(49))
        .calendar(Calendar::Day, "Europe/Berlin")
        .aggregate(Op::Rate)
        .unwrap()
        .groups;
    let days: Vec<(SystemTime, f64)> = rates[0].buckets.iter().map(|bucket| (bucket.at, number(bucket))).collect();
    let hours = |count: u64| Duration::from_hours(count);
    assert_eq!(days, [(midnight - DAY, 1.0), (midnight, 1.0), (midnight + hours(25), 1.0)]);
}

#[test]
fn every_and_a_calendar_together_or_a_zone_no_one_has_are_refused() {
    let f = fixture();
    f.metrics().ingest(&[Series::gauge("orders").sample(f.now(), 1.0)]).unwrap();
    let query = || f.metrics().query().name("orders").since(DAY);
    let both = query().every(HOUR).calendar(Calendar::Day, "UTC").aggregate(Op::Sum).unwrap_err();
    assert!(both.to_string().contains("`every` or `calendar`, not both"), "{both}");
    let nowhere = query().calendar(Calendar::Week, "Mars/Olympus").aggregate(Op::Sum).unwrap_err();
    assert_eq!(nowhere.kind(), ErrorKind::Invalid, "{nowhere}");
    let read = query().calendar(Calendar::Day, "UTC").read().unwrap_err();
    assert!(read.to_string().contains("`calendar`"), "{read}");
}
