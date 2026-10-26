use std::time::{Duration, UNIX_EPOCH};

use crate::metrics::fixture::{Fixture, fixture};

/// The last value written of the series of `name` and `labels`.
fn last(f: &Fixture, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    let (from, to) = (UNIX_EPOCH + Duration::from_secs(1_700_000_000), UNIX_EPOCH + Duration::from_secs(1_900_000_000));
    let mut query = f.metrics().query().name(name).from(from).to(to);
    for (label, value) in labels {
        query = query.filter(*label, *value);
    }
    let read = query.read().unwrap().series;
    let series = read.iter().find(|series| series.labels().len() == labels.len())?;
    series.samples().last().map(|sample| sample.value)
}

#[test]
fn the_metrics_crates_macros_count_into_the_stores_instruments() {
    let f = fixture();
    let stats = f.metrics();
    let recorder = stats.recorder().buckets("latency_ms", &[10.0, 100.0]);
    ::metrics::with_local_recorder(&recorder, || {
        ::metrics::describe_counter!("requests_total", "Requests served.");
        ::metrics::describe_histogram!("latency_ms", ::metrics::Unit::Milliseconds, "How long a request took.");
        ::metrics::counter!("requests_total", "route" => "/notes").increment(3);
        ::metrics::counter!("requests_total", "route" => "/notes").increment(2);
        ::metrics::gauge!("queue_depth").set(4.0);
        ::metrics::gauge!("queue_depth").increment(1.5);
        ::metrics::gauge!("queue_depth").decrement(0.5);
        ::metrics::histogram!("latency_ms", "route" => "/users").record(42.0);
        ::metrics::histogram!("latency_ms", "route" => "/users").record(420.0);
        // a name opened as a gauge counts nowhere as a counter
        ::metrics::counter!("queue_depth").increment(1);
        // a total the program keeps itself, which starts again from a smaller one
        for total in [10, 15, 4] {
            ::metrics::counter!("kept_total").absolute(total);
        }
        ::metrics::gauge!("queue_depth").set(f64::NAN);
        // and the gauge counts on after it
        ::metrics::gauge!("queue_depth").increment(1.0);
    });
    stats.flush().unwrap();
    assert_eq!(last(&f, "requests_total", &[("route", "/notes")]), Some(5.0));
    assert_eq!(last(&f, "queue_depth", &[]), Some(6.0), "a NaN set changes nothing, nor a counter's call");
    assert_eq!(last(&f, "kept_total", &[]), Some(19.0), "10, 5 more, and 4 after it started again");
    let route = [("route", "/users")];
    assert_eq!((last(&f, "latency_ms_count", &route), last(&f, "latency_ms_sum", &route)), (Some(2.0), Some(462.0)));
    let le = |bound: &str| last(&f, "latency_ms_bucket", &[("route", "/users"), ("le", bound)]);
    assert_eq!((le("10"), le("100"), le("+Inf")), (Some(0.0), Some(1.0), Some(2.0)));
    let described = f.engine().file.read(|connection| {
        let unit = |name: &str| -> (String, String) {
            connection
                .query_row("select unit, help from names where name = ?1", [name], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
        };
        Ok((unit("requests_total"), unit("latency_ms_sum")))
    });
    let described = described.unwrap();
    assert_eq!(described.0, (String::new(), "Requests served.".to_owned()));
    assert_eq!(described.1, ("ms".to_owned(), "How long a request took.".to_owned()));
}

#[test]
fn a_description_after_a_name_is_open_is_written_at_the_next_write() {
    let f = fixture();
    let stats = f.metrics();
    let recorder = stats.recorder();
    ::metrics::with_local_recorder(&recorder, || {
        ::metrics::gauge!("open_tickets").set(3.0);
        ::metrics::describe_gauge!("open_tickets", ::metrics::Unit::Count, "Tickets not closed yet.");
    });
    stats.flush().unwrap();
    let help = f.engine().file.read(|connection| {
        Ok(connection
            .query_row("select help from names where name = 'open_tickets'", [], |row| row.get::<_, String>(0))
            .unwrap())
    });
    assert_eq!(help.unwrap(), "Tickets not closed yet.");
    assert_eq!(last(&f, "open_tickets", &[]), Some(3.0));
}
