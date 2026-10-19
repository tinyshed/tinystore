use std::time::Duration;

use crate::metrics::fixture::{DAY, HOUR, SECOND, fixture};
use crate::metrics::{Series, maintain};

// contract: MET-12
#[test]
fn samples_past_what_their_name_keeps_go_and_the_rest_stay() {
    let f = fixture();
    let start = f.now();
    let series = f.gauge("cpu", 100, HOUR, |index| index as f64).label("host", "a");
    f.metrics().ingest(&[series]).unwrap();
    // 100 hours of samples up to now; 30 days later the oldest go
    f.pass(30 * DAY - 50 * HOUR);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (49, 0));
    let read = f.metrics().query().name("cpu").read().unwrap();
    assert_eq!(read[0].samples().len(), 51);
    assert_eq!(read[0].samples()[0].at, start - 50 * HOUR);
    assert_eq!(maintain(&f.store).unwrap(), Default::default(), "nothing more is due");
}

#[test]
fn a_series_left_with_nothing_goes_with_its_pairs_and_frees_its_place_and_its_names_kind() {
    let f = fixture();
    let now = f.now();
    let gone = Series::gauge("jobs").label("queue", "mail").label("region", "eu").sample(now, 1.0);
    let stays = Series::gauge("disk").label("region", "eu").sample(now + 20 * DAY, 1.0);
    f.metrics().ingest(&[gone]).unwrap();
    f.pass(20 * DAY);
    f.metrics().ingest(&[stays]).unwrap();
    assert_eq!((f.rows("series"), f.rows("label_values")), (2, 4));
    f.pass(11 * DAY);
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (1, 1));
    assert_eq!(f.rows("series"), 1);
    assert_eq!(f.value("select series_count from store_state"), 1);
    // __name__=jobs and queue=mail go; region=eu stays with disk's series
    assert_eq!(f.rows("label_values"), 2);
    assert_eq!(f.value("select posting_count from label_values where name = 'region'"), 1);
    assert_eq!(f.rows("heads"), 1);
    // with no series left, the name may take another kind
    f.metrics().ingest(&[Series::counter("jobs").sample(f.now(), 1.0)]).unwrap();
}

#[test]
fn a_keep_changed_between_runs_moves_when_its_series_are_due() {
    let f = fixture();
    let now = f.now();
    let week = Duration::from_hours(7 * 24);
    f.metrics().ingest(&[Series::gauge("short").sample(now, 1.0).keep(2 * week)]).unwrap();
    f.pass(week + SECOND);
    assert_eq!(maintain(&f.store).unwrap().expired, 0);
    let f = f.reopen();
    f.metrics().ingest(&[Series::gauge("short").label("a", "b").sample(f.now(), 1.0).keep(week)]).unwrap();
    let done = maintain(&f.store).unwrap();
    assert_eq!((done.expired, done.reclaimed), (1, 1), "the first series is past a week now");
}
