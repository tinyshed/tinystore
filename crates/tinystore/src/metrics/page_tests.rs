use std::time::Duration;

use super::{AggregatePage, Cursor, GroupPlace, Place, SeriesPage, Start};
use crate::metrics::budget::Budget;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Aggregate, Op, Query, Series, maintain};
use crate::{ErrorKind, unix_millis};

/// A series read back as its labels and each sample's time and bits.
type Bits = (String, Vec<(i64, u64)>);

fn bits(series: &Series) -> Bits {
    let samples = series.samples().iter().map(|sample| (unix_millis(sample.at), sample.value.to_bits())).collect();
    (format!("{series}"), samples)
}

/// Every page of `query` read `limit` samples at a time by `next`, each
/// checked to hold at most `limit` and to go on past the page before it.
fn pages(query: &Query, limit: usize) -> Vec<SeriesPage> {
    let paged = query.clone().limit(limit);
    let mut pages = vec![paged.read().unwrap()];
    let mut place = (i64::MIN, i64::MIN);
    while let Some(next) = pages.last().unwrap().next.clone() {
        let Start::Series(went_on) = Cursor::read(&next).unwrap().start else { panic!("{next} names no series") };
        assert!((went_on.series, went_on.at) > place, "{went_on:?} after {place:?}");
        place = (went_on.series, went_on.at);
        pages.push(paged.clone().after(&next).read().unwrap());
    }
    for page in &pages {
        assert!(page.series.iter().map(|series| series.samples().len()).sum::<usize>() <= limit);
    }
    pages
}

/// The series of pages joined: a part that says `more` goes on in the first
/// part of the page after it.
fn joined(pages: &[SeriesPage]) -> Vec<Bits> {
    let mut joined: Vec<Bits> = Vec::new();
    let mut more = false;
    for series in pages.iter().flat_map(|page| &page.series) {
        let (name, samples) = bits(series);
        match joined.last_mut() {
            Some(last) if more => {
                assert_eq!(last.0, name, "a part goes on with the series before it");
                last.1.extend(samples);
            }
            _ => joined.push((name, samples)),
        }
        more = series.more();
    }
    joined
}

/// Three hosts' cpu, each packed into hourly blocks with a head after them,
/// at the same ticks, and a fourth whose samples are all older than an hour.
fn written(f: &Fixture) {
    let stats = f.metrics();
    let gauge = |host: &str, count: usize, step: Duration| {
        f.gauge("cpu", count, step, |index| index as f64 * 0.5 - 7.0).label("host", host)
    };
    stats.ingest(&[gauge("a", 1000, 15 * SECOND), gauge("b", 37, MINUTE), gauge("c", 2401, 15 * SECOND)]).unwrap();
    let old = Series::gauge("cpu").label("host", "old").sample(f.now() - 3 * HOUR, 1.0);
    stats.ingest(&[old]).unwrap();
    maintain(&f.store).unwrap();
    f.pass(20 * MINUTE);
    stats.ingest(&[gauge("a", 80, 15 * SECOND), gauge("c", 80, 15 * SECOND)]).unwrap();
}

// contract: MET-17
#[test]
fn pages_followed_by_next_give_the_whole_read_once_and_in_order() {
    let f = fixture();
    written(&f);
    let query = f.metrics().query().name("cpu").since(10 * HOUR);
    let whole: Vec<Bits> = query.read().unwrap().series.iter().map(bits).collect();
    assert_eq!(
        whole.iter().map(|(_, samples)| samples.len()).sum::<usize>(),
        3519,
        "c's oldest 20 minutes left the range"
    );
    for limit in [1, 7, 240, 1000, 3518, 3519, 100_000] {
        let read = pages(&query, limit);
        assert_eq!(joined(&read), whole, "pages of {limit}");
        assert!(read.last().unwrap().series.iter().all(|series| !series.more()));
    }
    let from = f.now() - HOUR;
    let range = f.metrics().query().name("cpu").from(from).to(f.now());
    let within: Vec<Bits> = range.read().unwrap().series.iter().map(bits).collect();
    assert_eq!(joined(&pages(&range, 100)), within, "a range that cuts blocks and leaves a series out");
}

// contract: MET-17
#[test]
fn a_page_stops_before_the_samples_it_may_decode_rather_than_fail() {
    let f = fixture();
    let first = f.now() - SECOND * 1_099_999;
    for start in (0..1_100_000).step_by(10_000) {
        let times = (start..start + 10_000).map(|index| (first + SECOND * index as u32, index as f64));
        let mut series = Series::gauge("ping_rtt_ms").label("target", "a");
        series.extend(times);
        f.metrics().ingest(&[series]).unwrap();
        // a head holds 16,384 samples at most, so packing keeps up as it would
        maintain(&f.store).unwrap();
    }
    let query = f.metrics().query().name("ping_rtt_ms").since(30 * 24 * HOUR);
    let whole = query.read().unwrap_err();
    assert_eq!((whole.kind(), whole.fact("limit")), (ErrorKind::Limit, Some("samples decoded")));
    let (asked, paging) = query.clone().limit(10_000).paged(None).unwrap();
    let mut budget = Budget::default();
    let (taken, stopped) = f.engine().take_page(&asked, &paging.unwrap(), &mut budget).unwrap();
    assert!(budget.decoded <= 10_000 + 240, "a page takes its samples and one block: {budget:?}");
    assert!(budget.bytes < 1 << 20, "{budget:?}");
    assert_eq!((taken.len(), stopped.is_some()), (1, true));
    let mut read = 0;
    for series in query.clone().limit(100_000).read().unwrap().all() {
        let series = series.unwrap();
        assert_eq!(series.samples()[0].value, read as f64, "parts follow each other");
        read += series.samples().len();
    }
    assert_eq!(read, 1_100_000);
    let hourly = query.clone().every(HOUR);
    let median = Op::Percentile(50.0);
    assert_eq!(hourly.aggregate(median).unwrap_err().fact("limit"), Some("samples decoded"));
    let mut samples = 0;
    for group in hourly.clone().limit(100_000).aggregate(median).unwrap().all() {
        samples += group.unwrap().buckets.iter().map(|bucket| bucket.samples).sum::<u64>();
    }
    assert_eq!(samples, 1_100_000, "every hour's median, from pages that share the samples they decode");
    let one_bucket = query.clone().limit(10).aggregate(median).unwrap_err();
    assert_eq!(one_bucket.fact("limit"), Some("samples decoded"), "a bucket is never split");
    assert!(!one_bucket.to_string().contains("`limit`"), "a page is not offered pages: {one_bucket}");
}

// contract: MET-17
#[test]
fn a_page_goes_on_past_the_series_a_query_finds() {
    let f = fixture();
    let now = f.now();
    let series: Vec<Series> =
        (0..1001).map(|host| Series::gauge("up").label("host", host).sample(now, f64::from(host))).collect();
    f.metrics().ingest(&series).unwrap();
    let query = f.metrics().query().name("up").since(HOUR);
    assert_eq!(query.read().unwrap_err().fact("limit"), Some("series"));
    let read = pages(&query, 100_000);
    assert_eq!(read.iter().map(|page| page.series.len()).collect::<Vec<_>>(), [1000, 1]);
}

/// The `_bucket` series of a latency histogram of `functions` functions,
/// twelve bounds each, an hour of a sample a minute.
fn latencies(f: &Fixture, functions: usize) {
    let bounds = ["50", "100", "200", "400", "800", "1600", "3200", "6400", "12800", "25600", "51200", "+Inf"];
    let first = f.now() - 59 * MINUTE;
    for function in 0..functions {
        let series = bounds.iter().enumerate().map(|(index, le)| {
            let mut series = Series::histogram("rpc_ms_bucket").label("fn", format!("fn{function:03}")).label("le", le);
            let counted = |minute: u32| f64::from(minute * (index as u32 + 1) + function as u32 % 5);
            series.extend((0..60).map(|minute| (first + MINUTE * minute, counted(minute))));
            series
        });
        f.metrics().ingest(&series.collect::<Vec<_>>()).unwrap();
    }
}

// contract: MET-20
#[test]
fn an_aggregates_pages_go_past_the_series_a_query_finds_a_group_at_a_time() {
    let f = fixture();
    latencies(&f, 92);
    let query = f.metrics().query().name("rpc_ms_bucket").since(2 * HOUR).by(&["fn"]).estimate();
    let p95 = Op::Percentile(95.0);
    assert_eq!(query.aggregate(p95).unwrap_err().fact("limit"), Some("series"), "1104 series in one call");
    let alone = |query: &Query| -> Vec<Aggregate> {
        let each = (0..92).map(|function| query.clone().filter("fn", format!("fn{function:03}")).aggregate(p95));
        each.flat_map(|page| page.unwrap().groups).collect()
    };
    let whole = aggregate_pages(&query, p95, 100_000);
    let groups: Vec<usize> = whole.iter().map(|page| page.groups.len()).collect();
    assert_eq!(groups, [83, 9], "83 functions are 996 series, the most whole groups within 1000");
    assert_eq!(joined_groups(&whole), alone(&query), "each group's buckets, as a call of it alone gives them");
    for (query, limit) in [(query.clone(), 7), (query.clone().every(10 * MINUTE), 20)] {
        assert_eq!(joined_groups(&aggregate_pages(&query, p95, limit)), alone(&query), "pages of {limit}");
    }
}

// contract: MET-20
#[test]
fn a_group_past_the_series_a_query_finds_is_a_limit_and_the_groups_before_it_are_not() {
    let f = fixture();
    let now = f.now();
    let host = |zone: &str, host: usize| Series::gauge("up").label("zone", zone).label("host", host).sample(now, 1.0);
    let mut series: Vec<Series> = (0..1001).map(|index| host("b", index)).collect();
    series.extend((0..5).map(|index| host("a", index)));
    f.metrics().ingest(&series).unwrap();
    let zones = f.metrics().query().name("up").since(HOUR).by(&["zone"]).limit(100);
    let first = zones.aggregate(Op::Max).unwrap();
    assert_eq!(first.groups.iter().map(|group| group.labels["zone"].as_str()).collect::<Vec<_>>(), ["a"]);
    let too_many = zones.clone().after(&first.next.unwrap()).aggregate(Op::Max).unwrap_err();
    let refused = too_many.to_string();
    assert_eq!((too_many.kind(), too_many.fact("limit")), (ErrorKind::Limit, Some("series")));
    assert!(refused.contains("up{zone=\"b\"}") && refused.contains("a group past 1000 series"), "{refused}");
    assert!(!refused.contains("`limit`"), "a page is not offered pages: {refused}");
    let one = f.metrics().query().name("up").since(HOUR).limit(100).aggregate(Op::Max).unwrap_err();
    assert_eq!(one.fact("limit"), Some("series"), "a name's series are one group without `by`");
}

// contract: MET-17
#[test]
fn a_cursor_keeps_the_range_and_the_clock_the_first_page_read() {
    let f = fixture();
    f.metrics().ingest(&[f.gauge("cpu", 60, MINUTE, |index| index as f64)]).unwrap();
    let query = f.metrics().query().name("cpu").since(HOUR);
    let first = query.clone().limit(20).read().unwrap();
    let whole = query.read().unwrap().series;
    f.pass(30 * MINUTE);
    f.metrics().ingest(&[f.gauge("cpu", 1, MINUTE, |_| 99.0)]).unwrap();
    let mut read: Vec<Series> = first.series.clone();
    read.extend(first.all().map(Result::unwrap).skip(1));
    let samples: Vec<f64> = read.iter().flat_map(|series| series.samples()).map(|sample| sample.value).collect();
    let before: Vec<f64> = whole[0].samples().iter().map(|sample| sample.value).collect();
    assert_eq!(samples, before, "the first page's hour, none of the half hour after it");
}

#[test]
fn what_changes_between_pages_is_seen_ahead_of_the_cursor_and_not_behind_it() {
    let f = fixture();
    let host = |name: &str, step: Duration| f.gauge("cpu", 30, step, |index| index as f64).label("host", name);
    f.metrics().ingest(&[host("a", MINUTE), host("b", 2 * MINUTE), host("c", MINUTE)]).unwrap();
    let query = f.metrics().query().name("cpu").since(HOUR);
    let first = query.clone().limit(10).read().unwrap();
    assert_eq!((first.series.len(), first.series[0].more()), (1, true));
    let now = f.now();
    let late = |name: &str, at| Series::gauge("cpu").label("host", name).sample(at, -1.0);
    let made = Series::gauge("cpu").label("host", "d").sample(now - MINUTE, 5.0);
    f.metrics().ingest(&[late("a", now - 59 * MINUTE + SECOND), late("b", now - MINUTE), made]).unwrap();
    assert!(f.metrics().delete("cpu", [("host", "c")]).unwrap());
    let rest = query.clone().limit(10).after(&first.next.unwrap());
    let hosts: Vec<(String, usize, bool)> = rest
        .read()
        .unwrap()
        .all()
        .map(Result::unwrap)
        .map(|series| {
            (series.labels()["host"].clone(), series.samples().len(), series.samples().iter().any(|s| s.value < 0.0))
        })
        .collect();
    let late_b = hosts.iter().any(|(host, _, late)| host == "b" && *late);
    let late_a = hosts.iter().any(|(host, _, late)| host == "a" && *late);
    assert!(late_b && !late_a, "ahead of the cursor seen, behind it not: {hosts:?}");
    assert!(hosts.iter().all(|(host, ..)| host != "c"), "a series deleted between pages is gone: {hosts:?}");
    assert_eq!(hosts.last().map(|(host, ..)| host.as_str()), Some("d"), "one made comes after the others");
}

// contract: MET-17
#[test]
fn a_cursor_is_refused_with_another_query_and_after_or_limit_where_no_page_is() {
    let f = fixture();
    f.metrics()
        .ingest(&[f.gauge("cpu", 30, MINUTE, |index| index as f64), f.gauge("mem", 30, MINUTE, |index| index as f64)])
        .unwrap();
    let cpu = f.metrics().query().name("cpu").since(HOUR);
    let next = cpu.clone().limit(10).read().unwrap().next.unwrap();
    let invalid = |query: Query| query.read().unwrap_err().kind();
    let mem = f.metrics().query().name("mem").since(HOUR);
    assert_eq!(invalid(mem.limit(10).after(&next)), ErrorKind::Invalid, "another query");
    assert_eq!(invalid(cpu.clone().since(2 * HOUR).limit(10).after(&next)), ErrorKind::Invalid, "another range");
    assert_eq!(invalid(cpu.clone().after(&next)), ErrorKind::Invalid, "after without limit");
    assert_eq!(invalid(cpu.clone().limit(0)), ErrorKind::Invalid);
    assert_eq!(invalid(cpu.clone().limit(100_001)), ErrorKind::Invalid);
    assert_eq!(invalid(cpu.clone().limit(10).after("17:300")), ErrorKind::Invalid, "no page's next");
    let other = next.replacen('1', "2", 1);
    let error = cpu.clone().limit(10).after(&other).read().unwrap_err();
    assert!(error.to_string().contains("version 2"), "{error}");
    assert_eq!(cpu.clone().limit(10).latest().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(cpu.clone().limit(10).explain(None).unwrap_err().kind(), ErrorKind::Invalid);
    let aggregate = cpu.clone().after(&next).aggregate(crate::metrics::Op::Avg).unwrap_err();
    assert_eq!(aggregate.kind(), ErrorKind::Invalid);
}

#[test]
fn a_cursor_reads_back_as_it_was_written() {
    let labels = [("route".to_owned(), "/a~b".to_owned())].into();
    let starts = [
        Start::First,
        Start::Series(Place { series: 42, at: -1_800_000_000_000 }),
        Start::Group(GroupPlace { group: ("http_requests_total".to_owned(), labels), at: 7 }),
    ];
    for start in starts {
        let cursor = Cursor { query: "00ff".to_owned(), from: i64::MIN, to: 7, now: 6, start };
        assert_eq!(Cursor::read(&cursor.written()).unwrap(), cursor);
    }
}

/// Every page of an aggregate of `op` `limit` buckets at a time by `next`,
/// each checked to hold at most `limit` and to go on past the page before it.
fn aggregate_pages(query: &Query, op: Op, limit: usize) -> Vec<AggregatePage> {
    let paged = query.clone().limit(limit);
    let mut pages = vec![paged.aggregate(op).unwrap()];
    let mut place = None;
    while let Some(next) = pages.last().unwrap().next.clone() {
        let Start::Group(went_on) = Cursor::read(&next).unwrap().start else { panic!("{next} names no group") };
        let went_on = Some((went_on.group, went_on.at));
        assert!(went_on > place, "{went_on:?} after {place:?}");
        place = went_on;
        pages.push(paged.clone().after(&next).aggregate(op).unwrap());
    }
    let held: Vec<usize> = pages.iter().map(|page| page.groups.iter().map(|group| group.buckets.len()).sum()).collect();
    assert!(held.iter().all(|&buckets| buckets <= limit), "{held:?}");
    assert!(held[..held.len() - 1].iter().all(|&buckets| buckets > 0), "no page but the last is empty: {held:?}");
    pages
}

/// The groups of pages joined: a part that says `more` goes on in the first
/// part of the page after it.
fn joined_groups(pages: &[AggregatePage]) -> Vec<Aggregate> {
    let mut joined: Vec<Aggregate> = Vec::new();
    let mut more = false;
    for group in pages.iter().flat_map(|page| &page.groups) {
        match joined.last_mut() {
            Some(last) if more => {
                assert_eq!((&last.name, &last.labels), (&group.name, &group.labels), "a part goes on with its group");
                last.buckets.extend(group.buckets.iter().copied());
            }
            _ => joined.push(Aggregate { more: false, ..group.clone() }),
        }
        more = group.more;
    }
    joined
}

/// Requests of three routes from two hosts each, counted every 15 seconds
/// with a reset every 700 samples, and their latencies: packed into hourly
/// blocks, with a head after them.
fn traffic(f: &Fixture) {
    let series = |name: &str, route: &str, host: &str, from: usize, count: usize| {
        let first = f.now() - 15 * SECOND * (count as u32 - 1);
        let mut series = match name {
            "requests_total" => Series::counter(name),
            _ => Series::gauge(name),
        };
        series = series.label("route", route).label("host", host);
        let value = |index: usize| (index % 700) as f64 * 1.5 + route.len() as f64 + host.len() as f64;
        series.extend((0..count).map(|index| (first + 15 * SECOND * index as u32, value(from + index))));
        series
    };
    let all = |from, count| {
        let mut all = Vec::new();
        for route in ["/a", "/bb", "/ccc"] {
            for host in ["h1", "h22"] {
                all.push(series("requests_total", route, host, from, count));
                all.push(series("latency_ms", route, host, from, count));
            }
        }
        all
    };
    f.metrics().ingest(&all(0, 800)).unwrap();
    maintain(&f.store).unwrap();
    f.pass(20 * MINUTE);
    f.metrics().ingest(&all(800, 80)).unwrap();
}

// contract: MET-17
#[test]
fn aggregate_pages_followed_by_next_give_the_whole_aggregate_bucket_for_bucket() {
    let f = fixture();
    traffic(&f);
    let counters = f.metrics().query().name("requests_total").since(10 * HOUR);
    let gauges = f.metrics().query().name("latency_ms").since(10 * HOUR);
    let cases = [
        (counters.clone().every(MINUTE).by(&["route"]), Op::Rate),
        (counters.clone().every(7 * MINUTE), Op::Increase),
        (counters.clone().every(HOUR).without(&[]), Op::Count),
        (gauges.clone().every(MINUTE).by(&["route"]), Op::Avg),
        (gauges.clone().every(HOUR).without(&[]), Op::Delta),
        (gauges.clone().every(10 * MINUTE).by(&["host"]), Op::Percentile(95.0)),
        (gauges.clone().by(&["route"]), Op::Max),
    ];
    for (query, op) in cases {
        let whole = query.aggregate(op).unwrap().groups;
        assert!(!whole.is_empty() && whole.iter().all(|group| !group.more), "{op}");
        for limit in [1, 3, 17, 100_000] {
            assert_eq!(joined_groups(&aggregate_pages(&query, op, limit)), whole, "{op} in pages of {limit}");
        }
    }
}

// contract: MET-17
#[test]
fn aggregate_pages_of_a_calendar_go_across_a_change_of_the_clock() {
    let f = fixture();
    // from mid January to the first days of April, past Berlin's change to summer time
    f.pass(80 * DAY);
    f.metrics().ingest(&[f.gauge("temp", 240, HOUR, |index| index as f64 * 0.25)]).unwrap();
    maintain(&f.store).unwrap();
    let query = f.metrics().query().name("temp").since(11 * DAY).calendar(crate::Calendar::Day, "Europe/Berlin");
    let whole = query.aggregate(Op::Avg).unwrap().groups;
    assert_eq!(whole[0].buckets.len(), 11);
    for limit in [1, 2, 5] {
        assert_eq!(joined_groups(&aggregate_pages(&query, Op::Avg, limit)), whole, "days in pages of {limit}");
    }
}

/// Two hosts' requests and memory over the last ten hours, a sample a minute
/// with long stretches of none: a, an hour from nine hours ago and a quarter
/// from three, packed; b, the half hour before the last ten minutes, in its
/// head.
fn sparse(f: &Fixture) {
    let minutes = |name: &str, host: &str, ago: Duration, count: u32| {
        let first = f.now() - ago;
        let mut series = match name {
            "requests_total" => Series::counter(name),
            _ => Series::gauge(name),
        };
        series = series.label("host", host);
        series.extend((0..count).map(|minute| (first + MINUTE * minute, f64::from(minute * 3 + count % 7))));
        series
    };
    for name in ["requests_total", "memory"] {
        f.metrics().ingest(&[minutes(name, "a", 9 * HOUR, 60), minutes(name, "a", 3 * HOUR, 15)]).unwrap();
    }
    maintain(&f.store).unwrap();
    for name in ["requests_total", "memory"] {
        f.metrics().ingest(&[minutes(name, "b", 40 * MINUTE, 30)]).unwrap();
    }
}

// contract: MET-21
#[test]
fn a_stretch_without_samples_costs_a_page_nothing() {
    let f = fixture();
    sparse(&f);
    let cases = [
        (f.metrics().query().name("memory").by(&["host"]), Op::Max),
        (f.metrics().query().name("memory"), Op::Avg),
        (f.metrics().query().name("requests_total").by(&["host"]), Op::Increase),
        (f.metrics().query().name("requests_total").by(&["host"]), Op::Rate),
    ];
    for (query, op) in cases {
        let query = query.since(10 * HOUR).every(MINUTE);
        let whole = query.aggregate(op).unwrap().groups;
        let buckets: usize = whole.iter().map(|group| group.buckets.len()).sum();
        assert!(buckets >= 75, "{op}: every minute of samples is a bucket, of 600 in the range");
        for limit in [1, 7, 30, 1000] {
            let pages = aggregate_pages(&query, op, limit);
            assert_eq!(joined_groups(&pages), whole, "{op} in pages of {limit}");
            let held: Vec<usize> = pages.iter().map(|page| page.groups.iter().map(|g| g.buckets.len()).sum()).collect();
            let (last, full) = held.split_last().unwrap();
            assert!(full.iter().all(|&page| page == limit) && *last <= limit, "{op} in pages of {limit}: {held:?}");
            assert_eq!(pages.len(), buckets.div_ceil(limit), "{op} in pages of {limit}");
        }
    }
}

// contract: MET-21
#[test]
fn a_gap_inside_dense_samples_shortens_a_page_and_empties_none() {
    let f = fixture();
    // two hours of a sample every ten seconds, packed, but for five minutes an hour in, inside a block
    let first = f.now() - 2 * HOUR - 20 * MINUTE;
    let at = |index: u32| first + 10 * SECOND * index;
    let mut series = Series::gauge("cpu").label("host", "a");
    series.extend((0..720).filter(|index| !(360..390).contains(index)).map(|index| (at(index), f64::from(index))));
    f.metrics().ingest(&[series]).unwrap();
    maintain(&f.store).unwrap();
    f.pass(20 * MINUTE);
    let query = f.metrics().query().name("cpu").since(3 * HOUR).every(MINUTE);
    let whole = query.aggregate(Op::Avg).unwrap().groups;
    assert_eq!(whole[0].buckets.len(), 115, "120 minutes but the 5 without samples");
    for limit in [1, 7, 60, 100, 1000] {
        assert_eq!(joined_groups(&aggregate_pages(&query, Op::Avg, limit)), whole, "pages of {limit}");
    }
    // the block that holds the gap is dense, so its five empty minutes count on the page that reads it
    let held = |page: &AggregatePage| page.groups.iter().map(|group| group.buckets.len()).sum::<usize>();
    let pages: Vec<usize> = aggregate_pages(&query, Op::Avg, 100).iter().map(held).collect();
    assert_eq!(pages, [95, 20]);
}

/// A thousand pods' cpu whose one sample is two hours old, then one pod's of
/// now, written after them: more series than one query finds, all but the
/// last without a sample in the last hour.
fn dead_first(f: &Fixture) {
    let pod = |index: usize, at| Series::gauge("cpu").label("pod", format!("p{index:04}")).sample(at, index as f64);
    let dead: Vec<Series> = (0..1000).map(|index| pod(index, f.now() - 2 * HOUR)).collect();
    f.metrics().ingest(&dead).unwrap();
    f.metrics().ingest(&[pod(1000, f.now())]).unwrap();
}

// contract: MET-21
#[test]
fn an_aggregates_page_goes_on_past_the_groups_without_a_sample_in_its_range() {
    let f = fixture();
    dead_first(&f);
    let query = f.metrics().query().name("cpu").since(HOUR).every(MINUTE).by(&["pod"]);
    let pages = aggregate_pages(&query, Op::Avg, 10);
    let pods: Vec<&str> =
        pages.iter().flat_map(|page| &page.groups).map(|group| group.labels["pod"].as_str()).collect();
    assert_eq!((pages.len(), pods), (1, vec!["p1000"]), "a thousand groups of nothing make no page");
}

// contract: MET-21
#[test]
fn a_sample_past_the_range_counts_no_bucket_toward_a_page() {
    let f = fixture();
    let from = f.now() - 3 * HOUR;
    let host = |host: &str| Series::gauge("temp").label("host", host);
    // a's samples reach across the range, the later one into the bucket that the range's end cuts
    let a = host("a").sample(from - 30 * MINUTE, 1.0).sample(from + 27 * MINUTE, 2.0);
    f.metrics().ingest(&[a, host("b").sample(from + 5 * MINUTE, 3.0)]).unwrap();
    let query = f.metrics().query().name("temp").from(from).to(from + 25 * MINUTE).every(10 * MINUTE).by(&["host"]);
    let whole = query.aggregate(Op::Max).unwrap().groups;
    assert_eq!(whole.iter().map(|group| group.labels["host"].as_str()).collect::<Vec<_>>(), ["b"]);
    assert_eq!(joined_groups(&aggregate_pages(&query, Op::Max, 1)), whole, "a's group is on no page");
}

// contract: MET-17
#[test]
fn an_aggregates_cursor_is_refused_with_another_op_or_another_call() {
    let f = fixture();
    traffic(&f);
    let query = f.metrics().query().name("requests_total").since(HOUR).every(MINUTE).limit(5);
    let next = query.aggregate(Op::Rate).unwrap().next.unwrap();
    let invalid = |error: crate::Error| error.kind();
    assert_eq!(invalid(query.clone().after(&next).aggregate(Op::Increase).unwrap_err()), ErrorKind::Invalid);
    assert_eq!(invalid(query.clone().every(HOUR).after(&next).aggregate(Op::Rate).unwrap_err()), ErrorKind::Invalid);
    let read = f.metrics().query().name("requests_total").since(HOUR).limit(5);
    assert_eq!(invalid(read.clone().after(&next).read().unwrap_err()), ErrorKind::Invalid, "a read takes no group");
    let read_next = read.read().unwrap().next.unwrap();
    let aggregate = query.clone().after(&read_next).aggregate(Op::Rate).unwrap_err();
    assert_eq!(invalid(aggregate), ErrorKind::Invalid, "an aggregate takes no series");
}
