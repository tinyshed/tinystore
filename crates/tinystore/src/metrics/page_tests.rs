use std::time::Duration;

use super::{Cursor, Place, SeriesPage};
use crate::metrics::budget::Budget;
use crate::metrics::fixture::{Fixture, HOUR, MINUTE, SECOND, fixture};
use crate::metrics::{Query, Series, maintain};
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
    let mut place = Place::FIRST;
    while let Some(next) = pages.last().unwrap().next.clone() {
        let went_on = Cursor::read(&next).unwrap().place;
        assert!((went_on.series, went_on.at) > (place.series, place.at), "{went_on:?} after {place:?}");
        place = went_on;
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
    let (asked, paging) = query.clone().limit(10_000).paged().unwrap();
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
    let place = Place { series: 42, at: -1_800_000_000_000 };
    let cursor = Cursor { query: "00ff".to_owned(), from: i64::MIN, to: 7, now: 6, place };
    assert_eq!(Cursor::read(&cursor.written()).unwrap(), cursor);
}
