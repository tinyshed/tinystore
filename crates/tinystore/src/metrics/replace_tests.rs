use std::time::{Duration, SystemTime};

use crate::clock::from_unix_millis;
use crate::limits::INGEST_SAMPLES;
use crate::metrics::directory::Row;
use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, bits, fixture};
use crate::metrics::publish::read_group;
use crate::metrics::{Op, Series, maintain};
use crate::{ErrorKind, unix_millis};

/// Today's midnight on the store's clock, `days` back.
fn midnight(f: &Fixture, days: u32) -> SystemTime {
    let today = unix_millis(f.now()) / 86_400_000 * 86_400_000;
    from_unix_millis(today) - DAY * days
}

fn revenue() -> Series {
    Series::gauge("revenue").label("shop", "msk")
}

/// A shop's revenue a day at 1000, from ten days back to today's midnight,
/// ingested and packed but today's, as the issue's collector leaves it.
fn eleven_days(f: &Fixture) {
    let mut series = revenue();
    series.extend((0..=10).rev().map(|days| (midnight(f, days), 1000.0)));
    f.metrics().ingest(&[series]).unwrap();
    assert_eq!(maintain(&f.store).unwrap().packed, 10, "every day but today is packed");
}

/// Every sample of revenue the store keeps, as unix milliseconds and the
/// bits of its value.
fn read(f: &Fixture) -> Vec<(i64, u64)> {
    let read = f.metrics().query().name("revenue").since(30 * DAY).read().unwrap().series;
    read.first().map(bits).unwrap_or_default()
}

/// The samples `days` back with their values, as `read` gives them.
fn days(f: &Fixture, days: &[(u32, f64)]) -> Vec<(i64, u64)> {
    days.iter().map(|&(back, value)| (unix_millis(midnight(f, back)), value.to_bits())).collect()
}

/// What the series' row, its head and its blocks hold, which a replace that
/// changes nothing leaves as it was.
fn kept(f: &Fixture) -> [i64; 7] {
    [
        f.value("select version from series_state"),
        f.value("select sealed_before from series_state"),
        f.value("select head_count from series_state"),
        f.rows("groups"),
        f.rows("payloads"),
        f.rows("clocks"),
        f.value("select next_payload_id from store_state"),
    ]
}

// contract: MET-19, MET-16, NOT-08
#[test]
fn yesterdays_revenue_is_corrected_behind_what_is_packed() {
    let f = fixture();
    eleven_days(&f);
    let sealed = f.value("select sealed_before from series_state");
    // an ingest still refuses the correction, naming where the series is packed up to
    let late = revenue().sample(midnight(&f, 1), 900.0);
    let error = f.metrics().ingest(&[late]).unwrap_err();
    assert_eq!((error.kind(), error.fact("packed").is_some()), (ErrorKind::TooOld, true), "{error}");
    let corrected =
        revenue().sample(midnight(&f, 3), 1000.0).sample(midnight(&f, 2), 1000.0).sample(midnight(&f, 1), 900.0);
    f.metrics().replace(&corrected, midnight(&f, 3)..midnight(&f, 0)).unwrap();
    let mut expected: Vec<(u32, f64)> = (0..=10).rev().map(|back| (back, 1000.0)).collect();
    expected[9].1 = 900.0;
    assert_eq!(read(&f), days(&f, &expected));
    assert_eq!(f.value("select sealed_before from series_state"), sealed, "what is sealed stays");
    let yesterday = f.metrics().query().name("revenue").from(midnight(&f, 1)).to(midnight(&f, 0)).read().unwrap();
    assert_eq!(yesterday.series[0].samples()[0].value, 900.0);
}

// contract: MET-19, MET-06
#[test]
fn an_aggregate_over_the_week_agrees_with_the_samples_read() {
    let f = fixture();
    eleven_days(&f);
    let corrected =
        revenue().sample(midnight(&f, 4), 1250.5).sample(midnight(&f, 2), -0.0).sample(midnight(&f, 1), 900.0);
    f.metrics().replace(&corrected, midnight(&f, 4)..midnight(&f, 0)).unwrap();
    let week = f.metrics().query().name("revenue").from(midnight(&f, 7)).to(midnight(&f, 0) + SECOND);
    let samples: Vec<f64> = week.read().unwrap().series[0].samples().iter().map(|sample| sample.value).collect();
    assert_eq!(samples, [1000.0, 1000.0, 1000.0, 1250.5, -0.0, 900.0, 1000.0], "the day not given went");
    let whole = |op| week.aggregate(op).unwrap().groups[0].buckets[0].value.exact().unwrap();
    assert_eq!(whole(Op::Sum), samples.iter().sum::<f64>());
    assert_eq!(whole(Op::Count), samples.len() as f64);
    assert_eq!(whole(Op::Min).to_bits(), (-0.0_f64).to_bits());
    assert_eq!(whole(Op::Max), 1250.5);
    let daily = week.clone().every(DAY).aggregate(Op::Sum).unwrap().groups[0].buckets.clone();
    let daily: Vec<f64> = daily.iter().map(|bucket| bucket.value.exact().unwrap()).collect();
    assert_eq!(daily, samples, "each day's block answers from a summary made again");
}

// contract: MET-19, MET-01
#[test]
fn a_value_replaced_is_kept_bit_for_bit_in_a_block_and_in_the_head() {
    let f = fixture();
    eleven_days(&f);
    let odd = f64::from_bits(0x7ff4_0000_0000_beef);
    let corrected = revenue().sample(midnight(&f, 1), odd).sample(midnight(&f, 0), f64::from_bits(1));
    f.metrics().replace(&corrected, midnight(&f, 1)..midnight(&f, 0) + SECOND).unwrap();
    let f = f.reopen();
    assert_eq!(read(&f)[9..], bits(&corrected));
}

/// A gauge of three hours a sample 15 s apart, ending half an hour into
/// one: blocks of an hour from half past, a payload each, and the last half
/// hour in the head.
fn dense(f: &Fixture) -> Series {
    f.pass(30 * MINUTE);
    let written = f.gauge("revenue", 721, 15 * SECOND, |index| index as f64 * 0.25).label("shop", "msk");
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    assert_eq!(maintain(&f.store).unwrap().blocks, 3);
    written
}

/// Each live block's first time and its payload's id, 0 for one inline.
fn payloads(f: &Fixture) -> Vec<(i64, i64)> {
    let engine = f.engine();
    let rows: Vec<Row> = engine
        .file
        .read(|connection| {
            let sql = "select series_id, start_ts, end_ts, clock, directory from groups";
            let mut select = connection.prepare(sql).unwrap();
            let rows = select.query_map([], |row| {
                let (series, start, end, clock) = (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
                Ok(Row { series, start, end, clock, directory: row.get(4)? })
            });
            Ok(rows.unwrap().map(Result::unwrap).collect())
        })
        .unwrap();
    let mut zstd = engine.codecs.take();
    let mut found = Vec::new();
    for row in &rows {
        let group = engine.file.read(|connection| read_group(connection, &mut zstd, row)).unwrap();
        let live = (0..group.blocks.len()).filter(|slot| group.is_live(*slot));
        found.extend(live.map(|slot| (group.blocks[slot].times.start, group.blocks[slot].payload)));
    }
    found.sort_unstable();
    found
}

/// The samples of each hour a read gives, by where the hour begins.
fn hours(read: &[(i64, u64)]) -> Vec<Vec<f64>> {
    let hour = 3_600_000;
    let values = read.iter().map(|(at, value)| (at.div_euclid(hour), f64::from_bits(*value)));
    values.collect::<Vec<_>>().chunk_by(|a, b| a.0 == b.0).map(|run| run.iter().map(|(_, v)| *v).collect()).collect()
}

// contract: MET-19, MET-04, MET-06
#[test]
fn the_block_a_stretch_reaches_is_made_again_and_the_others_keep_their_payloads() {
    let f = fixture();
    let written = dense(&f);
    let before = payloads(&f);
    let next = f.value("select next_payload_id from store_state");
    let sealed = f.value("select sealed_before from series_state");
    // inside the first block: one value changed, one dropped, and one added between two
    let samples = written.samples();
    let (from, to) = (samples[100].at, samples[110].at);
    let mut corrected = revenue();
    for (index, sample) in samples[100..110].iter().enumerate() {
        match index {
            3 => {}
            5 => corrected = corrected.sample(sample.at, 1234.5),
            _ => corrected = corrected.sample(sample.at, sample.value),
        }
    }
    let between = samples[107].at + Duration::from_millis(1);
    corrected = corrected.sample(between, -0.0);
    f.metrics().replace(&corrected, from..to).unwrap();
    let mut expected: Vec<(i64, u64)> = bits(&written).into_iter().filter(|(at, _)| *at < unix_millis(from)).collect();
    let mut given = bits(&corrected);
    given.sort_unstable();
    expected.extend(given);
    expected.extend(bits(&written).into_iter().filter(|(at, _)| *at >= unix_millis(to)));
    let read = f.metrics().query().name("revenue").since(4 * HOUR).read().unwrap().series;
    assert_eq!(bits(&read[0]), expected);
    let after = payloads(&f);
    assert_eq!(
        after.iter().map(|(start, _)| start).collect::<Vec<_>>(),
        before.iter().map(|(start, _)| start).collect::<Vec<_>>()
    );
    assert!(after[0].1 >= next, "the block the stretch reaches has a fresh payload");
    assert_eq!(after[1..], before[1..], "a block outside the stretch keeps its payload");
    assert_eq!(f.rows("payloads"), 3, "the old payload went");
    assert_eq!(f.value("select sealed_before from series_state"), sealed);
    // each hour's whole block answers from its summary, which adds up as the samples read
    let range = f.metrics().query().name("revenue").from(samples[0].at).to(samples[600].at).every(HOUR);
    let by_hour = hours(&expected[..expected.len() - 121]);
    for (op, fold) in [
        (Op::Sum, (|values: &[f64]| values.iter().sum::<f64>()) as fn(&[f64]) -> f64),
        (Op::Max, |values: &[f64]| values.iter().copied().fold(f64::MIN, f64::max)),
        (Op::Min, |values: &[f64]| values.iter().copied().fold(f64::MAX, f64::min)),
        (Op::Count, |values: &[f64]| values.len() as f64),
        (Op::Last, |values: &[f64]| values[values.len() - 1]),
    ] {
        let buckets = range.aggregate(op).unwrap().groups[0].buckets.clone();
        let answered: Vec<f64> = buckets.iter().map(|bucket| bucket.value.exact().unwrap()).collect();
        let raw: Vec<f64> = by_hour.iter().map(|values| fold(values)).collect();
        assert_eq!(answered, raw, "{op:?}");
    }
}

// contract: MET-19
#[test]
fn a_day_the_collector_missed_is_added_and_one_the_source_dropped_goes() {
    let f = fixture();
    let mut series = revenue();
    series.extend([10, 9, 8, 6, 5, 4, 3, 2, 1, 0].map(|days| (midnight(&f, days), 1000.0)));
    f.metrics().ingest(&[series]).unwrap();
    maintain(&f.store).unwrap();
    let corrected = revenue().sample(midnight(&f, 8), 1000.0).sample(midnight(&f, 7), 700.0);
    f.metrics().replace(&corrected, midnight(&f, 8)..midnight(&f, 6)).unwrap();
    let dropped = revenue().sample(midnight(&f, 3), 1000.0).sample(midnight(&f, 1), 1000.0);
    f.metrics().replace(&dropped, midnight(&f, 3)..midnight(&f, 0)).unwrap();
    let mut expected = vec![(10, 1000.0), (9, 1000.0), (8, 1000.0), (7, 700.0), (6, 1000.0), (5, 1000.0)];
    expected.extend([(4, 1000.0), (3, 1000.0), (1, 1000.0), (0, 1000.0)]);
    assert_eq!(read(&f), days(&f, &expected));
    // what is packed stays packed: an ingest behind it is still refused
    let error = f.metrics().ingest(&[revenue().sample(midnight(&f, 2), 1.0)]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
}

// contract: MET-19, MET-16
#[test]
fn the_same_samples_again_change_nothing() {
    let f = fixture();
    eleven_days(&f);
    let range = || midnight(&f, 3)..midnight(&f, 0);
    let same =
        revenue().sample(midnight(&f, 3), 1000.0).sample(midnight(&f, 2), 1000.0).sample(midnight(&f, 1), 1000.0);
    let before = (kept(&f), read(&f));
    f.metrics().replace(&same, range()).unwrap();
    assert_eq!((kept(&f), read(&f)), before, "the stretch held them already: no write");
    let corrected = revenue().sample(midnight(&f, 2), 1000.0).sample(midnight(&f, 1), 900.0);
    f.metrics().replace(&corrected, range()).unwrap();
    let after = (kept(&f), read(&f));
    assert_ne!(after, before);
    for _ in 0..2 {
        f.metrics().replace(&corrected, range()).unwrap();
        assert_eq!((kept(&f), read(&f)), after, "a correction sent again");
    }
    // a keep given is settled as an ingest settles it, whatever the samples
    f.metrics().replace(&corrected.clone().keep(400 * DAY), range()).unwrap();
    assert_eq!((f.value("select keep from names"), kept(&f)), ((400 * DAY).as_millis() as i64, after.0));
    let error = f.metrics().replace(&corrected.keep(200 * DAY), range()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "another keep while the store is open: {error}");
}

// contract: MET-19, MET-03
#[test]
fn a_counters_stretch_is_refused_and_changes_nothing() {
    let f = fixture();
    let mut requests = Series::counter("requests_total");
    requests.extend((0..=10).rev().map(|days: u32| (midnight(&f, days), f64::from(10 - days))));
    f.metrics().ingest(&[requests]).unwrap();
    maintain(&f.store).unwrap();
    let rows = (f.rows("groups"), f.rows("payloads"), f.value("select version from series_state"));
    let range = midnight(&f, 3)..midnight(&f, 0);
    let counter = Series::counter("requests_total").sample(midnight(&f, 2), 5.0);
    let error = f.metrics().replace(&counter, range.clone()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("requests_total") && error.to_string().contains("gauge"), "{error}");
    let histogram = Series::histogram("rpc_ms_bucket").label("le", "5").sample(midnight(&f, 2), 5.0);
    assert_eq!(f.metrics().replace(&histogram, range.clone()).unwrap_err().kind(), ErrorKind::Invalid);
    // a gauge of a name whose series are counters is refused too, its samples the same or not
    for value in [8.0, 5.0] {
        let gauge = Series::gauge("requests_total").sample(midnight(&f, 2), value);
        let error = f.metrics().replace(&gauge, range.clone()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    }
    assert_eq!((f.rows("groups"), f.rows("payloads"), f.value("select version from series_state")), rows);
}

// contract: MET-19
#[test]
fn a_stretch_across_the_frontier_rewrites_its_blocks_and_its_head_in_one_write() {
    let f = fixture();
    eleven_days(&f);
    let sealed = f.value("select sealed_before from series_state");
    let corrected = revenue()
        .sample(midnight(&f, 2), 2.0)
        .sample(midnight(&f, 1), 1.0)
        .sample(midnight(&f, 0) + MINUTE, 3.0)
        .sample(midnight(&f, 0) + HOUR, 4.0);
    f.metrics().replace(&corrected, midnight(&f, 2)..midnight(&f, 0) + 2 * HOUR).unwrap();
    let mut expected = days(&f, &(3..=10).rev().map(|back| (back, 1000.0)).collect::<Vec<_>>());
    expected.extend(bits(&corrected));
    assert_eq!(read(&f), expected, "today's midnight went, two samples after it came");
    assert_eq!(f.value("select sealed_before from series_state"), sealed);
    assert_eq!(f.value("select head_count from series_state"), 2);
    // the head's new samples pack as any others once the next day comes
    f.pass(DAY);
    let tomorrow = revenue().sample(f.now(), 5.0);
    f.metrics().ingest(std::slice::from_ref(&tomorrow)).unwrap();
    assert_eq!(maintain(&f.store).unwrap().packed, 2);
    expected.extend(bits(&tomorrow));
    assert_eq!(read(&f), expected);
}

// contract: MET-19, DUR-08
#[test]
fn a_read_beside_a_replace_sees_the_stretch_as_it_was_or_as_it_is_never_half() {
    let f = fixture();
    let written = dense(&f);
    let samples = written.samples();
    // from inside the second block, past the third, into the head
    let (from, to) = (samples[200].at, samples[700].at);
    let mut every_15s = revenue();
    every_15s.extend(samples[200..700].iter().map(|sample| (sample.at, 1.0)));
    let mut every_30s = revenue();
    every_30s.extend(samples[200..700].iter().step_by(2).map(|sample| (sample.at, 2.0)));
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let read = f.metrics().query().name("revenue").from(from).to(to).read().unwrap().series;
                let values: Vec<f64> = read[0].samples().iter().map(|sample| sample.value).collect();
                let first = values.len() == 500
                    && values.iter().enumerate().all(|(index, value)| *value == (200 + index) as f64 * 0.25);
                let whole = values == [1.0; 500] || values == [2.0; 250];
                assert!(first || whole, "a read saw half a replace: {} samples", values.len());
            }
        });
        for round in 0..20 {
            let next = if round % 2 == 0 { &every_15s } else { &every_30s };
            f.metrics().replace(next, from..to).unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
    });
}

// contract: MET-19, ERR-05
#[test]
fn a_replace_is_bounded_and_its_stretch_lies_inside_what_the_name_keeps() {
    let f = fixture();
    eleven_days(&f);
    let range = midnight(&f, 3)..midnight(&f, 0);
    let mut many = revenue();
    let most = INGEST_SAMPLES.bound;
    many.extend((0..=most).map(|index| (range.start + Duration::from_millis(index), 1.0)));
    let error = f.metrics().replace(&many, range.clone()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    let facts = (error.fact("limit"), error.fact("wanted"), error.fact("bound"));
    assert_eq!(facts, (Some("ingest samples"), Some("10001"), Some("10000")));
    let refused = [
        (revenue().sample(midnight(&f, 0), 1.0), range.clone(), ErrorKind::Invalid, "outside the range"),
        (revenue(), midnight(&f, 0)..midnight(&f, 3), ErrorKind::Invalid, "ends where it begins"),
        (revenue(), midnight(&f, 31)..midnight(&f, 0), ErrorKind::TooOld, "older than its name keeps"),
        (revenue(), midnight(&f, 1)..f.now() + HOUR, ErrorKind::TooNew, "ten minutes ahead"),
    ];
    let before = (kept(&f), read(&f));
    for (series, range, kind, why) in refused {
        let error = f.metrics().replace(&series, range).unwrap_err();
        assert_eq!(error.kind(), kind, "{error}");
        assert!(error.to_string().contains(why) && error.to_string().contains("revenue"), "{error}");
    }
    assert_eq!((kept(&f), read(&f)), before);
    f.metrics().replace(&revenue(), midnight(&f, 1)..f.now() + 10 * MINUTE).unwrap();
    assert_eq!(read(&f), days(&f, &(2..=10).rev().map(|back| (back, 1000.0)).collect::<Vec<_>>()));
}

// contract: MET-19
#[test]
fn a_series_the_store_lacks_is_written_and_one_left_with_nothing_goes() {
    let f = fixture();
    let range = || midnight(&f, 3)..midnight(&f, 0);
    f.metrics().replace(&revenue(), range()).unwrap();
    assert_eq!(f.rows("series"), 0, "nothing given, nothing to replace");
    let new = revenue().sample(midnight(&f, 2), 5.0).sample(midnight(&f, 1), 6.0);
    f.metrics().replace(&new, range()).unwrap();
    assert_eq!(read(&f), days(&f, &[(2, 5.0), (1, 6.0)]));
    assert_eq!(maintain(&f.store).unwrap().packed, 1);
    f.metrics().replace(&revenue(), range()).unwrap();
    assert_eq!((read(&f), f.rows("series"), f.rows("groups"), f.rows("payloads")), (Vec::new(), 0, 0, 0));
    assert_eq!(f.rows("clocks"), 0, "its clock let go with its last group");
}
