use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::ErrorKind;
use crate::metrics::fixture::{DAY, HOUR, MINUTE, SECOND, bits, fixture};
use crate::metrics::{Series, maintain};

/// A group as its start, its end and each block's first and last times and
/// count.
type Found = (i64, i64, Vec<(i64, i64, usize)>);

/// The groups of the file.
fn groups(f: &crate::metrics::fixture::Fixture) -> Vec<Found> {
    let engine = f.engine();
    let rows: Vec<(i64, i64, i64, i64, Vec<u8>)> = engine
        .file
        .read(|connection| {
            let mut select = connection
                .prepare(
                    "select series_id, start_ts, end_ts, clock, directory from groups order by series_id, start_ts",
                )
                .unwrap();
            let rows =
                select.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)));
            Ok(rows.unwrap().map(Result::unwrap).collect())
        })
        .unwrap();
    let mut zstd = engine.codecs.take();
    rows.into_iter()
        .map(|(series, start, end, clock, directory)| {
            let row = crate::metrics::directory::Row { series, start, end, clock, directory };
            let group = engine
                .file
                .read(|connection| crate::metrics::publish::read_group(connection, &mut zstd, &row))
                .unwrap();
            let blocks =
                group.blocks.iter().map(|block| (block.times.start, block.times.end, block.times.count)).collect();
            (start, end, blocks)
        })
        .collect()
}

// contract: MET-01
#[test]
fn a_series_is_cut_into_blocks_where_its_hours_begin_and_reads_back_whole() {
    let f = fixture();
    // three hours and a half of 15-second samples, ending at the clock, an hour's start
    let written = f.gauge("cpu", 841, 15 * SECOND, |index| (index % 97) as f64 * 0.25);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!(
        (done.packed, done.blocks),
        (840, 4),
        "three whole hours and the half before them; the newest sample waits"
    );
    let hour = 3_600_000;
    let found = groups(&f);
    let blocks: Vec<(i64, i64, usize)> = found.iter().flat_map(|(_, _, blocks)| blocks.clone()).collect();
    assert_eq!(blocks.iter().map(|block| block.2).collect::<Vec<_>>(), [120, 240, 240, 240]);
    assert!(blocks.iter().all(|(start, end, _)| start.div_euclid(hour) == end.div_euclid(hour)), "{blocks:?}");
    assert_eq!(f.value("select head_count from series_state"), 1);
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(bits(&read[0]), bits(&written));
    let f = f.reopen();
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(bits(&read[0]), bits(&written), "after a restart too");
}

#[test]
fn a_range_reads_the_blocks_and_the_head_it_touches_and_no_more() {
    let f = fixture();
    let written = f.gauge("cpu", 2401, 15 * SECOND, |index| index as f64);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    maintain(&f.store).unwrap();
    let start = written.samples()[1000].at;
    let read = f.metrics().query().name("cpu").from(start).to(start + 10 * MINUTE).read().unwrap().series;
    assert_eq!(read[0].samples().len(), 40);
    assert_eq!(read[0].samples()[0].value, 1000.0);
    let mut budget = crate::metrics::budget::Budget::default();
    let asked = f.metrics().query().name("cpu").from(start).to(start + 10 * MINUTE).asked().unwrap();
    f.engine().take(&asked, crate::metrics::snapshot::Want::Samples, &mut budget).unwrap();
    assert!(budget.decoded <= 480, "one or two hourly blocks, not ten: {budget:?}");
}

// contract: DUR-08
#[test]
fn a_read_sees_one_state_while_ingests_and_packing_go_on_beside_it() {
    let f = fixture();
    let written = f.gauge("cpu", 4000, 15 * SECOND, |index| index as f64);
    let (from, to) = (written.samples()[0].at, written.samples()[3999].at + SECOND);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let read = f.metrics().query().name("cpu").from(from).to(to).read().unwrap().series;
                let values: Vec<f64> =
                    read.iter().flat_map(|series| series.samples().iter().map(|s| s.value)).collect();
                // what a read sees is every ingest whole, each sample once, in a head or in a block
                assert_eq!(values.len() % 100, 0, "a read saw part of an ingest");
                assert!(values.iter().enumerate().all(|(index, &value)| value == index as f64), "a torn read");
            }
        });
        for piece in written.samples().chunks(100) {
            let mut series = Series::gauge("cpu");
            series.extend(piece.iter().map(|sample| (sample.at, sample.value)));
            f.metrics().ingest(&[series]).unwrap();
            maintain(&f.store).unwrap();
        }
        done.store(true, std::sync::atomic::Ordering::Release);
    });
}

// contract: ERR-01, MET-03
#[test]
fn a_sample_behind_what_its_series_has_packed_is_too_old() {
    let f = fixture();
    let written = f.gauge("cpu", 481, 15 * SECOND, |index| index as f64);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    maintain(&f.store).unwrap();
    let packed = written.samples()[100].at;
    let late = Series::gauge("cpu").sample(packed + Duration::from_millis(1), 1.0);
    let error = f.metrics().ingest(&[late]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
    assert!(error.to_string().contains("behind what its series has packed"), "{error}");
    let newest = Series::gauge("cpu").sample(f.now(), 7.0);
    f.metrics().ingest(&[newest]).unwrap();
}

#[test]
fn series_written_at_the_same_ticks_share_a_clock_until_the_last_lets_go() {
    let f = fixture();
    let jittered = |index: usize| (index % 7) as i64;
    let mut a = Series::gauge("a");
    let mut b = Series::gauge("b");
    let first = f.now() - 15 * SECOND * 480;
    for index in 0..481 {
        let at = first + 15 * SECOND * index as u32 + Duration::from_millis(jittered(index) as u64);
        a.extend([(at, index as f64)]);
        b.extend([(at, -(index as f64))]);
    }
    f.metrics().ingest(&[a, b]).unwrap();
    maintain(&f.store).unwrap();
    assert_eq!(f.rows("clocks"), 1, "one irregular clock both series hold");
    assert_eq!(f.value("select refs from clocks"), 2);
    f.pass(30 * DAY + HOUR);
    maintain(&f.store).unwrap();
    assert_eq!((f.rows("clocks"), f.rows("groups"), f.rows("payloads"), f.rows("series")), (0, 0, 0, 0));
}

#[test]
fn a_group_takes_in_the_one_before_it_while_that_holds_no_more_blocks() {
    let f = fixture();
    let start = f.now();
    let mut written = Vec::new();
    for hour in 0..8_u32 {
        // the hour that just ended, and the sample that closes it
        f.pass(HOUR);
        let mut series = Series::gauge("cpu");
        series.extend(
            (0..240_u32).map(|index| (start + hour * HOUR + 15 * SECOND * index, f64::from(hour * 1000 + index))),
        );
        written.extend(series.samples().iter().copied());
        f.metrics().ingest(&[series, Series::gauge("cpu").sample(f.now(), 0.0)]).unwrap();
        maintain(&f.store).unwrap();
    }
    // a group a publication, each taking in those before while they hold no more: 8 blocks in one group
    let found = groups(&f);
    assert_eq!(found.iter().map(|(_, _, blocks)| blocks.len()).collect::<Vec<_>>(), [8]);
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    let values: Vec<f64> = read[0].samples().iter().map(|sample| sample.value).filter(|value| *value != 0.0).collect();
    assert_eq!(values.len(), 8 * 240 - 1, "every packed sample but the first, which is 0");
}

#[test]
fn a_group_of_the_most_blocks_goes_after_every_row_and_one_still_growing_before() {
    let f = fixture();
    let start = f.now();
    for hour in 0..33_u32 {
        f.pass(HOUR);
        let mut series = Series::gauge("cpu");
        series.extend((0..240_u32).map(|index| (start + hour * HOUR + 15 * SECOND * index, f64::from(index % 7))));
        f.metrics().ingest(&[series, Series::gauge("cpu").sample(f.now(), 0.0)]).unwrap();
        maintain(&f.store).unwrap();
    }
    let sides = |table: &str| -> Vec<i64> {
        let sql = format!("select id > 0 from {table} order by id desc");
        f.engine()
            .file
            .read(|connection| {
                let mut select = connection.prepare(&sql).unwrap();
                Ok(select.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect())
            })
            .unwrap()
    };
    // 32 hourly blocks in a group no merge takes in again, and the newest hour growing beside it
    assert_eq!(groups(&f).iter().map(|(_, _, blocks)| blocks.len()).collect::<Vec<_>>(), [32, 1]);
    assert_eq!(sides("groups"), [1, 0]);
    assert_eq!(sides("clocks"), [1, 0]);
}

#[test]
fn a_unit_holding_more_than_240_samples_is_cut_every_240() {
    let f = fixture();
    // a sample a day ago, 300 within a second twelve hours ago, and one now: units of eight hours
    let burst = f.now() - 12 * HOUR;
    let mut written = Series::gauge("bursty").sample(f.now() - DAY, -1.0);
    written.extend((0..300_u32).map(|index| (burst + Duration::from_millis(u64::from(index)), f64::from(index))));
    let written = written.sample(f.now(), -2.0);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    maintain(&f.store).unwrap();
    let blocks: Vec<usize> = groups(&f).iter().flat_map(|(_, _, blocks)| blocks.iter().map(|block| block.2)).collect();
    assert_eq!(blocks, [1, 240, 60]);
    let read = f.metrics().query().name("bursty").read().unwrap().series;
    assert_eq!(bits(&read[0]), bits(&written));
}

#[test]
fn a_series_a_sample_a_millisecond_packs_before_its_head_is_full() {
    let f = fixture();
    let first = f.now() - Duration::from_secs(40);
    // four ingests of ten seconds each: a head holds 16,384 samples
    for batch in 0..4_u32 {
        let mut series = Series::gauge("fast");
        let from = first + Duration::from_secs(10) * batch;
        series.extend((0..10_000_u32).map(|index| (from + Duration::from_millis(u64::from(index)), f64::from(index))));
        f.metrics().ingest(&[series]).unwrap();
        maintain(&f.store).unwrap();
    }
    let blocks: Vec<usize> = groups(&f).iter().flat_map(|(_, _, blocks)| blocks.iter().map(|block| block.2)).collect();
    assert!(blocks.iter().all(|count| *count == 200), "units of 200 milliseconds: {blocks:?}");
    assert_eq!(f.value("select head_count from series_state") + 200 * blocks.len() as i64, 40_000);
}

#[test]
fn a_quiet_series_is_not_forced_into_a_block() {
    let f = fixture();
    // half-hourly since 03:30: its unit is a day, which ends at midnight
    let written = f.gauge("quiet", 10, 30 * MINUTE, |index| index as f64);
    f.metrics().ingest(&[written]).unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.blocks, 0, "a day's unit not yet behind its newest sample");
    f.pass(30 * DAY + 2 * HOUR);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed, done.blocks), (10, 1, 0), "its head expired whole");
}

/// The one series' blocks as packing would publish them now.
fn staged(f: &crate::metrics::fixture::Fixture) -> Staged {
    let engine = f.engine();
    let mut zstd = engine.codecs.take();
    let id = f.value("select series_id from series_state");
    let candidate = engine.candidate(&mut zstd, id, engine.now()).unwrap().unwrap();
    encode(&mut zstd, candidate).unwrap()
}

// contract: DUR-03, MET-03
#[test]
fn a_publication_another_write_raced_among_its_samples_is_dropped_and_tried_again() {
    let f = fixture();
    let written = f.gauge("cpu", 481, 15 * SECOND, |index| index as f64);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    let among = written.samples()[100].at;
    // a sample packing read changed, and one written between two it read
    let changed = Series::gauge("cpu").sample(among, 9.0);
    let between = Series::gauge("cpu").sample(among + Duration::from_millis(1), 9.0);
    for raced in [changed, between] {
        let staged = staged(&f);
        f.metrics().ingest(&[raced]).unwrap();
        let mut packed = Packed::default();
        f.engine().publish(vec![staged], &mut packed).unwrap();
        assert_eq!((packed.conflicts, packed.blocks), (1, 0));
        assert_eq!((f.rows("groups"), f.rows("payloads"), f.rows("clocks")), (0, 0, 0), "nothing of it was written");
    }
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.packed, 481, "every sample but the newest, the one written between them too");
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(read[0].samples().len(), 482);
}

#[test]
fn a_write_past_what_packing_read_leaves_its_publication_standing() {
    let f = fixture();
    let written = f.gauge("cpu", 481, 15 * SECOND, |index| index as f64);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    let staged = staged(&f);
    // the newest sample, which waits in the head, written again, and one after it
    let later = Series::gauge("cpu").sample(f.now(), 9.0).sample(f.now() + 15 * SECOND, 10.0);
    f.metrics().ingest(&[later]).unwrap();
    let mut packed = Packed::default();
    f.engine().publish(vec![staged], &mut packed).unwrap();
    assert_eq!((packed.conflicts, packed.blocks, packed.samples), (0, 2, 480));
    assert_eq!(f.value("select head_count from series_state"), 2);
    let read = f.metrics().query().name("cpu").to(f.now() + MINUTE).read().unwrap().series;
    let values: Vec<f64> = read[0].samples().iter().map(|sample| sample.value).collect();
    assert_eq!((values.len(), values[479], values[480], values[481]), (482, 479.0, 9.0, 10.0));
}

#[test]
fn a_block_expires_whole_once_its_last_sample_has_and_a_group_keeps_its_payloads_addresses() {
    let f = fixture();
    let written = f.gauge("cpu", 961, 15 * SECOND, |index| (index as f64).sqrt());
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    maintain(&f.store).unwrap();
    let before = groups(&f);
    assert_eq!(before.iter().map(|(_, _, blocks)| blocks.len()).sum::<usize>(), 4);
    // 30 days after the first hour's last sample, that hour's block goes, and only it
    let first_end = before[0].2[0].1;
    let at = UNIX_EPOCH + Duration::from_millis((first_end + 30 * 86_400_000 + 1) as u64);
    f.clock.set(at).unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!(done.expired, 240);
    let after = groups(&f);
    assert_eq!(
        after.iter().map(|(start, _, _)| *start).collect::<Vec<_>>(),
        before.iter().map(|(start, _, _)| *start).collect::<Vec<_>>()
    );
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(read[0].samples().len(), 961 - 240);
    assert_eq!(f.rows("payloads"), 3);
}

/// Two hours of 15-second samples kept two hours, then ten minutes on: the
/// first 39 have expired, and no expiry has taken them yet.
fn partly_expired(f: &crate::metrics::fixture::Fixture) -> Series {
    let mut written = Series::gauge("cpu").keep(2 * HOUR);
    let first = f.now() - 2 * HOUR + 15 * SECOND;
    written.extend((0..480_u32).map(|index| (first + 15 * SECOND * index, f64::from(index))));
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    f.pass(10 * MINUTE);
    written
}

#[test]
fn packing_a_head_whose_oldest_samples_have_expired_leaves_none_of_them_before_its_blocks() {
    let f = fixture();
    let written = partly_expired(&f);
    let engine = f.engine();
    let (packed, _) = engine.pack(engine.now()).unwrap();
    assert_eq!((packed.samples, packed.blocks, packed.expired), (440, 2, 39));
    let read = f.metrics().query().name("cpu").read().unwrap().series;
    assert_eq!(bits(&read[0]), bits(&written)[39..], "every sample its name keeps, in order");
    assert!(f.metrics().damaged().unwrap().is_empty());
    assert_eq!(f.value("select head_count from series_state"), 1, "the newest sample waits");
}

#[test]
fn a_head_packing_expires_first_is_published_only_as_packing_read_it() {
    let f = fixture();
    partly_expired(&f);
    let raced = staged(&f);
    f.metrics().ingest(&[Series::gauge("cpu").sample(f.now(), 1.0)]).unwrap();
    let mut packed = Packed::default();
    f.engine().publish(vec![raced], &mut packed).unwrap();
    assert_eq!((packed.conflicts, packed.expired, f.rows("groups")), (1, 0, 0));
}
