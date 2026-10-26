use std::time::{Duration, Instant};

use crate::metrics::fixture::{DAY, Fixture, HOUR, MINUTE, SECOND, bits, fixture};
use crate::metrics::timestamp::rfc3339;
use crate::metrics::{Op, Series, maintain};
use crate::{ErrorKind, unix_millis};

/// A gauge of three hours of samples 15 s apart, ending at the clock, ingested
/// and packed but its newest hour, as a collector's step writes it.
fn packed(f: &Fixture, value: impl Fn(usize) -> f64) -> Series {
    let written = f.gauge("revenue", 721, 15 * SECOND, value).label("site", "kazan");
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    assert!(maintain(&f.store).unwrap().packed > 0);
    written
}

/// What the series' row, its head and its blocks hold, which a call that
/// changes nothing leaves as it was.
fn kept(f: &Fixture) -> [i64; 5] {
    [
        f.value("select version from series_state"),
        f.value("select sealed_before from series_state"),
        f.value("select head_count from series_state"),
        f.rows("groups"),
        f.rows("payloads"),
    ]
}

/// The series' samples as a read gives them.
fn read(f: &Fixture) -> Vec<(i64, u64)> {
    bits(&f.metrics().query().name("revenue").since(4 * HOUR).read().unwrap().series[0])
}

/// `series` with its sample at `index` given another value.
fn changed(series: &Series, index: usize, value: f64) -> Series {
    let mut again = Series::gauge("revenue").label("site", "kazan");
    again.extend(
        series
            .samples()
            .iter()
            .enumerate()
            .map(|(at, sample)| (sample.at, if at == index { value } else { sample.value })),
    );
    again
}

// contract: MET-16, MET-03
#[test]
fn a_batch_sent_again_after_packing_changes_nothing() {
    let f = fixture();
    let written = packed(&f, |index| index as f64 * 0.25);
    let before = (kept(&f), read(&f));
    assert!(unix_millis(written.samples()[0].at) < before.0[1], "the batch reaches behind what is packed");
    for _ in 0..2 {
        f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
        assert_eq!((kept(&f), read(&f)), before);
    }
}

// contract: MET-16, MET-01, MET-03, ERR-01
#[test]
fn a_sample_behind_what_is_packed_that_is_not_the_one_kept_is_too_old() {
    let f = fixture();
    let odd = f64::from_bits(0x7ff4_0000_0000_beef);
    let written = packed(&f, |index| match index {
        10 => -0.0,
        20 => odd,
        index => index as f64,
    });
    let before = (kept(&f), read(&f));
    let sealed = before.0[1];
    let between = written.samples()[30].at + Duration::from_millis(1);
    let refused = [
        (changed(&written, 10, 0.0), "another value"),
        (changed(&written, 20, f64::from_bits(0x7ff4_0000_0000_beee)), "another value"),
        (changed(&written, 40, 40.000_000_000_001), "another value"),
        (written.clone().sample(between, 30.0), "no sample"),
        (Series::gauge("revenue").label("site", "kazan").sample(between, 30.0), "no sample"),
    ];
    for (series, held) in refused {
        // with a sample past what is packed, which the refusal keeps out too
        let series = series.sample(f.now(), 9.0);
        let error = f.metrics().ingest(&[series]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
        assert!(
            error.to_string().contains(&format!("behind what its series has packed, which holds {held}")),
            "{error}"
        );
        assert_eq!(error.fact("packed"), Some(rfc3339(sealed).as_str()));
        assert!(error.fact("at").is_some_and(|at| at < rfc3339(sealed).as_str()), "{error}");
        assert_eq!((kept(&f), read(&f)), before, "{error}");
    }
}

// contract: MET-16
#[test]
fn a_batch_sent_again_with_newer_samples_writes_the_newer() {
    let f = fixture();
    let written = packed(&f, |index| index as f64);
    f.pass(MINUTE);
    let newer = written.clone().sample(f.now() - 30 * SECOND, 1000.0).sample(f.now(), 1001.0);
    f.metrics().ingest(&[newer]).unwrap();
    let read = read(&f);
    assert_eq!(read.len(), 723);
    assert_eq!(&read[..721], &bits(&written)[..]);
    assert_eq!(read[721..].iter().map(|(_, value)| f64::from_bits(*value)).collect::<Vec<_>>(), [1000.0, 1001.0]);
}

// contract: MET-16, MET-12
#[test]
fn a_batch_sent_again_past_what_its_name_keeps_is_too_old() {
    let f = fixture();
    let written = f.gauge("revenue", 721, 15 * SECOND, |index| index as f64).keep(4 * HOUR);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    maintain(&f.store).unwrap();
    f.pass(2 * HOUR);
    maintain(&f.store).unwrap();
    let error = f.metrics().ingest(&[written]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld, "{error}");
    assert!(error.to_string().contains("older than its name keeps"), "{error}");
}

// contract: MET-16, MET-04
#[test]
fn a_counter_sent_again_across_a_reset_keeps_its_increase() {
    let f = fixture();
    let first = f.now() - 4 * DAY;
    let mut counted = Series::counter("jobs_total");
    counted.extend([100.0, 110.0, 5.0, 20.0].into_iter().zip(0_u32..).map(|(value, day)| (first + DAY * day, value)));
    let increase = || {
        let answer =
            f.metrics().query().name("jobs_total").from(first).to(f.now()).aggregate(Op::Increase).unwrap().groups;
        answer[0].buckets[0].value.exact().unwrap()
    };
    f.metrics().ingest(std::slice::from_ref(&counted)).unwrap();
    assert!(maintain(&f.store).unwrap().packed > 0);
    assert_eq!(increase(), 30.0, "10, then 5 from the reset, then 15");
    f.metrics().ingest(&[counted]).unwrap();
    assert_eq!(increase(), 30.0);
}

/// What a collector's retries cost the writer: many series' batches sent
/// again after packing, against the same batches written the first time.
///
/// `cargo test -p tinystore --release --lib metrics::repeat::tests -- --ignored --nocapture`
#[test]
#[ignore = "a timing, run by hand"]
fn timing_batches_sent_again_after_packing() {
    let f = fixture();
    let calls: Vec<Vec<Series>> = (0..8)
        .map(|call| {
            let series =
                |one| f.gauge("revenue", 721, 15 * SECOND, |index| index as f64).label("site", format!("{one}"));
            (0..13).map(|one| series(call * 13 + one)).collect()
        })
        .collect();
    let timed = |what: &str| {
        let started = Instant::now();
        calls.iter().for_each(|call| f.metrics().ingest(call).unwrap());
        let took = started.elapsed();
        println!("{what}: {} calls of 9373 samples, {:?} a call", calls.len(), took / calls.len() as u32);
    };
    timed("written");
    println!("packed: {:?}", maintain(&f.store).unwrap().packed);
    timed("sent again");
}
