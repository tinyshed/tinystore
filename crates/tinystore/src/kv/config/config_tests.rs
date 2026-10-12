//! A config as a program opens, reads, changes and watches one.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::*;
use crate::kv::fixture::{Fixture, fixture};
use crate::{Error, ErrorKind};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    port: u16,
    origins: Vec<String>,
    timeout: Duration,
    db: Db,
    limits: Limits,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Db {
    url: Secret<String>,
    pool: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Limits {
    rps: Live<u32>,
    burst: Live<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            port: 8080,
            origins: vec!["localhost".to_owned()],
            timeout: Duration::from_secs(30),
            db: Db::default(),
            limits: Limits::default(),
        }
    }
}

impl Default for Db {
    fn default() -> Self {
        Db { url: Secret::new("postgres://local".to_owned()), pool: 10 }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits { rps: Live::new(100), burst: Live::new(10) }
    }
}

fn env(variables: &[(&str, &str)]) -> Env {
    Env::prefix("APP").from(variables.iter().map(|&(name, value)| (name, value)))
}

fn file(f: &Fixture, name: &str, text: &str) -> std::path::PathBuf {
    let path = f.dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn open(f: &Fixture, variables: &[(&str, &str)]) -> Config<Settings> {
    f.store.config("app", Settings::default()).env(env(variables)).open().unwrap()
}

fn refused<T>(result: Result<T>) -> Error {
    match result {
        Ok(_) => panic!("refused, not opened"),
        Err(error) => error,
    }
}

fn line(config: &Config<Settings>, path: &str) -> Line {
    config.explain().settings.into_iter().find(|line| line.path == path).unwrap()
}

#[test]
fn a_config_is_its_declaration_then_each_layer_in_the_order_written() {
    let f = fixture();
    let yaml = file(&f, "app.yaml", "port: 8000\nlimits:\n  rps: 50\n");
    let config =
        f.store.config("app", Settings::default()).file(yaml.clone()).env(env(&[("APP_PORT", "9000")])).open().unwrap();
    let settings = config.get();
    assert_eq!((settings.port, *settings.limits.rps, settings.db.pool), (9000, 50, 10));
    let port = line(&config, "port");
    let shown = yaml.display().to_string();
    assert_eq!((port.value.as_str(), port.from.as_str()), ("9000", "APP_PORT"));
    assert_eq!(port.over, [format!("8000 from {shown}"), "8080 from the declaration".to_owned()]);
    let swapped =
        f.store.config("swapped", Settings::default()).env(env(&[("APP_PORT", "9000")])).file(yaml).open().unwrap();
    assert_eq!(swapped.get().port, 8000, "a file written after the environment goes over it");
}

#[test]
fn a_variable_is_read_as_its_settings_kind_and_every_bad_one_is_said_at_once() {
    let f = fixture();
    let config = open(&f, &[("APP_ORIGINS", "a.com, b.com"), ("APP_TIMEOUT", "1m30s"), ("APP_LIMITS_RPS", "70")]);
    let settings = config.get();
    assert_eq!(settings.origins, ["a.com", "b.com"]);
    assert_eq!((settings.timeout, *settings.limits.rps), (Duration::from_secs(90), 70));
    assert_eq!(line(&config, "timeout").value, "1m30s");
    let error = refused(
        f.store
            .config("bad", Settings::default())
            .env(env(&[("APP_PORT", "eighty"), ("APP_TIMEOUT", "30 seconds")]))
            .open(),
    );
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let message = error.to_string();
    assert!(message.contains(r#"APP_PORT: "eighty" is not a number"#), "{message}");
    assert!(message.contains(r#"APP_TIMEOUT: "30 seconds" is not a span, as 30s or 1h30m"#), "{message}");
}

#[test]
fn a_value_past_what_its_type_holds_is_named_by_the_layer_that_gave_it() {
    let f = fixture();
    let error = refused(f.store.config("app", Settings::default()).env(env(&[("APP_PORT", "70000")])).open());
    assert!(error.to_string().contains("APP_PORT: invalid value: integer `70000`, expected u16"), "{error}");
    let yaml = file(&f, "app.yaml", "port: 70000\n");
    let error = refused(f.store.config("app", Settings::default()).file(yaml).open());
    assert!(error.to_string().contains("app.yaml: port: invalid value"), "{error}");
}

#[test]
fn a_file_key_no_setting_has_and_a_missing_file_are_refused_but_an_optional_file_may_be_absent() {
    let f = fixture();
    let yaml = file(&f, "app.yaml", "prot: 1\n");
    let error = refused(f.store.config("app", Settings::default()).file(yaml).open());
    assert!(error.to_string().contains("app.yaml: prot: no such setting"), "{error}");
    let missing = f.dir.path().join("missing.toml");
    let error = refused(f.store.config("app", Settings::default()).file(missing.clone()).open());
    assert!(error.to_string().contains("missing.toml: no such file"), "{error}");
    let config = f.store.config("app", Settings::default()).file(File::optional(missing.clone())).open().unwrap();
    assert_eq!(config.explain().notes, [format!("{}: absent", missing.display())]);
}

#[test]
fn name_file_gives_a_setting_its_files_text_and_both_set_is_refused() {
    let f = fixture();
    let secret = file(&f, "db", "postgres://secret\n");
    let path = secret.display().to_string();
    let config = open(&f, &[("APP_DB_URL_FILE", &path)]);
    assert_eq!(config.get().db.url.reveal(), "postgres://secret");
    let url = line(&config, "db.url");
    assert_eq!((url.value.as_str(), url.from), ("***", format!("APP_DB_URL_FILE, {path}")));
    let error = refused(
        f.store.config("both", Settings::default()).env(env(&[("APP_DB_URL_FILE", &path), ("APP_DB_URL", "x")])).open(),
    );
    assert!(error.to_string().contains("APP_DB_URL and APP_DB_URL_FILE are both set"), "{error}");
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Deploy {
    region: Required<String>,
    token: Secret<String>,
}

#[test]
fn a_required_setting_is_given_by_a_layer_or_the_config_does_not_open_naming_what_would() {
    let f = fixture();
    let yaml = f.dir.path().join("deploy.yaml");
    let opening = |variables: &[(&str, &str)]| {
        f.store.config("deploy", Deploy::default()).file(File::optional(yaml.clone())).env(env(variables)).open()
    };
    let error = refused(opening(&[("APP_REGION", "")]));
    let shown = yaml.display();
    assert!(
        error.to_string().contains(&format!("region is required: region in {shown}, or set APP_REGION")),
        "{error}"
    );
    assert!(error.to_string().contains(&format!("token is required: token in {shown}, or set APP_TOKEN")), "{error}");
    let config = opening(&[("APP_REGION", "eu"), ("APP_TOKEN", "t")]).unwrap();
    assert_eq!((config.get().region.as_str(), config.get().token.reveal().as_str()), ("eu", "t"));
}

#[test]
fn a_secret_prints_as_stars_wherever_it_goes_and_no_change_keeps_one() {
    let f = fixture();
    let config = open(&f, &[("APP_DB_URL", "postgres://user:password@host")]);
    let settings = config.get();
    for printed in [format!("{settings:?}"), serde_json::to_string(&*settings).unwrap(), config.explain().to_string()] {
        assert!(!printed.contains("password") && printed.contains("***"), "{printed}");
    }
    let error = refused(config.update(|settings| settings.db.url = Secret::new("other".to_owned())));
    assert!(error.to_string().contains("db.url is a secret, which only a layer gives"), "{error}");
    let kept: String = crate::kv::engine::Kv::of(&f.store)
        .unwrap()
        .file()
        .read(|connection| {
            Ok(connection.query_row("select declaration from _tinystore_kv_configs", [], |row| row.get(0)).unwrap())
        })
        .unwrap();
    assert!(!kept.contains("postgres://local"), "a secret's default is not kept: {kept}");
    let yaml = file(
        &f,
        "app.yaml",
        "db:
  url: 12345
",
    );
    let error = refused(f.store.config("other", Settings::default()).file(yaml).open());
    assert!(error.to_string().contains("db.url: a secret is text") && !error.to_string().contains("12345"), "{error}");
}

#[test]
fn only_a_live_setting_changes_while_the_program_runs() {
    let f = fixture();
    let config = open(&f, &[]);
    let error = refused(config.update(|settings| settings.port = 9000));
    assert_eq!((error.kind(), error.to_string().contains("port is not live")), (ErrorKind::Invalid, true));
    let before = config.get().version();
    config.update(|settings| *settings.limits.rps = 200).unwrap();
    assert_eq!(*config.get().limits.rps, 200);
    assert!(config.get().version() > before);
    config.update(|_| {}).unwrap();
}

#[test]
fn a_change_goes_over_every_layer_survives_a_restart_and_reset_gives_the_layers_back() {
    let f = fixture();
    let variables = [("APP_LIMITS_RPS", "1000")];
    let config = open(&f, &variables);
    config.update(|settings| *settings.limits.rps = 200).unwrap();
    assert_eq!(*config.get().limits.rps, 200, "a change goes over a variable");
    let rps = line(&config, "limits.rps");
    assert!(rps.from.starts_with("a change, version "), "{}", rps.from);
    assert_eq!(rps.over, ["1000 from APP_LIMITS_RPS", "100 from the declaration"]);
    drop(config);
    let f = f.reopen();
    let config = open(&f, &variables);
    assert_eq!(*config.get().limits.rps, 200, "a change survives a restart");
    config.reset("limits.rps").unwrap();
    assert_eq!(*config.get().limits.rps, 1000);
    let error = refused(config.reset("limits.rsp"));
    assert!(error.to_string().contains("reset limits.rsp: no such setting, and no change of it is kept"), "{error}");
    config.update(|settings| (*settings.limits.rps, *settings.limits.burst) = (7, 8)).unwrap();
    config.reset("limits").unwrap();
    assert_eq!((*config.get().limits.rps, *config.get().limits.burst), (1000, 10));
    config.update(|settings| *settings.limits.rps = 7).unwrap();
    config.reset_all().unwrap();
    assert_eq!(*config.get().limits.rps, 1000);
}

#[test]
fn two_changes_of_different_settings_both_stay_and_if_version_refuses_a_change_made_since() {
    let f = fixture();
    let (first, second) = (open(&f, &[]), open(&f, &[]));
    first.update(|settings| *settings.limits.rps = 200).unwrap();
    second.update(|settings| *settings.limits.burst = 500).unwrap();
    for config in [&first, &second] {
        assert_eq!((*config.get().limits.rps, *config.get().limits.burst), (200, 500));
    }
    let shown = first.get();
    second.update(|settings| *settings.limits.rps = 300).unwrap();
    let error = refused(first.if_version(shown.version()).update(|settings| *settings.limits.rps = 400));
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert_eq!(*first.get().limits.rps, 300);
    first.if_version(first.get().version()).update(|settings| *settings.limits.rps = 400).unwrap();
}

type Seen = Arc<Mutex<Vec<(u32, Option<u32>)>>>;

fn watched(config: &Config<Settings>) -> (Watching, Seen) {
    let seen: Seen = Arc::default();
    let into = Arc::clone(&seen);
    let watching = config.watch(move |next, previous| {
        into.lock().unwrap().push((*next.limits.rps, previous.map(|previous| *previous.limits.rps)));
    });
    (watching, seen)
}

#[test]
fn every_handle_and_watcher_sees_a_change_before_update_returns() {
    let f = fixture();
    let (first, second) = (open(&f, &[]), open(&f, &[]));
    let (watching, seen) = watched(&first);
    let (_also, seen_too) = watched(&first);
    assert_eq!(*seen.lock().unwrap(), [(100, None)], "a watcher is called with the value now");
    second.update(|settings| *settings.limits.rps = 200).unwrap();
    assert_eq!(*first.get().limits.rps, 200);
    assert_eq!(*seen.lock().unwrap(), [(100, None), (200, Some(100))]);
    assert_eq!(*seen_too.lock().unwrap(), [(100, None), (200, Some(100))]);
    drop(watching);
    second.update(|settings| *settings.limits.rps = 300).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2, "a watch stops when its guard is dropped");
    assert_eq!(seen_too.lock().unwrap().len(), 3);
}

#[test]
fn a_watcher_that_changes_the_config_sees_each_version_in_order() {
    let f = fixture();
    let config = open(&f, &[]);
    let seen: Seen = Arc::default();
    let (into, changing) = (Arc::clone(&seen), config.clone());
    let _watching = config.watch(move |next, previous| {
        into.lock().unwrap().push((*next.limits.rps, previous.map(|previous| *previous.limits.rps)));
        if *next.limits.rps < 103 {
            changing.update(|settings| *settings.limits.rps += 1).unwrap();
        }
    });
    assert_eq!(*seen.lock().unwrap(), [(100, None), (101, Some(100)), (102, Some(101)), (103, Some(102))]);
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Fixed {
    limits: FixedLimits,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct FixedLimits {
    rps: u32,
}

#[test]
fn a_change_another_handles_declaration_refuses_keeps_nothing() {
    let f = fixture();
    let live = open(&f, &[]);
    let _fixed = f.store.config("app", Fixed::default()).open().unwrap();
    let error = refused(live.update(|settings| *settings.limits.rps = 200));
    assert!(error.to_string().contains("limits.rps: not live, as another handle of the config declares it"), "{error}");
    assert_eq!(*live.get().limits.rps, 100);
}

#[test]
fn a_kept_change_that_no_longer_fits_is_ignored_and_said_and_strict_refuses_to_open() {
    let f = fixture();
    open(&f, &[]).update(|settings| *settings.limits.rps = 5000).unwrap();
    let at_most = |most: u32| {
        move |settings: &Settings| if *settings.limits.rps > most { Err(format!("rps past {most}")) } else { Ok(()) }
    };
    let config = f.store.config("app", Settings::default()).validate(at_most(1000)).open().unwrap();
    assert_eq!(*config.get().limits.rps, 100);
    let notes = config.explain().notes;
    assert!(notes.len() == 1 && notes[0].starts_with("limits.rps: the change of version "), "{notes:?}");
    assert!(notes[0].ends_with("is ignored: rps past 1000"), "{notes:?}");
    let error = refused(f.store.config("app", Settings::default()).validate(at_most(1000)).strict().open());
    assert!(error.to_string().contains("rps past 1000"), "{error}");
    drop(config);
    let wider = f.store.config("app", Settings::default()).validate(at_most(10_000)).open().unwrap();
    assert_eq!(*wider.get().limits.rps, 5000, "a kept change applies again once it fits");
}

#[test]
fn validate_refuses_at_open_and_keeps_no_change_it_refuses() {
    let f = fixture();
    let check = |settings: &Settings| {
        if settings.port == 0 || *settings.limits.rps == 0 { Err("zero".to_owned()) } else { Ok(()) }
    };
    let error =
        refused(f.store.config("app", Settings::default()).env(env(&[("APP_PORT", "0")])).validate(check).open());
    assert!(error.to_string().contains("zero"), "{error}");
    let config = f.store.config("app", Settings::default()).validate(check).open().unwrap();
    let version = config.get().version();
    let error = refused(config.update(|settings| *settings.limits.rps = 0));
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert_eq!((config.get().version(), *config.get().limits.rps), (version, 100));
}

#[test]
fn a_change_of_a_setting_this_program_does_not_declare_is_another_programs() {
    let f = fixture();
    open(&f, &[]).update(|settings| *settings.limits.burst = 50).unwrap();
    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    struct Other {
        port: u16,
    }
    let other = f.store.config("app", Other::default()).open().unwrap();
    assert_eq!(other.explain().notes, ["limits.burst: a change of a setting this program does not declare"]);
}

#[test]
fn a_variable_with_the_prefix_that_names_no_setting_is_said() {
    let f = fixture();
    let config = open(&f, &[("APP_PROT", "1"), ("OTHER", "2")]);
    assert_eq!(config.explain().notes, ["APP_PROT: a variable with the prefix that names no setting"]);
}

#[test]
fn a_change_past_its_bound_is_refused() {
    let f = fixture();
    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    #[serde(default)]
    struct Banner {
        text: Live<String>,
    }
    let config = f.store.config("banner", Banner::default()).open().unwrap();
    let error = refused(config.update(|banner| *banner.text = "x".repeat(17 << 10)));
    assert_eq!(error.kind(), ErrorKind::Limit);
}

#[test]
fn what_a_config_keeps_of_its_changes_is_bounded_and_a_path_too() {
    let f = fixture();
    let keys: Vec<String> = (0..20).map(|n| format!("text{n:02}")).collect();
    let live = |key: &String| (key.clone(), json!({ "\u{0}tinystore:live": "" }));
    let declared: serde_json::Map<String, serde_json::Value> = keys.iter().map(live).collect();
    let config = f.store.config("texts", serde_json::Value::Object(declared)).open().unwrap();
    let long = "x".repeat(15 << 10);
    let mut refused_at = None;
    for (at, key) in keys.iter().enumerate() {
        if let Err(error) = config.update(|texts| texts[key.as_str()] = json!(long)) {
            assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
            refused_at = Some(at);
            break;
        }
    }
    assert_eq!(refused_at, Some(17), "17 changes of 15 KiB fit in 256 KiB, the 18th does not");
    let deep = json!({ "a".repeat(200): { "b".repeat(100): 1 } });
    let error = refused(f.store.config("deep", deep).open());
    assert!(error.to_string().contains("a path past the 256 bytes"), "{error}");
}

#[test]
fn the_store_keeps_the_last_declaration_a_config_was_opened_with() {
    let f = fixture();
    let config = open(&f, &[]);
    let kv = crate::kv::engine::Kv::of(&f.store).unwrap();
    let kept: String = kv
        .file()
        .read(|connection| {
            Ok(connection
                .query_row("select declaration from _tinystore_kv_configs where name = 'app'", [], |row| row.get(0))
                .unwrap())
        })
        .unwrap();
    let kept: serde_json::Value = serde_json::from_str(&kept).unwrap();
    assert_eq!(kept["limits"]["group"]["rps"], json!({ "kind": "integer", "default": 100, "live": true }));
    assert_eq!(kept["db"]["group"]["url"]["secret"], json!(true));
    let commits = f.commits();
    drop(open(&f, &[]));
    assert_eq!(f.commits(), commits, "the same declaration again writes nothing");
    drop(config);
}

#[test]
fn a_name_is_a_buckets_kind_of_name() {
    let f = fixture();
    let error = refused(f.store.config("App", Settings::default()).open());
    assert_eq!(error.kind(), ErrorKind::Invalid);
}
