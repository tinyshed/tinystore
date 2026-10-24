//! A store's logger and the config `log`: its options declared through the
//! config, the environment's variables a layer over them, and a change of
//! its level taken by the logger at once.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;

use super::follow;
use crate::config::{Config, Live, Watching};
use crate::console::fixture::Printed;
use crate::console::{Console, Declared, Environment, Format, Layer, Told};
use crate::records::Levels;
use crate::{ErrorKind, Options, Store};

/// The config `log` as a program that changes the level declares it.
#[derive(Clone, Serialize, Deserialize)]
struct Log {
    level: Live<String>,
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    (dir, store)
}

fn silent(_: &str) -> Option<String> {
    None
}

/// A console of the stream `api` printing JSON to `printed`, told `level`,
/// in an environment `lookup` answers for.
fn console(printed: &Printed, level: Option<Level>, lookup: fn(&str) -> Option<String>) -> Arc<Console> {
    let env = Some(Environment { prefix: String::new(), lookup });
    let told = Told { level, format: Some(Format::Json), to: Some(Box::new(printed.clone())), env, ..Told::default() };
    Arc::new(Console::new("api", told))
}

/// Logs a line at debug, then one at each level above it.
fn four_lines(console: &Arc<Console>) {
    let layer = Layer::new(Arc::clone(console), None);
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
        tracing::debug!("debug");
        tracing::info!("info");
        tracing::warn!("warn");
        tracing::error!("error");
    });
}

/// The messages printed, in order.
fn messages(printed: &Printed) -> Vec<String> {
    let message = |line: String| serde_json::from_str::<Value>(&line).unwrap()["msg"].as_str().unwrap().to_owned();
    printed.untimed().into_iter().map(message).collect()
}

fn explained(following: &Arc<dyn std::any::Any + Send + Sync>, path: &str) -> crate::config::Line {
    let (config, _) = following.downcast_ref::<(Config<Value>, Mutex<Watching>)>().unwrap();
    config.explain().settings.into_iter().find(|line| line.path == path).unwrap()
}

// contract: LOG-07
#[test]
fn a_change_of_the_level_reaches_the_logger_at_once_and_a_reset_gives_its_own_back() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let console = console(&printed, Some(Level::WARN), silent);
    let _following = follow(&store, &console).unwrap();
    let log = store.config("log", Log { level: Live::new("info".to_owned()) }).open().unwrap();
    four_lines(&console);
    log.update(|log| *log.level = "debug".to_owned()).unwrap();
    four_lines(&console);
    log.reset("level").unwrap();
    four_lines(&console);
    assert_eq!(messages(&printed), ["warn", "error", "debug", "info", "warn", "error", "warn", "error"]);
}

// contract: LOG-07
#[test]
fn a_stores_logger_follows_the_config_log_while_a_clone_of_it_lives() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let built = store.logger("api").level(Level::WARN).console(Format::Json).no_env().to(printed.clone());
    let log = built.layer().unwrap();
    let settings = store.config("log", Log { level: Live::new("info".to_owned()) }).open().unwrap();
    settings.update(|log| *log.level = "debug".to_owned()).unwrap();
    let subscriber = tracing_subscriber::registry().with(log.clone());
    drop(log);
    tracing::subscriber::with_default(subscriber, || tracing::debug!("followed"));
    assert_eq!(messages(&printed), ["followed"]);
}

// contract: LOG-07
#[test]
fn a_change_of_the_level_moves_what_is_kept_and_leaves_a_console_part_as_the_code_gave_it() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let levels = Levels { store: Some(Level::ERROR), console: Some(Level::WARN) };
    let built = store.logger("api").level(levels).console(Format::Json).no_env();
    let log = built.to(printed.clone()).layer().unwrap();
    let settings = store.config("log", Log { level: Live::new("info".to_owned()) }).open().unwrap();
    settings.update(|log| *log.level = "debug".to_owned()).unwrap();
    let subscriber = tracing_subscriber::registry().with(log.clone());
    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!("debug");
        tracing::warn!("warn");
    });
    log.flush();
    assert_eq!(messages(&printed), ["warn"]);
    let kept = store.events("api").open().unwrap().list().page().unwrap().events;
    let kept: Vec<Option<&str>> = kept.iter().map(|event| event.message.as_deref()).collect();
    assert_eq!(kept, [Some("debug"), Some("warn")]);
}

// contract: LOG-07
#[test]
fn a_level_no_logger_means_is_refused_while_a_logger_has_the_config_open() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let _following = follow(&store, &console(&printed, None, silent)).unwrap();
    let log = store.config("log", Log { level: Live::new("info".to_owned()) }).open().unwrap();
    let error = log.update(|log| *log.level = "verbose".to_owned()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let refused = r#"level: "verbose" is not one of trace, debug, info, warn, error, as another handle"#;
    assert!(error.to_string().contains(refused), "{error}");
}

// contract: LOG-07
#[test]
fn a_kept_change_is_the_level_of_a_logger_that_opens_after_it() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let log = store.config("log", Log { level: Live::new("info".to_owned()) }).open().unwrap();
    log.update(|log| *log.level = "error".to_owned()).unwrap();
    let console = console(&printed, Some(Level::DEBUG), silent);
    let following = follow(&store, &console).unwrap();
    four_lines(&console);
    assert_eq!(messages(&printed), ["error"]);
    let level = explained(&following, "level");
    assert!(level.from.starts_with("a change, version "), "{level:?}");
    assert_eq!(level.over, [r#""debug" from the declaration"#]);
}

fn warning_said(name: &str) -> Option<String> {
    (name == "LOG_LEVEL").then(|| "WARNING".to_owned())
}

// contract: LOG-07
#[test]
fn the_environment_goes_over_the_code_as_a_layer_that_explain_names() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let console = console(&printed, Some(Level::DEBUG), warning_said);
    let following = follow(&store, &console).unwrap();
    four_lines(&console);
    assert_eq!(messages(&printed), ["warn", "error"]);
    let level = explained(&following, "level");
    assert_eq!((level.value.as_str(), level.from.as_str()), (r#""warn""#, "LOG_LEVEL"));
    assert_eq!(level.over, [r#""debug" from the declaration"#]);
    let format = explained(&following, "format");
    assert_eq!((format.value.as_str(), format.from.as_str()), (r#""json""#, "the declaration"));
}

fn loud_said(name: &str) -> Option<String> {
    (name == "LOG_LEVEL").then(|| "loud-in-a-log-config-test".to_owned())
}

#[test]
fn a_variable_no_logger_means_is_said_once_and_the_config_opens_without_it() {
    let ((_dir, store), printed) = (store(), Printed::default());
    let console = console(&printed, Some(Level::INFO), loud_said);
    let said = r#"{"level":"WARN","stream":"tinystore","msg":"LOG_LEVEL is ignored","value":"loud-in-a-log-config-test","expected":"trace, debug, info, warn or error"}"#;
    assert_eq!(printed.untimed(), [said]);
    let following = follow(&store, &console).unwrap();
    four_lines(&console);
    assert_eq!(messages(&printed), ["LOG_LEVEL is ignored", "info", "warn", "error"]);
    assert_eq!(explained(&following, "level").from, "the declaration");
}

/// The format, and `LOG_CONSOLE_LEVEL`, which the config `log` has no setting for.
fn json_said(name: &str) -> Option<String> {
    match name {
        "APP_LOG_FORMAT" => Some("json".to_owned()),
        "APP_LOG_CONSOLE_LEVEL" => Some("warn".to_owned()),
        _ => None,
    }
}

// contract: LOG-07
#[test]
fn a_logger_declares_its_codes_options_and_the_variables_it_read_and_off_stays_off() {
    let printed = Printed::default();
    let declared = |told: Told| Console::new("api", told).declared().clone();
    let env = Some(Environment { prefix: "APP".to_owned(), lookup: json_said });
    let told = || Told { to: Some(Box::new(printed.clone())), env: env.clone(), ..Told::default() };
    let variables = vec![("APP_LOG_FORMAT".to_owned(), "json".to_owned())];
    assert_eq!(
        declared(Told { format: Some(Format::Pretty), ..told() }),
        Declared {
            level: "trace",
            format: "pretty",
            time: "clock",
            env: Some(("APP_LOG".to_owned(), variables.clone())),
        },
    );
    let off = declared(Told { format: Some(Format::Off), level: Some(Level::ERROR), ..told() });
    assert_eq!((off.level, off.format, off.env), ("error", "off", Some(("APP_LOG".to_owned(), Vec::new()))));
    let chosen = declared(told());
    assert_eq!((chosen.format, chosen.env), ("json", Some(("APP_LOG".to_owned(), variables))), "no terminal");
    assert_eq!(declared(Told { env: None, ..told() }).env, None);
}
