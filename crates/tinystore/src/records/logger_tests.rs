use std::time::{Duration, Instant};

use serde_json::json;
use tracing_subscriber::layer::SubscriberExt;

use super::super::event::Level;
use super::super::fixture::{DAY, Fixture, fixture, names};
use super::*;
use crate::console::Format;
use crate::console::fixture::Printed;
use crate::{ErrorKind, Options};

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SPAN: &str = "00f067aa0ba902b7";
const MINUTE: Duration = Duration::from_secs(60);

/// A logger of the stream `api` printing JSON to `printed`, reading no
/// variable of the environment the test runs in.
fn logger<'s>(f: &'s Fixture, printed: &Printed) -> LoggerBuilder<'s> {
    f.store.logger("api").console(Format::Json).no_env().to(printed.clone())
}

/// Runs `log` with `layer` as its thread's subscriber.
fn logged(layer: &Layer, log: impl FnOnce()) {
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer.clone()), log);
}

#[test]
fn a_line_is_printed_as_it_is_logged_and_kept_once_its_logger_flushes() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).layer().unwrap();
    logged(&log, || {
        let request = tracing::info_span!("request", request_id = 7, trace_id = TRACE, span_id = SPAN);
        let _in_request = request.enter();
        tracing::info!(user = 42, "signed in");
        tracing::warn!(ms = 1200, "slow request");
    });
    assert_eq!(printed.lines().len(), 2);
    let api = f.events("api");
    assert!(api.list().page().unwrap().events.is_empty(), "held till a flush");
    log.flush();
    let events = api.list().page().unwrap().events;
    assert_eq!(names(&events), ["log", "log"]);
    let signed_in = &events[0];
    assert_eq!((signed_in.at, signed_in.level), (f.now(), Some(Level::INFO)));
    assert_eq!(signed_in.message.as_deref(), Some("signed in"));
    assert_eq!((signed_in.fields.json("user"), signed_in.context.json("request_id")), (Some("42"), Some("7")));
    assert_eq!(signed_in.trace_id.map(|id| id.to_string()).as_deref(), Some(TRACE));
    assert_eq!(signed_in.span_id.map(|id| id.to_string()).as_deref(), Some(SPAN));
    assert_eq!(events[1].level, Some(Level::WARN));
}

// contract: LOG-01
#[test]
fn a_full_buffer_drops_a_line_counts_it_and_a_flush_says_so_once_in_ten_minutes() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).buffer(4).layer().unwrap();
    let six_lines = || (0..6).for_each(|line| tracing::info!(line, "a line"));
    let said = || -> Vec<String> { printed.untimed().into_iter().filter(|line| line.contains("dropped")).collect() };
    let notice =
        r#"{"level":"WARN","stream":"tinystore","msg":"log lines dropped","logger":"api","dropped":2,"buffer":4}"#;
    logged(&log, six_lines);
    assert_eq!(log.dropped(), 2);
    log.flush();
    logged(&log, six_lines);
    log.flush();
    assert_eq!(said(), [notice], "the next two are unsaid within ten minutes");
    f.pass(10 * MINUTE);
    log.flush();
    assert_eq!(said(), [notice, notice], "and said ten minutes on");
    assert_eq!((log.dropped(), f.events("api").list().page().unwrap().events.len()), (4, 8));
}

// contract: LOG-02
#[test]
fn a_store_that_closes_keeps_what_its_loggers_held_and_drops_a_line_after() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).console(Format::Off).layer().unwrap();
    logged(&log, || tracing::info!("before the close"));
    let f = f.reopen();
    logged(&log, || tracing::info!("after the close"));
    assert_eq!(log.dropped(), 1);
    let events = f.events("api").list().page().unwrap().events;
    let messages: Vec<Option<&str>> = events.iter().map(|event| event.message.as_deref()).collect();
    assert_eq!(messages, [Some("before the close")]);
    assert!(printed.lines().is_empty(), "a console turned off prints nothing, and the lines are kept");
}

// contract: LOG-06
#[test]
fn the_stores_own_lines_are_printed_and_never_kept() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).layer().unwrap();
    logged(&log, || tracing::warn!(target: "tinystore", work = "kv: maintenance", "background work failed"));
    log.flush();
    assert_eq!(printed.lines().len(), 1);
    assert!(f.events("api").list().page().unwrap().events.is_empty());
}

#[test]
fn an_event_is_kept_with_its_name_and_neither_a_level_nor_a_message() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).layer().unwrap();
    log.event("signup", &json!({ "plan": "pro" }));
    log.flush();
    let events = f.events("api").list().page().unwrap().events;
    assert_eq!(names(&events), ["signup"]);
    assert_eq!((events[0].level, events[0].message.as_deref()), (None, None));
    assert_eq!(events[0].fields.json("plan"), Some(r#""pro""#));
}

#[test]
fn a_line_the_file_refuses_is_dropped_and_counted_and_the_rest_are_kept() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).console(Format::Off).layer().unwrap();
    let big = "x".repeat(300 << 10);
    logged(&log, || {
        tracing::info!(big = big.as_str(), "past what an event weighs");
        tracing::info!("kept");
    });
    log.flush();
    assert_eq!(log.dropped(), 1);
    let events = f.events("api").list().page().unwrap().events;
    assert_eq!(events.iter().map(|event| event.message.as_deref()).collect::<Vec<_>>(), [Some("kept")]);
}

#[test]
fn a_logger_opens_with_a_streams_name_a_buffer_of_lines_and_the_streams_keep() {
    let f = fixture();
    let refused = |builder: LoggerBuilder<'_>| builder.no_env().layer().unwrap_err();
    assert_eq!(refused(f.store.logger("Bad Name")).kind(), ErrorKind::Invalid);
    let no_buffer = refused(f.store.logger("api").buffer(0));
    assert_eq!(no_buffer.to_string(), "logger api: a buffer of no lines: invalid");
    f.store.logger("api").keep(30 * DAY).console(Format::Off).no_env().layer().unwrap();
    assert_eq!(refused(f.store.logger("api").keep(DAY)).kind(), ErrorKind::Invalid, "another keep while it is open");
}

#[test]
fn half_a_buffer_wakes_the_flush_before_its_second() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let log = store.logger("api").buffer(2).console(Format::Off).no_env().layer().unwrap();
    let api = store.events("api").open().unwrap();
    let logged_at = Instant::now();
    logged(&log, || tracing::info!("one of two"));
    while api.list().page().unwrap().events.is_empty() {
        // the flush runs every second: far sooner, it was woken
        assert!(logged_at.elapsed() < Duration::from_millis(800), "not written before the flush's second");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// contract: LOG-04
#[test]
fn a_secret_is_hidden_where_it_prints_and_where_it_is_kept() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).redact(crate::console::SECRETS).layer().unwrap();
    logged(&log, || {
        let request = tracing::info_span!("request", api_key = "k-123");
        let _in_request = request.enter();
        tracing::info!(password = "hunter2", user = 42, "signed in");
    });
    log.flush();
    let events = f.events("api").list().page().unwrap().events;
    let hidden = Some(r#""[redacted]""#);
    assert_eq!((events[0].fields.json("password"), events[0].context.json("api_key")), (hidden, hidden));
    assert_eq!(events[0].fields.json("user"), Some("42"));
    let printed = printed.untimed();
    assert!(printed[0].contains(r#""api_key":"[redacted]""#) && printed[0].contains(r#""password":"[redacted]""#));
}

// contract: LOG-02
#[test]
fn a_logger_dropped_with_lines_waiting_loses_none_of_them() {
    let (f, printed) = (fixture(), Printed::default());
    let log = logger(&f, &printed).console(Format::Off).layer().unwrap();
    logged(&log, || tracing::info!("held when its logger went"));
    drop(log);
    f.records().flush_held(false).unwrap();
    let events = f.events("api").list().page().unwrap().events;
    assert_eq!(
        events.iter().map(|event| event.message.as_deref()).collect::<Vec<_>>(),
        [Some("held when its logger went")]
    );
}
