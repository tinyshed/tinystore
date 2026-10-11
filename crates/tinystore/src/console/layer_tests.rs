use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::Level;
use tracing_subscriber::layer::{Context, SubscriberExt};

use super::*;
use crate::console::fixture::Printed;

/// An environment that says nothing, so that what the test runs in changes
/// no line.
fn silent(_: &str) -> Option<String> {
    None
}

/// What a logger of the console alone is told, printing to `printed` as JSON
/// in an environment `said` answers for.
fn told(printed: &Printed, said: fn(&str) -> Option<String>) -> Told {
    Told {
        format: Some(Format::Json),
        to: Some(Box::new(printed.clone())),
        env: Some(Environment { prefix: String::new(), lookup: said }),
        ..Told::default()
    }
}

fn layer(told: Told) -> Layer {
    Layer::new(Arc::new(Console::new("api", told)), None)
}

/// Runs `log` with `layer` as its thread's subscriber.
fn logged(layer: Layer, log: impl FnOnce()) {
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), log);
}

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SPAN: &str = "00f067aa0ba902b7";

#[test]
fn a_line_has_its_fields_its_spans_fields_as_context_and_their_trace() {
    let printed = Printed::default();
    logged(layer(told(&printed, silent)), || {
        let request = tracing::info_span!("request", request_id = 7, trace_id = TRACE, span_id = SPAN);
        let _in_request = request.enter();
        let step = tracing::info_span!("step", step = "charge");
        let _in_step = step.enter();
        tracing::info!(user = 42, name = "ann", ok = true, ratio = 0.5, error = %"card declined", "signed in");
    });
    let expected = format!(
        r#"{{"level":"INFO","stream":"api","msg":"signed in","request_id":7,"step":"charge","user":42,"name":"ann","ok":true,"ratio":0.5,"error":"card declined","trace_id":"{TRACE}","span_id":"{SPAN}"}}"#
    );
    assert_eq!(printed.untimed(), [expected]);
}

#[test]
fn a_field_recorded_on_a_span_later_joins_its_context() {
    let printed = Printed::default();
    logged(layer(told(&printed, silent)), || {
        let request = tracing::info_span!("request", user = tracing::field::Empty);
        let _in_request = request.enter();
        request.record("user", 42);
        tracing::info!("signed in");
    });
    assert_eq!(printed.untimed(), [r#"{"level":"INFO","stream":"api","msg":"signed in","user":42}"#]);
}

#[test]
fn an_events_own_trace_wins_over_its_spans_and_a_trace_that_is_no_id_is_a_field() {
    let printed = Printed::default();
    let own = "0af7651916cd43dd8448eb211c80319c";
    logged(layer(told(&printed, silent)), || {
        let request = tracing::info_span!("request", trace_id = TRACE);
        let _in_request = request.enter();
        tracing::info!(trace_id = own, "own");
        tracing::info!(trace_id = "42", "no id");
    });
    assert_eq!(
        printed.untimed(),
        [
            format!(r#"{{"level":"INFO","stream":"api","msg":"own","trace_id":"{own}"}}"#),
            format!(r#"{{"level":"INFO","stream":"api","msg":"no id","trace_id":"42","trace_id":"{TRACE}"}}"#),
        ]
    );
}

#[test]
fn a_pretty_line_reads_as_the_other_languages_print_it() {
    let printed = Printed::default();
    let told = Told { format: Some(Format::Pretty), time: Time::Off, ..told(&printed, silent) };
    logged(layer(told), || {
        tracing::warn!(route = "/notes", ms = 1200, "slow request");
        tracing::debug!(key = "a b", "cache miss");
    });
    assert_eq!(
        printed.lines(),
        ["WARN  api  slow request  route=/notes ms=1200", r#"DEBUG api  cache miss  key="a b""#]
    );
}

#[test]
fn a_line_under_the_level_is_not_printed_nor_a_span_under_it_its_context() {
    let printed = Printed::default();
    logged(layer(Told { level: Some(Level::WARN), ..told(&printed, silent) }), || {
        let request = tracing::warn_span!("request", request_id = 7);
        let _in_request = request.enter();
        let step = tracing::debug_span!("step", step = 1);
        let _in_step = step.enter();
        tracing::info!("hidden");
        tracing::error!("shown");
    });
    assert_eq!(printed.untimed(), [r#"{"level":"ERROR","stream":"api","msg":"shown","request_id":7}"#]);
}

/// Counts the events it sees.
struct Counting(Arc<AtomicUsize>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Counting {
    fn on_event(&self, _: &tracing::Event<'_>, _: Context<'_, S>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn the_subscribers_other_layers_still_see_what_a_loggers_level_hides() {
    let (printed, seen) = (Printed::default(), Arc::new(AtomicUsize::new(0)));
    let logger = layer(Told { level: Some(Level::ERROR), ..told(&printed, silent) });
    let subscriber = tracing_subscriber::registry().with(logger).with(Counting(Arc::clone(&seen)));
    tracing::subscriber::with_default(subscriber, || tracing::info!("hidden here"));
    assert_eq!((printed.lines().len(), seen.load(Ordering::SeqCst)), (0, 1));
}

#[test]
fn two_loggers_in_one_subscriber_each_print_a_spans_fields_once() {
    let (first, second) = (Printed::default(), Printed::default());
    let subscriber =
        tracing_subscriber::registry().with(layer(told(&first, silent))).with(layer(told(&second, silent)));
    tracing::subscriber::with_default(subscriber, || {
        let request = tracing::info_span!("request", user = tracing::field::Empty);
        let _in_request = request.enter();
        request.record("user", 42);
        tracing::info!("signed in");
    });
    let expected = [r#"{"level":"INFO","stream":"api","msg":"signed in","user":42}"#];
    assert_eq!(first.untimed(), expected);
    assert_eq!(second.untimed(), expected);
}

#[test]
fn a_secret_is_hidden_and_replace_changes_a_field_before_it_is() {
    let printed = Printed::default();
    let replace: Replace = Arc::new(|key: &str, json: &str| match key {
        "card" => Some("****".to_owned()),
        "dropped" => None,
        "nested" => Some(r#"{"token":"t"}"#.to_owned()),
        _ => Some(json.to_owned()),
    });
    let told = Told {
        redact: vec!["password".to_owned(), "token".to_owned()],
        replace: Some(replace),
        ..told(&printed, silent)
    };
    logged(layer(told), || {
        tracing::info!(
            password = "hunter2",
            card = 4242,
            dropped = 1,
            nested = 0,
            url = "postgres://ann:pw@db/app",
            "paid"
        );
    });
    assert_eq!(
        printed.untimed(),
        [
            r#"{"level":"INFO","stream":"api","msg":"paid","password":"[redacted]","card":"****","nested":{"token":"[redacted]"},"url":"postgres://ann:[redacted]@db/app"}"#
        ]
    );
}

#[test]
fn a_source_says_where_the_line_was_logged() {
    let printed = Printed::default();
    let told = Told { source: true, format: Some(Format::Pretty), time: Time::Off, ..told(&printed, silent) };
    let line = line!() + 1;
    logged(layer(told), || tracing::info!("here"));
    assert_eq!(printed.lines(), [format!("INFO  api  here  source=console/layer_tests.rs:{line}")]);
}

fn level_said(name: &str) -> Option<String> {
    (name == "LOG_LEVEL").then(|| "error".to_owned())
}

fn json_said(name: &str) -> Option<String> {
    (name == "LOG_FORMAT").then(|| "json".to_owned())
}

#[test]
fn the_environment_wins_over_the_code_and_never_turns_a_console_on() {
    let printed = Printed::default();
    logged(layer(Told { level: Some(Level::DEBUG), ..told(&printed, level_said) }), || {
        tracing::warn!("hidden");
        tracing::error!("shown");
    });
    assert_eq!(printed.untimed(), [r#"{"level":"ERROR","stream":"api","msg":"shown"}"#]);

    let printed = Printed::default();
    logged(layer(Told { format: Some(Format::Pretty), time: Time::Off, ..told(&printed, json_said) }), || {
        tracing::info!("as json");
    });
    assert_eq!(printed.untimed(), [r#"{"level":"INFO","stream":"api","msg":"as json"}"#]);

    let printed = Printed::default();
    logged(layer(Told { format: Some(Format::Off), ..told(&printed, json_said) }), || tracing::info!("off"));
    assert!(printed.lines().is_empty());
}

fn loud_said(name: &str) -> Option<String> {
    (name == "APP_LOG_LEVEL").then(|| "loud-in-a-layer-test".to_owned())
}

#[test]
fn a_variable_none_can_mean_is_said_once_and_ignored() {
    let printed = Printed::default();
    let told =
        || Told { env: Some(Environment { prefix: "APP".to_owned(), lookup: loud_said }), ..told(&printed, silent) };
    logged(layer(told()), || tracing::debug!("kept"));
    logged(layer(told()), || {});
    let said = r#"{"level":"WARN","stream":"tinystore","msg":"APP_LOG_LEVEL is ignored","value":"loud-in-a-layer-test","expected":"debug, info, warn or error"}"#;
    assert_eq!(printed.untimed(), [said, r#"{"level":"DEBUG","stream":"api","msg":"kept"}"#]);
}

#[test]
fn the_stores_own_lines_print_from_info_or_the_loggers_level_under_its_stream() {
    let printed = Printed::default();
    logged(layer(told(&printed, silent)), || {
        tracing::debug!(target: "tinystore", "a flush");
        tracing::warn!(target: "tinystore", work = "records: loggers", "background work failed");
    });
    let failed = r#"{"level":"WARN","stream":"tinystore","msg":"background work failed","work":"records: loggers"}"#;
    assert_eq!(printed.untimed(), [failed]);

    let printed = Printed::default();
    logged(layer(Told { level: Some(Level::DEBUG), ..told(&printed, silent) }), || {
        tracing::debug!(target: "tinystore", "a flush");
    });
    assert_eq!(printed.untimed(), [r#"{"level":"DEBUG","stream":"tinystore","msg":"a flush"}"#]);
}

#[test]
fn an_event_has_its_name_its_fields_and_the_current_spans_context() {
    let printed = Printed::default();
    let logger = layer(told(&printed, silent));
    let subscriber = tracing_subscriber::registry().with(logger.clone());
    tracing::subscriber::with_default(subscriber, || {
        let request = tracing::info_span!("request", request_id = 7);
        let _in_request = request.enter();
        logger.event("signup", &serde_json::json!({ "plan": "pro" }));
        logger.event("", &serde_json::json!({ "plan": "pro" }));
        logger.event("signup", &[1, 2]);
    });
    assert_eq!(printed.untimed(), [r#"{"stream":"api","event":"signup","request_id":7,"plan":"pro"}"#]);
    assert_eq!(logger.dropped(), 2, "an event without a name, and fields that are no object");
}

#[test]
fn a_float_is_spelled_as_gos_loggers_spell_it() {
    let spelled = [
        (0.5, "0.5"),
        (100.0, "100"),
        (-0.0, "-0"),
        (123_456.0, "123456"),
        (1_234_567.0, "1.234567e+06"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1e21, "1e+21"),
        (f64::NAN, r#""NaN""#),
        (f64::INFINITY, r#""+Inf""#),
        (f64::NEG_INFINITY, r#""-Inf""#),
    ];
    for (value, expected) in spelled {
        assert_eq!(float(value), expected, "{value}");
    }
}

fn forced(name: &str) -> Option<String> {
    (name == "FORCE_COLOR").then(|| "1".to_owned())
}

#[test]
fn a_logger_told_where_to_print_prints_json_there_and_force_color_makes_it_pretty() {
    let printed = Printed::default();
    logged(layer(Told { format: None, ..told(&printed, silent) }), || tracing::info!("no terminal"));
    assert_eq!(printed.untimed(), [r#"{"level":"INFO","stream":"api","msg":"no terminal"}"#]);

    let printed = Printed::default();
    logged(layer(Told { format: None, time: Time::Off, ..told(&printed, forced) }), || tracing::info!("forced"));
    assert_eq!(printed.lines(), ["\u{1b}[32mINFO\u{1b}[0m  \u{1b}[36mapi\u{1b}[0m  forced"]);
}

#[test]
fn the_lines_of_many_threads_never_interleave() {
    let printed = Printed::default();
    let logger = layer(told(&printed, silent));
    let threads: Vec<_> = (0..8)
        .map(|thread| {
            let logger = logger.clone();
            std::thread::spawn(move || {
                let text = "x".repeat(100);
                logged(logger, || {
                    (0..200).for_each(|line| tracing::info!(thread, line, text = text.as_str(), "a line"))
                });
            })
        })
        .collect();
    threads.into_iter().for_each(|thread| thread.join().unwrap());
    let lines = printed.lines();
    assert_eq!(lines.len(), 1600);
    assert!(lines.iter().all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()), "every line whole");
}
