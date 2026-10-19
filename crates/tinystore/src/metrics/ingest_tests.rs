use std::time::Duration;

use crate::ErrorKind;
use crate::metrics::fixture::{DAY, HOUR, MINUTE, SECOND, bits, fixture};
use crate::metrics::{Kind, Series};

// contract: MET-01
#[test]
fn a_sample_comes_back_bit_for_bit() {
    let f = fixture();
    let values = [
        -0.0,
        0.0,
        f64::NAN,
        f64::from_bits(0x7ff4_0000_0000_beef),
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::from_bits(1),
    ];
    let written = f.gauge("odd", values.len(), SECOND, |index| values[index]);
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    let read = f.metrics().query().name("odd").since(HOUR).read().unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(bits(&read[0]), bits(&written));
    assert_eq!((read[0].name(), read[0].kind()), ("odd", Kind::Gauge));
}

#[test]
fn a_histograms_bound_is_written_as_javascript_writes_it_and_one_without_is_refused() {
    let f = fixture();
    let now = f.now();
    let bucket = |le: &str| Series::histogram("rpc_ms_bucket").label("le", le).sample(now, 1.0);
    f.metrics().ingest(&[bucket("5.0"), bucket("1e3"), bucket("+Inf"), bucket("0.250")]).unwrap();
    // the same bound however it is spelt is one series
    f.metrics().ingest(&[bucket("5").sample(now - SECOND, 0.0)]).unwrap();
    let read = f.metrics().query().name("rpc_ms_bucket").since(HOUR).read().unwrap();
    let mut bounds: Vec<(&str, usize)> =
        read.iter().map(|series| (series.labels()["le"].as_str(), series.samples().len())).collect();
    bounds.sort_unstable();
    assert_eq!(bounds, [("+Inf", 1), ("0.25", 1), ("1000", 1), ("5", 2)]);
    for refused in [Series::histogram("rpc_ms_bucket").sample(now, 1.0), bucket("five"), bucket("-1"), bucket("NaN")] {
        let error = f.metrics().ingest(&[refused]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        assert!(error.to_string().contains("its bound, `le`"), "{error}");
    }
}

// contract: MET-01
#[test]
fn a_sample_survives_a_restart() {
    let f = fixture();
    let written = f.gauge("cpu", 600, SECOND, |index| index as f64 / 4.0).label("host", "web-1");
    f.metrics().ingest(std::slice::from_ref(&written)).unwrap();
    let f = f.reopen();
    let read = f.metrics().query().name("cpu").since(HOUR).read().unwrap();
    assert_eq!(bits(&read[0]), bits(&written));
    assert_eq!(read[0].labels()["host"], "web-1");
}

// contract: MET-02
#[test]
fn a_series_is_its_name_and_its_labels_in_any_order() {
    let f = fixture();
    let now = f.now();
    let one = Series::gauge("cpu").label("host", "a").label("zone", "1").sample(now - SECOND, 1.0);
    let same = Series::gauge("cpu").label("zone", "1").label("host", "a").sample(now, 2.0);
    let other = Series::gauge("cpu").label("host", "a").sample(now, 3.0);
    f.metrics().ingest(&[one, same, other]).unwrap();
    assert_eq!(f.rows("series"), 2);
    let read = f.metrics().query().name("cpu").filter("zone", "1").since(HOUR).read().unwrap();
    assert_eq!(read[0].samples().iter().map(|sample| sample.value).collect::<Vec<_>>(), [1.0, 2.0]);
}

// contract: MET-02
#[test]
fn an_ingest_is_atomic_and_a_time_given_twice_keeps_the_last_value() {
    let f = fixture();
    let now = f.now();
    let good = Series::gauge("cpu").label("host", "a").sample(now, 1.0);
    let bad = Series::gauge("cpu").label("__host", "b").sample(now, 2.0);
    let error = f.metrics().ingest(&[good.clone(), bad]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(f.metrics().query().name("cpu").since(HOUR).read().unwrap().is_empty(), "nothing of the call was written");

    let twice = good.clone().sample(now - SECOND, 5.0).sample(now, 7.0);
    f.metrics().ingest(&[twice]).unwrap();
    f.metrics().ingest(&[Series::gauge("cpu").label("host", "a").sample(now - SECOND, 6.0)]).unwrap();
    let read = f.metrics().query().name("cpu").since(HOUR).read().unwrap();
    assert_eq!(read[0].samples().iter().map(|sample| sample.value).collect::<Vec<_>>(), [6.0, 7.0]);
}

// contract: ERR-01, ERR-02
#[test]
fn a_refused_series_names_itself_and_its_sample() {
    let f = fixture();
    let stale = Series::gauge("cpu").label("host", "web-1").sample(f.now() - 31 * DAY, 1.0);
    let error = f.metrics().ingest(&[stale]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TooOld);
    assert!(error.to_string().starts_with(r#"metrics cpu{host="web-1"}: sample at 2026-12-15T08:00:00Z"#), "{error}");
    let ahead = Series::gauge("cpu").label("host", "web-1").sample(f.now() + 11 * MINUTE, 1.0);
    assert_eq!(f.metrics().ingest(&[ahead]).unwrap_err().kind(), ErrorKind::TooNew);
    let near = Series::gauge("cpu").label("host", "web-1").sample(f.now() + 10 * MINUTE, 1.0);
    f.metrics().ingest(&[near]).unwrap();
}

#[test]
fn a_name_has_one_kind_while_it_has_series() {
    let f = fixture();
    let now = f.now();
    f.metrics().ingest(&[Series::gauge("jobs").label("queue", "a").sample(now, 1.0)]).unwrap();
    let error = f.metrics().ingest(&[Series::counter("jobs").label("queue", "b").sample(now, 1.0)]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let error = f.metrics().ingest(&[Series::counter("jobs").label("queue", "a").sample(now, 1.0)]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    let both = [Series::gauge("x").sample(now, 1.0), Series::counter("x").label("a", "b").sample(now, 1.0)];
    assert_eq!(f.metrics().ingest(&both).unwrap_err().kind(), ErrorKind::Invalid);
}

// contract: MET-12
#[test]
fn a_names_keep_is_its_own_and_another_while_the_store_is_open_is_refused() {
    let f = fixture();
    let now = f.now();
    let week = Duration::from_hours(7 * 24);
    f.metrics().ingest(&[Series::gauge("short").sample(now, 1.0).keep(week)]).unwrap();
    f.metrics().ingest(&[Series::gauge("short").sample(now, 2.0).keep(week)]).unwrap();
    f.metrics().ingest(&[Series::gauge("short").label("a", "b").sample(now, 2.0)]).unwrap();
    let error = f.metrics().ingest(&[Series::gauge("short").sample(now, 3.0).keep(2 * week)]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains("keep 14d, where it is kept 7d"), "{error}");
    let old = Series::gauge("short").sample(now - 8 * DAY, 1.0);
    assert_eq!(f.metrics().ingest(&[old]).unwrap_err().kind(), ErrorKind::TooOld, "the name keeps a week");
    let f = f.reopen();
    f.metrics().ingest(&[Series::gauge("short").sample(now, 3.0).keep(2 * week)]).unwrap();
    f.metrics().ingest(&[Series::gauge("short").sample(now - 8 * DAY, 1.0)]).unwrap();
    assert_eq!(
        f.metrics().ingest(&[Series::gauge("x").sample(now, 1.0).keep(Duration::ZERO)]).unwrap_err().kind(),
        ErrorKind::Invalid
    );
}

// contract: ERR-05, MET-07
#[test]
fn an_ingest_past_its_bound_is_a_limit() {
    let f = fixture();
    let many = f.gauge("many", 10_001, Duration::from_millis(10), |index| index as f64);
    let error = f.metrics().ingest(&[many]).unwrap_err();
    assert_eq!((error.kind(), error.fact("limit")), (ErrorKind::Limit, Some("one ingest")));
    let fits = f.gauge("many", 10_000, Duration::from_millis(10), |index| index as f64);
    f.metrics().ingest(&[fits]).unwrap();
}

// contract: ERR-05, MET-07
#[test]
fn a_series_past_the_stores_bound_is_a_limit() {
    let f = fixture();
    let now = f.now();
    f.metrics().ingest(&[Series::gauge("cpu").label("host", "a").sample(now, 1.0)]).unwrap();
    let full = |tx: &crate::sqlite::Tx<'_>| Ok(tx.execute("update store_state set series_count = 100000", []).unwrap());
    f.engine().file.write(0, full).unwrap();
    let error = f.metrics().ingest(&[Series::gauge("cpu").label("host", "b").sample(now, 1.0)]).unwrap_err();
    assert_eq!(
        (error.kind(), error.fact("limit"), error.fact("bound")),
        (ErrorKind::Limit, Some("series in a store"), Some("100000"))
    );
    f.metrics().ingest(&[Series::gauge("cpu").label("host", "a").sample(now, 2.0)]).unwrap();
}

#[test]
fn a_series_without_samples_is_refused() {
    let f = fixture();
    assert_eq!(f.metrics().ingest(&[Series::gauge("cpu")]).unwrap_err().kind(), ErrorKind::Invalid);
    f.metrics().ingest(&[]).unwrap();
}

#[test]
fn samples_in_any_order_come_back_in_the_order_of_their_times() {
    let f = fixture();
    let now = f.now();
    let mut shuffled = Series::gauge("shuffled");
    shuffled.extend((0..500_u32).map(|n| ((n * 7919) % 500, n)).map(|(at, n)| (now - SECOND * at, f64::from(n))));
    f.metrics().ingest(&[shuffled]).unwrap();
    let read = f.metrics().query().name("shuffled").since(HOUR).read().unwrap();
    let times: Vec<i64> = bits(&read[0]).iter().map(|(at, _)| *at).collect();
    assert_eq!(times.len(), 500);
    assert!(times.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn a_call_refused_for_its_times_makes_no_name_and_moves_no_keep() {
    let f = fixture();
    let now = f.now();
    let week = Duration::from_hours(7 * 24);
    let refused =
        [Series::gauge("fresh").sample(now, 1.0).keep(week), Series::gauge("stale").sample(now - 31 * DAY, 1.0)];
    assert_eq!(f.metrics().ingest(&refused).unwrap_err().kind(), ErrorKind::TooOld);
    let names: i64 = f
        .engine()
        .file
        .read(|c| {
            c.query_row("select count(*) from names", [], |row| row.get(0))
                .map_err(|e| crate::sqlite::sql_error("names", e))
        })
        .unwrap();
    assert_eq!(names, 0, "every series or none");
    f.metrics().ingest(&[Series::gauge("fresh").sample(now, 1.0).keep(2 * week)]).unwrap();
}
