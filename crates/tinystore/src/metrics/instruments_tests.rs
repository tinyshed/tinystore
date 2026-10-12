use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use crate::ErrorKind;
use crate::metrics::fixture::{DAY, Fixture, HOUR, SECOND, fixture};
use crate::metrics::{Not, Op, Series, maintain};

/// The fixture's clock at its start, in milliseconds.
const START: i64 = 1_800_000_000_000;

/// The samples of the series of `name` and `labels`, each as its time from
/// the fixture's start in milliseconds and its value.
fn samples(f: &Fixture, name: &str, labels: &[(&str, &str)]) -> Vec<(i64, f64)> {
    // from long before the fixture's clock to past it, for a write placed after what its series holds
    let (from, to) = (UNIX_EPOCH + Duration::from_secs(1_700_000_000), UNIX_EPOCH + Duration::from_secs(1_900_000_000));
    let mut query = f.metrics().query().name(name).from(from).to(to);
    for (label, value) in labels {
        query = query.filter(*label, *value);
    }
    let read = query.read().unwrap();
    let series = read.iter().find(|series| series.labels().len() == labels.len());
    series.map_or(Vec::new(), |series| {
        series.samples().iter().map(|sample| (crate::unix_millis(sample.at) - START, sample.value)).collect()
    })
}

fn ms(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).unwrap()
}

#[test]
fn a_counter_counts_into_one_total_written_with_its_runs_zero() {
    let f = fixture();
    let stats = f.metrics();
    let requests = stats.counter("http_requests_total").help("Requests served.").open().unwrap();
    let notes = requests.with(&[("route", "/notes"), ("status", "200")]);
    notes.inc();
    notes.add(2.0);
    // the same labels in any order are one series, and a second handle of the name the same instrument
    requests.with(&[("status", "200"), ("route", "/notes")]).inc();
    stats.counter("http_requests_total").open().unwrap().with(&[("route", "/notes"), ("status", "200")]).add(6.0);
    requests.with(&[("route", "/empty")]).add(0.0);
    let _never = requests.with(&[("route", "/never")]);
    let notes_labels = [("route", "/notes"), ("status", "200")];
    assert!(samples(&f, "http_requests_total", &notes_labels).is_empty(), "a call writes nothing");
    stats.flush().unwrap();
    // the run's zero at the store's start, a millisecond before its count, which is of the same millisecond
    assert_eq!(samples(&f, "http_requests_total", &notes_labels), [(-1, 0.0), (0, 10.0)]);
    assert_eq!(
        samples(&f, "http_requests_total", &[("route", "/empty")]),
        [(-1, 0.0), (0, 0.0)],
        "add(0) makes it known"
    );
    assert!(samples(&f, "http_requests_total", &[("route", "/never")]).is_empty(), "a set no call reached");
    f.pass(15 * SECOND);
    notes.inc();
    stats.flush().unwrap();
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    let fifteen = ms(15 * SECOND);
    assert_eq!(
        samples(&f, "http_requests_total", &notes_labels),
        [(-1, 0.0), (0, 10.0), (fifteen, 11.0), (2 * fifteen, 11.0)],
        "a live total is written at each write"
    );
    let help = f.engine().file.read(|connection| {
        Ok(connection
            .query_row("select help from names where name = 'http_requests_total'", [], |row| row.get::<_, String>(0))
            .unwrap())
    });
    assert_eq!(help.unwrap(), "Requests served.");
}

#[test]
fn a_runs_zero_survives_a_first_count_of_the_same_millisecond() {
    let f = fixture();
    let stats = f.metrics();
    let jobs = stats.counter("jobs_total").open().unwrap();
    jobs.add(5.0);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "jobs_total", &[]), [(-1, 0.0), (0, 5.0)]);
    // an hour of writes, and packing cuts the hour the zero lies in into a block of its own
    for _ in 0..241 {
        f.pass(15 * SECOND);
        jobs.inc();
        stats.flush().unwrap();
    }
    assert!(maintain(&f.store).unwrap().blocks >= 2);
    let read = samples(&f, "jobs_total", &[]);
    assert_eq!(read[..3], [(-1, 0.0), (0, 5.0), (15_000, 6.0)], "the zero kept, bit for bit");
    assert_eq!(read[0].1.to_bits(), 0);
    assert_eq!(read.len(), 243);
}

#[test]
fn a_restart_begins_a_new_run_with_its_zero_and_increase_counts_both() {
    let f = fixture();
    f.metrics().counter("jobs_total").open().unwrap().add(2.0);
    // closing the store writes the totals last; it opens again an hour on
    let f = f.reopen();
    f.pass(HOUR);
    let stats = f.metrics();
    stats.counter("jobs_total").open().unwrap().add(5.0);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    let hour = ms(HOUR);
    assert_eq!(samples(&f, "jobs_total", &[]), [(-1, 0.0), (0, 2.0), (hour, 0.0), (hour + ms(15 * SECOND), 5.0)]);
    let grew = f.metrics().query().name("jobs_total").since(DAY).aggregate(Op::Increase).unwrap();
    assert_eq!(
        (grew[0].buckets[0].value, grew[0].buckets[0].resets),
        (7.0, 1),
        "2, then a run that began at 0 and reached 5"
    );
}

#[test]
fn a_store_opened_again_within_the_millisecond_it_closed_writes_after_its_last_sample() {
    let f = fixture();
    f.metrics().counter("jobs_total").open().unwrap().add(2.0);
    let f = f.reopen();
    let stats = f.metrics();
    stats.counter("jobs_total").open().unwrap().add(5.0);
    stats.flush().unwrap();
    // the new run's zero would have fallen on the last run's last sample: it lies after it
    assert_eq!(samples(&f, "jobs_total", &[]), [(-1, 0.0), (0, 2.0), (1, 0.0), (2, 5.0)]);
}

#[test]
fn instruments_of_two_connections_add_into_one_total() {
    let f = fixture();
    let stats = f.metrics();
    let engine = f.engine();
    let (one, two) = (engine.totals.connect(), engine.totals.connect());
    // two processes counting into one series count into one total
    stats.counter("jobs_total").open().unwrap().add(2.0);
    stats.counter("jobs_total").open().unwrap().add(3.0);
    let mail = stats.gauge("queue_depth").owned_by(&one).open().unwrap().with(&[("queue", "mail")]);
    let sync = stats.gauge("queue_depth").owned_by(&two).open().unwrap().with(&[("queue", "sync")]);
    mail.set(3.0);
    sync.set(5.0);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "jobs_total", &[]).last(), Some(&(0, 5.0)));
    // a gauge belongs to the connection that set it, and leaves with it
    drop(one);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "queue_depth", &[("queue", "mail")]), [(0, 3.0)]);
    assert_eq!(samples(&f, "queue_depth", &[("queue", "sync")]), [(0, 5.0), (ms(15 * SECOND), 5.0)]);
    // set again, by whoever sets it
    mail.set(4.0);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "queue_depth", &[("queue", "mail")]).last(), Some(&(ms(30 * SECOND), 4.0)));
}

#[test]
fn the_overflow_is_a_group_of_its_own() {
    let f = fixture();
    let stats = f.metrics();
    let logins = stats.counter("logins_total").open().unwrap();
    for user in 0..2001 {
        logins.with(&[("user", &user.to_string())]).inc();
    }
    logins.with(&[("user", "9999")]).add(2.0);
    stats.flush().unwrap();
    assert_eq!(f.rows("series"), 2001, "2000 sets of labels and the series of __overflow");
    // a query reads 1000 series at most: the users in two halves, and the series no user's label holds
    let users = |range: std::ops::Range<u32>| range.map(|user| user.to_string()).collect::<Vec<_>>();
    let query = || f.metrics().query().name("logins_total").since(HOUR);
    let total = |query: crate::metrics::Query| query.aggregate(Op::Last).unwrap()[0].buckets[0].value;
    let halves = total(query().filter("user", users(0..1000))) + total(query().filter("user", users(1000..2000)));
    let past = query().filter("user", Not(users(0..1000))).filter("user", Not(users(1000..2000)));
    let overflow = past.by(&["user"]).aggregate(Op::Last).unwrap();
    assert_eq!(overflow.len(), 1, "a group of its own, whatever the query groups by");
    let labels: Vec<&str> = overflow[0].labels.keys().map(String::as_str).collect();
    assert_eq!((labels, overflow[0].buckets[0].value), (vec!["__overflow"], 3.0), "the set past them and one after");
    assert_eq!(halves + overflow[0].buckets[0].value, 2003.0, "the groups add up to the total");
}

#[test]
fn a_series_nothing_was_sent_to_for_an_hour_ends_and_a_call_begins_another_run() {
    let f = fixture();
    let stats = f.metrics();
    let jobs = stats.counter("jobs_total").open().unwrap();
    let mail = jobs.with(&[("queue", "mail")]);
    mail.add(4.0);
    jobs.with(&[("queue", "sync")]).add(1.0);
    stats.flush().unwrap();
    f.pass(HOUR);
    stats.flush().unwrap();
    assert_eq!(f.engine().totals.sets_held("jobs_total"), 1, "the set no handle holds is freed");
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    let hour = ms(HOUR);
    assert_eq!(
        samples(&f, "jobs_total", &[("queue", "mail")]),
        [(-1, 0.0), (0, 4.0), (hour, 4.0)],
        "written no longer"
    );
    // a held handle begins a run again, its zero at the write before
    mail.add(2.0);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    let (quiet, now) = (hour + ms(15 * SECOND), hour + ms(30 * SECOND));
    assert_eq!(samples(&f, "jobs_total", &[("queue", "mail")])[3..], [(quiet, 0.0), (now, 2.0)]);
    // a set whose slot was freed begins one the same way
    jobs.with(&[("queue", "sync")]).add(7.0);
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "jobs_total", &[("queue", "sync")])[3..], [(now, 0.0), (now + ms(15 * SECOND), 7.0)]);
    let grew =
        f.metrics().query().name("jobs_total").filter("queue", "mail").since(DAY).aggregate(Op::Increase).unwrap();
    assert_eq!(grew[0].buckets[0].value, 6.0);
}

#[test]
fn a_timer_writes_its_count_sum_longest_and_buckets() {
    let f = fixture();
    let stats = f.metrics();
    let latency =
        stats.timer("http_request_ms").buckets(&[5.0, 10.0, 25.0]).help("How long a request took.").open().unwrap();
    let users = latency.with(&[("route", "/users")]);
    for taken in [3, 7, 30] {
        users.record(Duration::from_millis(taken));
    }
    stats.flush().unwrap();
    let route = [("route", "/users")];
    assert_eq!(samples(&f, "http_request_ms_count", &route), [(-1, 0.0), (0, 3.0)]);
    assert_eq!(samples(&f, "http_request_ms_sum", &route), [(-1, 0.0), (0, 40.0)]);
    assert_eq!(samples(&f, "http_request_ms_max", &route), [(0, 30.0)], "a gauge, without a zero");
    let bucket = |le: &str| samples(&f, "http_request_ms_bucket", &[("route", "/users"), ("le", le)]);
    let counts: Vec<f64> = ["5", "10", "25", "+Inf"].iter().map(|le| bucket(le).last().unwrap().1).collect();
    assert_eq!(counts, [1.0, 2.0, 2.0, 3.0], "each counts what fell at most its bound");
    assert_eq!(bucket("5")[0], (-1, 0.0));
    // a write that measured nothing leaves the longest out
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "http_request_ms_max", &route).len(), 1);
    users.record(Duration::from_micros(1500));
    assert_eq!(users.measure(|| 42), 42);
    {
        let _timing = users.start();
    }
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "http_request_ms_count", &route).last().unwrap().1, 6.0);
    assert!(samples(&f, "http_request_ms_sum", &route).last().unwrap().1 >= 41.5);
    assert!(samples(&f, "http_request_ms_max", &route).last().unwrap().1 >= 1.5);
    let described = f.engine().file.read(|connection| {
        let mut select = connection
            .prepare("select name, unit, help, buckets from names where name like 'http_request_ms%' order by name")
            .unwrap();
        let rows = select.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(3)?))
        });
        Ok(rows.unwrap().map(Result::unwrap).collect::<Vec<_>>())
    });
    let described = described.unwrap();
    let with = |name: &str| described.iter().find(|row| row.0 == name).cloned().unwrap();
    assert_eq!(with("http_request_ms_sum").1, "ms");
    assert_eq!(with("http_request_ms_count").1, "", "a count is no unit");
    assert_eq!(with("http_request_ms_bucket").2.as_deref(), Some("[5,10,25]"));
}

#[test]
fn a_histogram_records_numbers_and_its_bounds_read_as_javascript_writes_them() {
    let f = fixture();
    let stats = f.metrics();
    let sizes = stats.histogram("response_bytes").unit("bytes").buckets(&[0.5, 1.0, 2.5e3]).open().unwrap();
    sizes.record(0.25);
    sizes.record(2000.0);
    sizes.record(-1.0);
    sizes.record(f64::NAN);
    stats.flush().unwrap();
    for (le, count) in [("0.5", 1.0), ("1", 1.0), ("2500", 2.0), ("+Inf", 2.0)] {
        assert_eq!(samples(&f, "response_bytes_bucket", &[("le", le)]).last().unwrap().1, count, "le {le}");
    }
    assert_eq!(samples(&f, "response_bytes_count", &[]).last().unwrap().1, 2.0, "what it refused counts nowhere");
}

#[test]
fn a_call_it_refuses_changes_nothing() {
    let f = fixture();
    let stats = f.metrics();
    let jobs = stats.counter("jobs_total").open().unwrap();
    jobs.add(2.0);
    for refused in [-1.0, f64::NAN, f64::INFINITY] {
        jobs.add(refused);
    }
    jobs.with(&[("__own", "x")]).add(100.0);
    jobs.with(&[("label", &"x".repeat(5000))]).add(100.0);
    let depth = stats.gauge("queue_depth").open().unwrap();
    depth.set(3.0);
    depth.set(f64::NAN);
    depth.add(f64::INFINITY);
    depth.inc();
    depth.dec();
    depth.dec();
    let latency = stats.timer("http_request_ms").buckets(&[5.0]).open().unwrap();
    latency.with(&[("le", "5")]).record(Duration::from_millis(1));
    stats.flush().unwrap();
    assert_eq!(samples(&f, "jobs_total", &[]).last(), Some(&(0, 2.0)));
    assert_eq!(f.value("select count(*) from series where name = (select id from names where name = 'jobs_total')"), 1);
    assert_eq!(samples(&f, "queue_depth", &[]), [(0, 2.0)]);
    assert!(samples(&f, "http_request_ms_count", &[]).is_empty(), "a timer's `le` is its buckets'");
}

#[test]
fn an_instrument_opens_against_what_the_store_and_its_file_hold() {
    let f = fixture();
    let stats = f.metrics();
    let refused = |opened: crate::Result<()>| {
        let error = opened.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        error.to_string()
    };
    stats.counter("requests_total").open().unwrap();
    let kind = refused(stats.gauge("requests_total").open().map(drop));
    assert!(kind.contains("metrics requests_total: a gauge, where it is a counter while the store is open"), "{kind}");
    f.metrics().ingest(&[Series::gauge("temperature").sample(f.now(), 21.5)]).unwrap();
    let held = refused(stats.counter("temperature").open().map(drop));
    assert!(held.contains("a counter, where the name's series are gauges"), "{held}");
    stats.timer("latency_ms").buckets(&[1.0, 2.0]).open().unwrap();
    refused(stats.timer("latency_ms").buckets(&[1.0, 3.0]).open().map(drop));
    stats.timer("latency_ms").buckets(&[1.0, 2.0]).open().unwrap();
    let writer = refused(stats.counter("latency_ms_count").open().map(drop));
    assert!(writer.contains("latency_ms_count is written by the instrument latency_ms"), "{writer}");
    stats.counter("kept_total").keep(DAY).open().unwrap();
    refused(stats.counter("kept_total").keep(2 * DAY).open().map(drop));
    stats.counter("kept_total").open().unwrap();
    refused(stats.counter("").open().map(drop));
    refused(stats.counter("helped_total").help("h".repeat(1025)).open().map(drop));
    refused(stats.counter("united_total").unit("u".repeat(33)).open().map(drop));
    refused(stats.timer("bounded_ms").buckets(&[]).open().map(drop));
    refused(stats.counter("forever_total").keep(Duration::ZERO).open().map(drop));
    stats.gauge_func("read_once", || 1.0).open().unwrap();
    refused(stats.gauge_func("read_once", || 2.0).open());
    refused(stats.gauge("read_once").open().map(drop));
}

#[test]
fn one_refused_instrument_keeps_no_other_out() {
    let f = fixture();
    let stats = f.metrics();
    let shared = stats.counter("shared").open().unwrap();
    let fine = stats.counter("fine_total").open().unwrap();
    // another writer makes the name a gauge's before the counter's first write
    f.metrics().ingest(&[Series::gauge("shared").sample(f.now() - SECOND, 1.0)]).unwrap();
    shared.inc();
    fine.inc();
    stats.flush().unwrap();
    assert_eq!(samples(&f, "fine_total", &[]).last(), Some(&(0, 1.0)));
    assert_eq!(samples(&f, "shared", &[]), [(-1000, 1.0)], "the gauge alone");
    f.pass(15 * SECOND);
    stats.flush().unwrap();
    assert_eq!(samples(&f, "fine_total", &[]).len(), 3);
}

#[test]
fn a_gauges_function_is_read_at_each_write_and_writes_nothing_when_it_fails() {
    let f = fixture();
    let stats = f.metrics();
    let open = Arc::new(AtomicU64::new(3));
    let tickets = Arc::clone(&open);
    stats.gauge_func("open_tickets", move || tickets.load(Ordering::Relaxed) as f64).unit("tickets").open().unwrap();
    let failing = Arc::new(AtomicU64::new(0));
    let calls = Arc::clone(&failing);
    stats
        .gauge_func("pool_size", move || match calls.fetch_add(1, Ordering::Relaxed) {
            0 => Ok(8.0),
            1 => Err("the pool is closed"),
            2 => Ok(f64::NAN),
            3 => panic!("a gauge's function that panics"),
            _ => Ok(9.0),
        })
        .open()
        .unwrap();
    for (seconds, tickets) in [(0, 3), (15, 5), (30, 5), (45, 7), (60, 7)] {
        open.store(tickets, Ordering::Relaxed);
        f.clock.set(UNIX_EPOCH + Duration::from_millis((START + seconds * 1000) as u64)).unwrap();
        stats.flush().unwrap();
    }
    let read: Vec<f64> = samples(&f, "open_tickets", &[]).iter().map(|(_, value)| *value).collect();
    assert_eq!(read, [3.0, 5.0, 5.0, 7.0, 7.0]);
    assert_eq!(samples(&f, "pool_size", &[]), [(0, 8.0), (60_000, 9.0)], "a failure, a NaN and a panic write nothing");
}

#[test]
fn a_timer_the_store_refuses_writes_none_of_its_series() {
    let f = fixture();
    let stats = f.metrics();
    let latency = stats.timer("latency_ms").buckets(&[10.0]).open().unwrap();
    // another writer makes one of its names a gauge's before its first write
    f.metrics().ingest(&[Series::gauge("latency_ms_sum").sample(f.now() - SECOND, 1.0)]).unwrap();
    latency.record(Duration::from_millis(3));
    stats.flush().unwrap();
    for name in ["latency_ms_count", "latency_ms_max", "latency_ms_bucket"] {
        assert_eq!(f.metrics().query().name(name).read().unwrap().len(), 0, "{name}: a timer's series go together");
    }
}

#[test]
fn calls_from_many_threads_count_once_each_while_the_store_writes() {
    let f = fixture();
    let stats = f.metrics();
    let jobs = stats.counter("jobs_total").open().unwrap();
    let latency = stats.timer("latency_ms").buckets(&[1.0, 10.0]).open().unwrap();
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let (jobs, latency) = (jobs.clone(), latency.clone());
            scope.spawn(move || {
                let queue = if thread % 2 == 0 { "mail" } else { "sync" };
                for call in 0..5000_u64 {
                    jobs.with(&[("queue", queue)]).inc();
                    latency.record(Duration::from_millis(call % 20));
                }
            });
        }
        for _ in 0..20 {
            stats.flush().unwrap();
        }
    });
    stats.flush().unwrap();
    let last = |name: &str, labels: &[(&str, &str)]| samples(&f, name, labels).last().map(|(_, value)| *value);
    assert_eq!(
        (last("jobs_total", &[("queue", "mail")]), last("jobs_total", &[("queue", "sync")])),
        (Some(20_000.0), Some(20_000.0))
    );
    assert_eq!(last("latency_ms_count", &[]), Some(40_000.0));
    assert_eq!(last("latency_ms_bucket", &[("le", "+Inf")]), Some(40_000.0));
    assert_eq!(last("latency_ms_bucket", &[("le", "1")]), Some(4000.0), "0 and 1 ms of every 20");
}
