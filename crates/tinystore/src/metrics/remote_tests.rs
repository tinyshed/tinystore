use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::metrics::fixture::{Fixture, SECOND, fixture};

const SESSION: [u8; 16] = [7; 16];

/// The values of the series of `name` and `labels`, oldest first.
fn values(f: &Fixture, name: &str, labels: &[(&str, &str)]) -> Vec<f64> {
    let (from, to) = (UNIX_EPOCH + Duration::from_secs(1_700_000_000), UNIX_EPOCH + Duration::from_secs(1_900_000_000));
    let mut query = f.metrics().query().name(name).from(from).to(to);
    for (label, value) in labels {
        query = query.filter(*label, *value);
    }
    let read = query.read().unwrap();
    let series = read.iter().find(|series| series.labels().len() == labels.len());
    series.map_or(Vec::new(), |series| series.samples().iter().map(|sample| sample.value).collect())
}

fn declared(number: u64, name: &str, form: &str) -> Declared {
    Declared { number, name: name.to_owned(), form: form.to_owned(), unit: None, help: None, keep: None, buckets: None }
}

fn set(number: u64, instrument: u64, labels: &[(&str, &str)]) -> Set {
    let labels = labels.iter().map(|(label, value)| ((*label).to_owned(), (*value).to_owned())).collect();
    Set { number, instrument, labels, overflow: false }
}

fn add(set: u64, n: f64) -> Change {
    Change { set, add: Some(n), ..Change::default() }
}

/// Batch `number` of a counter's changes: it declares the counter and its
/// set of labels, as the first batch a stream sends does.
fn counted(number: u64, n: f64) -> Batch {
    Batch {
        number,
        instruments: vec![declared(1, "requests_total", "counter")],
        sets: vec![set(1, 1, &[("route", "/notes")])],
        changes: vec![add(1, n)],
        flush: false,
    }
}

#[test]
fn a_batch_applies_once_in_order_and_one_sent_again_changes_nothing() {
    let f = fixture();
    let stats = f.metrics();
    let (mut remote, ack) = stats.remote(SESSION, 0);
    assert_eq!(ack, Ack { applied: 0, known: false, refused: Vec::new() });
    assert_eq!(remote.apply(counted(1, 5.0)).unwrap().applied, 1);
    // its answer lost with its connection, the client sends it again on another
    drop(remote);
    let (mut again, ack) = stats.remote(SESSION, 0);
    assert_eq!((ack.applied, ack.known), (1, true));
    assert_eq!(again.apply(counted(1, 5.0)).unwrap().applied, 1, "applied already: answered, nothing changes");
    let next = Batch { number: 2, changes: vec![add(1, 2.0)], ..Batch::default() };
    assert_eq!(again.apply(next).unwrap().applied, 2, "what the batch sent again declared serves the next");
    let past = again.apply(Batch { number: 4, ..Batch::default() }).unwrap_err();
    assert_eq!(past.kind(), crate::ErrorKind::Invalid);
    assert!(past.to_string().contains("batch 4, where the session's next is 3"), "{past}");
    stats.flush().unwrap();
    assert_eq!(values(&f, "requests_total", &[("route", "/notes")]), [0.0, 7.0], "5 and 2, each once");
}

#[test]
fn a_store_started_again_knows_no_session_and_numbers_on_from_its_clients_last_answer() {
    let f = fixture();
    let (mut remote, _) = f.metrics().remote(SESSION, 0);
    remote.apply(counted(1, 1.0)).unwrap();
    remote.apply(Batch { number: 2, changes: vec![add(1, 1.0)], ..Batch::default() }).unwrap();
    drop(remote);
    let f = f.reopen();
    let (mut remote, ack) = f.metrics().remote(SESSION, 2);
    assert_eq!(ack, Ack { applied: 2, known: false, refused: Vec::new() });
    assert_eq!(remote.apply(counted(3, 1.0)).unwrap().applied, 3);
}

#[test]
fn an_instrument_the_store_refuses_is_answered_and_its_changes_go_nowhere() {
    let f = fixture();
    let stats = f.metrics();
    stats.ingest(&[crate::metrics::Series::gauge("queue_depth").sample(f.now(), 3.0)]).unwrap();
    let (mut remote, _) = stats.remote(SESSION, 0);
    let batch = Batch {
        number: 1,
        instruments: vec![declared(1, "queue_depth", "counter"), declared(2, "jobs_total", "counter")],
        sets: vec![set(1, 1, &[]), set(2, 2, &[])],
        changes: vec![add(1, 4.0), add(2, 1.0)],
        flush: true,
    };
    let ack = remote.apply(batch).unwrap();
    assert_eq!(ack.refused.len(), 1);
    assert_eq!(ack.refused[0].0, 1);
    assert!(ack.refused[0].1.contains("where the name's series are gauges"), "{}", ack.refused[0].1);
    assert_eq!(values(&f, "queue_depth", &[]), [3.0], "the gauge holds what it held");
    assert_eq!(values(&f, "jobs_total", &[]), [0.0, 1.0], "the other instrument counts");
}

#[test]
fn a_timers_parts_move_together_and_a_change_whose_buckets_do_not_add_up_is_refused() {
    let f = fixture();
    let (mut remote, _) = f.metrics().remote(SESSION, 0);
    let latency = Declared { buckets: Some(Buckets::Bounds(vec![5.0, 10.0])), ..declared(1, "db_ms", "timer") };
    let measured = Change { set: 1, count: 3, sum: 22.0, longest: 12.0, buckets: vec![1, 1, 1], ..Change::default() };
    let wrong = Change { set: 1, count: 3, sum: 1.0, longest: 1.0, buckets: vec![1, 1, 0], ..Change::default() };
    let batch = Batch {
        number: 1,
        instruments: vec![latency],
        sets: vec![set(1, 1, &[])],
        changes: vec![measured, wrong],
        flush: true,
    };
    remote.apply(batch).unwrap();
    assert_eq!(values(&f, "db_ms_count", &[]), [0.0, 3.0]);
    assert_eq!(values(&f, "db_ms_sum", &[]), [0.0, 22.0]);
    assert_eq!(values(&f, "db_ms_max", &[]), [12.0]);
    let buckets: Vec<Vec<f64>> =
        ["5", "10", "+Inf"].iter().map(|le| values(&f, "db_ms_bucket", &[("le", le)])).collect();
    assert_eq!(buckets, [[0.0, 1.0], [0.0, 2.0], [0.0, 3.0]], "the change whose buckets miss one counts nowhere");
}

#[test]
fn an_exponential_timers_intervals_count_where_the_client_put_them() {
    let f = fixture();
    let (mut remote, _) = f.metrics().remote(SESSION, 0);
    let latency = Declared { buckets: Some(Buckets::Exponential), ..declared(1, "db_ms", "timer") };
    let bins = vec![("3:0".to_owned(), 2), ("zero".to_owned(), 1)];
    let measured = Change { set: 1, count: 3, sum: 2.0, longest: 1.0, bins, ..Change::default() };
    let batch = Batch {
        number: 1,
        instruments: vec![latency],
        sets: vec![set(1, 1, &[])],
        changes: vec![measured],
        flush: true,
    };
    remote.apply(batch).unwrap();
    let read = f.metrics().query().name("db_ms_bucket").since(Duration::from_hours(1)).read().unwrap();
    let mut intervals: Vec<(String, Vec<f64>)> = read
        .iter()
        .map(|series| (series.labels()["__bin"].clone(), series.samples().iter().map(|sample| sample.value).collect()))
        .collect();
    intervals.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(intervals, [("3:0".to_owned(), vec![0.0, 2.0]), ("zero".to_owned(), vec![0.0, 1.0])]);
}

#[test]
fn a_gauge_leaves_the_totals_with_its_stream() {
    let f = fixture();
    let stats = f.metrics();
    let (mut remote, _) = stats.remote(SESSION, 0);
    let batch = Batch {
        number: 1,
        instruments: vec![declared(1, "in_flight", "gauge")],
        sets: vec![set(1, 1, &[])],
        changes: vec![Change { set: 1, value: Some(7.0), ..Change::default() }],
        flush: true,
    };
    remote.apply(batch).unwrap();
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(values(&f, "in_flight", &[]), [7.0, 7.0], "written while its stream is open");
    drop(remote);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(values(&f, "in_flight", &[]), [7.0, 7.0], "and no longer once it ended");
}
