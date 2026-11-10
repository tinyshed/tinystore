//! A config as a program opens, reads, changes and watches one.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::fixture::{env, written};
use super::*;
use crate::kv::fixture::{Fixture, fixture};
use crate::{Clock, Error, ErrorKind};

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

fn file(f: &Fixture, name: &str, text: &str) -> std::path::PathBuf {
    written(f.dir.path(), name, text)
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

// contract: CFG-01
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

// contract: CFG-03
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

// contract: CFG-03
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

// contract: CFG-05, CFG-06, SEC-05
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
    let kept: String = crate::kv::Kv::of(&f.store)
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Sites {
    instance: BTreeMap<String, Instance>,
    backups: Vec<Instance>,
    mirrors: Live<BTreeMap<String, Instance>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Instance {
    host: String,
    password: Secret<String>,
}

// contract: CFG-06, CFG-09, SEC-05
#[test]
fn a_secret_a_map_or_a_list_holds_inside_prints_as_stars_in_explain_and_no_change_keeps_it() {
    let f = fixture();
    let toml = file(&f, "sites.toml", "[instance.aimee]\nhost = \"10.0.0.1\"\npassword = \"of-the-file\"\n");
    let variables = [
        ("APP_INSTANCE", r#"{"aimee":{"host":"10.0.0.2","password":"of-the-env"}}"#),
        ("APP_BACKUPS", r#"[{"host":"b","password":"of-a-list"}]"#),
    ];
    let config = f.store.config("sites", Sites::default()).file(toml.as_path()).env(env(&variables)).open().unwrap();
    assert_eq!(config.get().instance["aimee"].password.reveal(), "of-the-env");
    let explained = config.explain();
    let printed = format!("{explained}{explained:?}");
    for secret in ["of-the-file", "of-the-env", "of-a-list"] {
        assert!(!printed.contains(secret), "{printed}");
    }
    let instance = explained.settings.iter().find(|line| line.path == "instance").unwrap();
    let file_said = format!(r#"{{"aimee":{{"host":"10.0.0.1","password":"***"}}}} from {}"#, toml.display());
    assert_eq!(instance.value, r#"{"aimee":{"host":"10.0.0.2","password":"***"}}"#, "the rest stays as it was given");
    assert_eq!(instance.over, [file_said, "{} from the declaration".to_owned()]);

    let host = |password: &str| Instance { host: "h".to_owned(), password: Secret::new(password.to_owned()) };
    let error = refused(config.update(|sites| drop(sites.mirrors.insert("kazan".to_owned(), host("kept?")))));
    assert!(error.to_string().contains("mirrors holds a secret, which only a layer gives"), "{error}");
    let changes: i64 = crate::kv::Kv::of(&f.store)
        .unwrap()
        .file()
        .read(|connection| {
            Ok(connection.query_row("select count(*) from _tinystore_kv_config_changes", [], |row| row.get(0)).unwrap())
        })
        .unwrap();
    assert_eq!(changes, 0, "a change that holds a secret keeps nothing");
}

/// A database on trust or peer auth, which takes no password.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Trusted {
    db: TrustedDb,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct TrustedDb {
    host: String,
    password: Option<Secret<String>>,
}

// contract: CFG-06, CFG-13
#[test]
fn an_optional_secret_left_none_is_a_secret_whatever_layer_gives_it() {
    let f = fixture();
    let opened = |toml: &str, variables: &[(&str, &str)]| {
        let toml = file(&f, "trusted.toml", toml);
        f.store.config("trusted", Trusted::default()).file(toml).env(env(variables)).open()
    };
    let config = opened("[db]\nhost = \"db\"\npassword = \"s3cret\"\n", &[]).unwrap();
    assert_eq!(config.get().db.password.as_ref().map(|password| password.reveal().as_str()), Some("s3cret"));
    let printed = format!("{0}{0:?}", config.explain());
    assert!(!printed.contains("s3cret") && printed.contains("***"), "{printed}");

    let config = opened("[db]\nhost = \"db\"\n", &[("APP_DB_PASSWORD", "12345")]).unwrap();
    let given = config.get().db.password.as_ref().map(|password| password.reveal().clone());
    assert_eq!(given.as_deref(), Some("12345"), "a variable's text is a secret's text, never a number");
    let error = refused(opened("[db]\npassword = 12345\n", &[])).to_string();
    assert!(error.contains("db.password: a secret is text") && !error.contains("12345"), "{error}");

    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Sites {
        site: Entries<TrustedDb>,
    }
    let toml = file(&f, "sites.toml", "[site.aimee]\nhost = \"db\"\n[site.kazan]\nhost = \"db2\"\n");
    let variables = [("APP_SITE_AIMEE_PASSWORD", "67890")];
    let sites = f.store.config("sites", Sites::default()).file(toml).env(env(&variables)).open().unwrap();
    let password = |site: &str| sites.get().site[site].password.as_ref().map(|password| password.reveal().clone());
    assert_eq!((password("aimee").as_deref(), password("kazan")), (Some("67890"), None));
    assert!(!sites.explain().to_string().contains("67890"), "{}", sites.explain());

    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Inside {
        password: Secret<Option<String>>,
    }
    let error = refused(f.store.config("inside", Inside { password: Secret::new(None) }).open()).to_string();
    assert!(error.contains("password: a secret is text; one with no value is Option<Secret<String>>"), "{error}");
}

/// Settings whose secrets the declaration cannot see: an empty map, an empty
/// list and a `None`, of a struct that holds a `Secret`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Sms {
    tokens: BTreeMap<String, Creds>,
    backups: Vec<Creds>,
    fallback: Option<Creds>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Creds {
    user: String,
    token: Secret<String>,
}

// contract: CFG-06, CFG-03
#[test]
fn a_map_a_list_or_any_value_that_does_not_read_is_refused_without_what_a_layer_gave_it() {
    let f = fixture();
    let refused_saying = |yaml: &str, variables: &[(&str, &str)], said: &str| {
        let yaml = file(&f, "sms.yaml", yaml);
        let error = refused(load("sms", Sms::default()).file(yaml).env(env(variables)).open()).to_string();
        assert!(error.contains(said), "{said}: {error}");
        assert!(!error.contains("s3cr3t") && !error.contains("482913"), "{error}");
    };
    let tokens = [("APP_TOKENS", r#"{"sms":{"token":"s3cr3t"},}"#)];
    refused_saying("", &tokens, "APP_TOKENS: its text is not a map: trailing comma at line 1 column ");
    let backups = [("APP_BACKUPS", r#"[{"token":"s3cr3t"},]"#)];
    refused_saying("", &backups, "APP_BACKUPS: its text is not a list: trailing comma at line 1 column ");
    refused_saying("tokens: s3cr3t\n", &[], "sms.yaml: tokens: text is not a map");
    let number = "tokens: { sms: { token: 482913 } }\n";
    refused_saying(number, &[], "sms.yaml: tokens: invalid type, expected a string");
    refused_saying("backups: [s3cr3t]\n", &[], "sms.yaml: backups: invalid type, expected struct Creds");
    let fallback = [("APP_FALLBACK", r#"{"token":482913}"#)];
    refused_saying("", &fallback, "APP_FALLBACK: invalid type, expected a string");
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Vault {
    tokens: Required<BTreeMap<String, Creds>>,
    mode: Secret<Mode>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
enum Mode {
    #[default]
    Sms,
    Call,
}

// contract: CFG-06
#[test]
fn a_required_value_that_does_not_read_is_refused_without_what_a_layer_gave_it() {
    let opened =
        |variables: &[(&str, &str)]| refused(load("vault", Vault::default()).env(env(variables)).open()).to_string();
    let error = opened(&[("APP_TOKENS", r#"{"sms":{"token":482913}}"#), ("APP_MODE", "Sms")]);
    assert!(error.contains("invalid type, expected a string") && !error.contains("482913"), "{error}");
    let error = opened(&[("APP_TOKENS", "{}"), ("APP_MODE", "s3cr3t")]);
    assert!(error.contains("unknown variant, expected `Sms` or `Call`") && !error.contains("s3cr3t"), "{error}");
}

// contract: CFG-06, CFG-03
#[test]
fn what_a_file_gives_in_place_of_a_group_that_holds_a_secret_is_said_by_what_it_is() {
    let f = fixture();
    let opened = |name: &str, yaml: &str| {
        let yaml = file(&f, name, yaml);
        refused(f.store.config("app", Settings::default()).file(yaml).open()).to_string()
    };
    let error = opened("app.yaml", "db: postgres://user:s3cr3t@host\nlimits: 3\n");
    assert!(error.contains("app.yaml: db: text is not a group of settings") && !error.contains("s3cr3t"), "{error}");
    assert!(error.contains("app.yaml: limits: 3 is not a group of settings"), "a group with no secret: {error}");
    let error = opened("list.yaml", "- db: postgres://user:s3cr3t@host\n");
    assert!(error.contains("list.yaml: a list is not a map of settings") && !error.contains("s3cr3t"), "{error}");
}

// contract: CFG-06, CFG-05
#[test]
fn a_change_that_holds_a_secret_is_refused_before_another_handles_check_could_quote_it() {
    #[derive(Clone, Serialize, Deserialize)]
    struct Mirrors {
        mirrors: Live<Vec<String>>,
    }
    let f = fixture();
    let sites = f.store.config("sites", Sites::default()).open().unwrap();
    let _texts = f.store.config("sites", Mirrors { mirrors: Live::new(vec!["x".to_owned()]) }).open().unwrap();
    let kazan = Instance { host: "h".to_owned(), password: Secret::new("kept?".to_owned()) };
    let error =
        refused(sites.update(|value| drop(value.mirrors.insert("kazan".to_owned(), kazan.clone())))).to_string();
    assert!(error.contains("mirrors holds a secret, which only a layer gives") && !error.contains("kept?"), "{error}");
}

/// Another program's declaration of `Sites`' mirrors, whose type holds no secret.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PlainSites {
    mirrors: Live<BTreeMap<String, PlainInstance>>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PlainInstance {
    host: String,
    password: String,
}

// contract: CFG-06, CFG-07
#[test]
fn a_kept_change_of_a_map_whose_type_here_holds_a_secret_inside_is_ignored_and_said_and_strict_refuses_to_open() {
    let f = fixture();
    let toml = file(&f, "sites.toml", "[mirrors.aimee]\nhost = \"10.0.0.1\"\npassword = \"of-the-file\"\n");
    let opening = || f.store.config("sites", Sites::default()).file(toml.as_path());
    let sites = opening().open().unwrap();
    let plain = f.store.config("sites", PlainSites::default()).open().unwrap();
    let another = PlainInstance { host: "h".to_owned(), password: "of-another".to_owned() };
    plain.update(|value| drop(value.mirrors.insert("aimee".to_owned(), another.clone()))).unwrap();
    assert_eq!(plain.get().mirrors["aimee"].password, "of-another", "a type with no secret there takes it");

    let password = |config: &Config<Sites>| config.get().mirrors["aimee"].password.reveal().clone();
    let reopened = opening().open().unwrap();
    assert_eq!((password(&sites), password(&reopened)), ("of-the-file".to_owned(), "of-the-file".to_owned()));
    for config in [&sites, &reopened] {
        let notes = config.explain().notes;
        assert!(notes.len() == 1 && notes[0].starts_with("mirrors: the change of version "), "{notes:?}");
        assert!(notes[0].ends_with("is ignored: it holds a secret, which only a layer gives"), "{notes:?}");
        let printed = format!("{0}{0:?}", config.explain());
        assert!(!printed.contains("of-another"), "{printed}");
    }
    let error = refused(opening().strict().open()).to_string();
    assert!(error.contains("mirrors: the change of version ") && !error.contains("of-another"), "{error}");
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Dashbin {
    instance: Entries<Site>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Site {
    plugin: String,
    settings: Postgres,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Postgres {
    host: String,
    port: u16,
    user: String,
    password: Secret<String>,
    timeout: Duration,
}

impl Default for Site {
    fn default() -> Self {
        Site { plugin: "postgres".to_owned(), settings: Postgres::default() }
    }
}

impl Default for Postgres {
    fn default() -> Self {
        let (host, user) = ("localhost".to_owned(), String::new());
        Postgres { host, port: 5432, user, password: Secret::default(), timeout: Duration::from_secs(30) }
    }
}

const SITES: &str = r#"
[instance.aimee.settings]
host = "127.0.0.1"
port = 15432
user = "dashbin_ro"

[instance.site-3.settings]
host = "10.0.0.3"
"#;

fn dashbin(f: &Fixture, toml: &std::path::Path, variables: &[(&str, &str)]) -> Result<Config<Dashbin>> {
    f.store.config("dashbin", Dashbin::default()).file(toml).env(env(variables)).open()
}

// contract: CFG-11, CFG-06, CFG-09
#[test]
fn an_entry_takes_each_field_from_the_layers_and_a_variable_one_field_of_an_entry_a_file_made() {
    let f = fixture();
    let toml = file(&f, "dashbin.toml", SITES);
    let secret = file(&f, "pg", "of-a-file\n").display().to_string();
    let variables = [
        ("APP_INSTANCE_AIMEE_SETTINGS_PASSWORD", "of-a-variable"),
        ("APP_INSTANCE_SITE_3_SETTINGS_PASSWORD_FILE", secret.as_str()),
        ("APP_INSTANCE", r#"{"aimee":{"settings":{"port":6432}}}"#),
    ];
    let config = dashbin(&f, &toml, &variables).unwrap();
    let (aimee, site) = (&config.get().instance["aimee"], &config.get().instance["site-3"]);
    let shown =
        |site: &Site| (site.plugin.clone(), site.settings.host.clone(), site.settings.port, site.settings.user.clone());
    assert_eq!(
        shown(aimee),
        ("postgres".into(), "127.0.0.1".into(), 6432, "dashbin_ro".into()),
        "merged field by field"
    );
    assert_eq!(
        (aimee.settings.password.reveal().as_str(), aimee.settings.timeout),
        ("of-a-variable", Duration::from_secs(30))
    );
    assert_eq!((site.settings.host.as_str(), site.settings.port), ("10.0.0.3", 5432));
    assert_eq!(site.settings.password.reveal(), "of-a-file");

    let given = |path: &str| config.explain().settings.into_iter().find(|line| line.path == path).unwrap().given;
    let toml_origin = Origin::File { path: toml.display().to_string() };
    assert_eq!(
        given("instance.aimee.settings.port"),
        [
            gave("6432", env_origin("APP_INSTANCE", None, None)),
            gave("15432", toml_origin),
            gave("5432", Origin::Declaration)
        ]
    );
    assert_eq!(
        given("instance.aimee.settings.password"),
        [gave("***", env_origin("APP_INSTANCE_AIMEE_SETTINGS_PASSWORD", None, None))]
    );
    let from_file = env_origin("APP_INSTANCE_SITE_3_SETTINGS_PASSWORD_FILE", None, Some(&secret));
    assert_eq!(given("instance.site-3.settings.password"), [gave("***", from_file)]);
    let printed = config.explain().to_string();
    assert!(!printed.contains("of-a-variable") && !printed.contains("of-a-file"), "{printed}");
}

// contract: CFG-11, CFG-03
#[test]
fn each_entry_is_given_its_required_fields_and_a_variable_of_an_entry_no_layer_made_is_said() {
    let f = fixture();
    let toml = file(&f, "dashbin.toml", SITES);
    let error = refused(dashbin(&f, &toml, &[("APP_INSTANCE_AIMEE_SETTINGS_PASSWORD", "p")])).to_string();
    let missing = "instance.site-3.settings.password is required: instance.site-3.settings.password in";
    assert!(error.contains(missing) && error.contains("set APP_INSTANCE_SITE_3_SETTINGS_PASSWORD"), "{error}");
    assert!(!error.contains("instance.aimee.settings.password"), "{error}");

    let variables = [
        ("APP_INSTANCE_AIMEE_SETTINGS_PASSWORD", "p"),
        ("APP_INSTANCE_SITE_3_SETTINGS_PASSWORD", "p"),
        ("APP_INSTANCE_KAZAN_SETTINGS_PASSWORD", "p"),
    ];
    let notes = dashbin(&f, &toml, &variables).unwrap().explain().notes;
    let unmade = "APP_INSTANCE_KAZAN_SETTINGS_PASSWORD: a variable with the prefix that names no setting";
    assert!(notes.iter().any(|note| note.starts_with(unmade)), "{notes:?}");

    let twice = file(&f, "twice.toml", "[instance.site-3]\nplugin = \"a\"\n[instance.site_3]\nplugin = \"b\"\n");
    let error = refused(dashbin(&f, &twice, &[])).to_string();
    assert!(error.contains("would give both instance.site-3.settings.host and instance.site_3"), "{error}");
    let dotted = file(&f, "dotted.toml", "[instance.\"db.example\"]\nplugin = \"a\"\n");
    let error = refused(dashbin(&f, &dotted, &[])).to_string();
    assert!(error.contains(r#"instance: "db.example": an entry's key is not empty and has no dot"#), "{error}");
    let misspelt = file(&f, "misspelt.toml", "[instance.aimee]\nplugn = \"a\"\n");
    let error = refused(dashbin(&f, &misspelt, &[])).to_string();
    assert!(error.contains("instance.aimee.plugn: no such setting; did you mean instance.aimee.plugin?"), "{error}");
}

// contract: CFG-11, CFG-06
#[test]
fn an_entry_given_what_does_not_read_is_refused_without_its_secret() {
    let f = fixture();
    let toml = file(&f, "dashbin.toml", SITES);
    let refused_saying = |toml: &std::path::Path, variables: &[(&str, &str)], said: &str| {
        let error = refused(dashbin(&f, toml, variables)).to_string();
        assert!(error.contains(said), "{said}: {error}");
        assert!(!error.contains("s3cr3t") && !error.contains("482913"), "{error}");
    };
    let broken = [("APP_INSTANCE", r#"{"aimee":{"settings":{"password":"s3cr3t"}},}"#)];
    refused_saying(&toml, &broken, "APP_INSTANCE: its text is not a map: trailing comma at line 1 column ");
    let number = [("APP_INSTANCE", r#"{"aimee":{"settings":{"password":482913}}}"#)];
    refused_saying(&toml, &number, "APP_INSTANCE: instance.aimee.settings.password: a secret is text");
    let whole = [("APP_INSTANCE", r#"{"aimee":"s3cr3t"}"#)];
    refused_saying(&toml, &whole, "APP_INSTANCE: instance.aimee: text is not a group of settings");
    let in_file = file(&f, "whole.toml", "[instance]\naimee = \"s3cr3t\"\n");
    refused_saying(&in_file, &[], "whole.toml: instance.aimee: text is not a group of settings");
}

// contract: CFG-11, CFG-02, CFG-06
#[test]
fn entries_the_declaration_gives_open_with_no_layer_and_no_change_or_mark_makes_an_entry_live() {
    let f = fixture();
    let local = Site {
        settings: Postgres { password: Secret::new("dev-only".to_owned()), ..Postgres::default() },
        ..Site::default()
    };
    let declared = Dashbin { instance: Entries::from_iter([("local", local)]) };
    let config = f.store.config("dashbin", declared).open().unwrap();
    assert_eq!(config.get().instance["local"].settings.password.reveal(), "dev-only");
    let error = refused(config.update(|dashbin| drop(dashbin.instance.insert("x".to_owned(), Site::default()))));
    assert!(error.to_string().contains("instance is not live: the layers give its entries"), "{error}");
    let kept: String = crate::kv::Kv::of(&f.store)
        .unwrap()
        .file()
        .read(|connection| {
            Ok(connection.query_row("select declaration from _tinystore_kv_configs", [], |row| row.get(0)).unwrap())
        })
        .unwrap();
    assert!(!kept.contains("dev-only"), "a secret an entry gives is not kept: {kept}");
    let kept: Value = serde_json::from_str(&kept).unwrap();
    assert_eq!(kept["instance"]["kind"], "object", "an older release reads a map given whole: {kept}");

    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Marked {
        instance: Live<Entries<Site>>,
    }
    let error = refused(f.store.config("marked", Marked::default()).open()).to_string();
    assert!(error.contains("instance: entries take no mark: each of their fields carries its own"), "{error}");
    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Texts {
        instance: Entries<String>,
    }
    let error = refused(f.store.config("texts", Texts::default()).open()).to_string();
    assert!(error.contains("instance: an entry is a struct of settings; a map of values is a BTreeMap"), "{error}");
    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Pool {
        size: Live<u32>,
    }
    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Pools {
        pool: Entries<Pool>,
    }
    let error = refused(f.store.config("pools", Pools::default()).open()).to_string();
    assert!(error.contains("pool.*.size: an entry's field is not live: the layers give it"), "{error}");
}

// contract: CFG-02
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

// contract: CFG-01
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

fn env_origin(variable: &str, dotenv: Option<&str>, file: Option<&str>) -> Origin {
    Origin::Env { variable: variable.to_owned(), dotenv: dotenv.map(str::to_owned), file: file.map(str::to_owned) }
}

fn gave(value: &str, from: Origin) -> Given {
    Given { value: value.to_owned(), from }
}

// contract: CFG-09, CFG-06
#[test]
fn explain_gives_every_value_a_setting_was_given_with_where_it_came_from_as_data() {
    let f = fixture();
    let shown = |path: &std::path::Path| path.display().to_string();
    let yaml = shown(&file(&f, "app.yaml", "port: 8000\n"));
    let dotenv = shown(&file(&f, ".env", "APP_TIMEOUT=1m\n"));
    let secret = shown(&file(&f, "db", "postgres://user:password@host\n"));
    let variables = [("APP_PORT", "9000"), ("APP_DB_URL_FILE", secret.as_str())];
    let env = Env::prefix("APP").files([&dotenv]).from(variables);
    let config = f.store.config("app", Settings::default()).file(yaml.as_str()).env(env).open().unwrap();
    config.update(|settings| *settings.limits.rps = 200).unwrap();
    let given = |path: &str| line(&config, path).given;
    assert_eq!(
        given("port"),
        [
            gave("9000", env_origin("APP_PORT", None, None)),
            gave("8000", Origin::File { path: yaml.clone() }),
            gave("8080", Origin::Declaration)
        ]
    );
    assert_eq!(
        given("timeout"),
        [gave("1m", env_origin("APP_TIMEOUT", Some(&dotenv), None)), gave("30s", Origin::Declaration)]
    );
    assert_eq!(
        given("db.url"),
        [gave("***", env_origin("APP_DB_URL_FILE", None, Some(&secret))), gave("***", Origin::Declaration)],
        "a secret is *** in what it was given too"
    );
    let change = Origin::Change { version: 1, at: f.clock.now() };
    assert_eq!(given("limits.rps"), [gave("200", change), gave("100", Origin::Declaration)]);
    assert_eq!(line(&config, "limits.rps").from, "a change, version 1, 2027-01-15 08:00:00 UTC");
    let explained = config.explain();
    assert!(!format!("{explained:?}").contains("password"), "{explained:?}");
    for line in &explained.settings {
        let (own, over) = line.given.split_first().unwrap();
        assert_eq!((&own.value, own.from.to_string()), (&line.value, line.from.clone()), "{}", line.path);
        let said: Vec<String> = over.iter().map(|given| format!("{} from {}", given.value, given.from)).collect();
        assert_eq!(said, line.over, "{}", line.path);
    }
}

// contract: CFG-04
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

// contract: CFG-04
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

// contract: CFG-04
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

// contract: CFG-05
#[test]
fn a_change_another_handles_declaration_refuses_keeps_nothing() {
    let f = fixture();
    let live = open(&f, &[]);
    let _fixed = f.store.config("app", Fixed::default()).open().unwrap();
    let error = refused(live.update(|settings| *settings.limits.rps = 200));
    assert!(error.to_string().contains("limits.rps: not live, as another handle of the config declares it"), "{error}");
    assert_eq!(*live.get().limits.rps, 100);
}

// contract: CFG-07
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

// contract: CFG-05
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

// contract: CFG-07
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
    assert_eq!(
        config.explain().notes,
        ["APP_PROT: a variable with the prefix that names no setting; did you mean APP_PORT?"]
    );
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
    let kv = crate::kv::Kv::of(&f.store).unwrap();
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

#[test]
fn a_change_of_one_setting_made_meanwhile_is_counted_by_the_retry() {
    let f = fixture();
    let (first, second) = (open(&f, &[]), open(&f, &[]));
    let mut runs = 0;
    second
        .update(|settings| {
            runs += 1;
            if runs == 1 {
                first.update(|settings| *settings.limits.rps += 1).unwrap();
            }
            *settings.limits.rps += 1;
        })
        .unwrap();
    assert_eq!(runs, 2, "the change ran again on the settings the other made");
    assert_eq!(*first.get().limits.rps, 102, "both increments stay");
}

/// What an agent beside someone's database reads at start, with no live
/// setting and no store.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Agent {
    hub: String,
    database: Secret<String>,
    retries: u32,
}

// contract: CFG-10, CFG-01, CFG-06
#[test]
fn a_config_loaded_without_a_store_reads_its_layers_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("agent.toml");
    std::fs::write(&file, "hub = \"wss://hub.example.com/agent\"\ndatabase = \"postgres://reader:secret@db/shop\"\n")
        .unwrap();
    let env = Env::prefix("AGENT").from([("AGENT_RETRIES", "5")]);
    let agent = load("agent", Agent::default()).file(File::optional(&file)).env(env).open().unwrap();
    let read = agent.get();
    assert_eq!((read.hub.as_str(), read.retries), ("wss://hub.example.com/agent", 5));
    assert_eq!(read.database.reveal(), "postgres://reader:secret@db/shop");
    assert!(agent.explain().to_string().contains("agent.toml"), "{}", agent.explain());
    // nothing to change, nothing to reset, and only the operator's file in the directory
    assert_eq!(agent.update(|agent| agent.retries = 6).unwrap_err().kind(), ErrorKind::Invalid);
    agent.reset_all().unwrap();
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, ["agent.toml"]);
}

// contract: CFG-10, CFG-03
#[test]
fn a_config_loaded_without_a_store_refuses_a_live_setting_by_its_path_and_a_missing_file_by_its() {
    let error = load("app", Settings::default()).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("limits.burst is live"), "{error}");
    let error = load("agent", Agent::default()).file("missing-agent.toml").open().unwrap_err();
    assert!(error.to_string().contains("missing-agent.toml"), "{error}");
}

/// `Settings` declared as data rather than by its type.
fn settings_as_data() -> Declaration {
    let declaration = json!({
        "port": { "kind": "integer", "default": 8080 },
        "origins": { "kind": { "list": { "kind": "text" } }, "default": ["localhost"] },
        "timeout": { "kind": "duration", "default": "30s" },
        "db": { "group": {
            "url": { "kind": "text", "secret": true, "default": "postgres://local" },
            "pool": { "kind": "integer", "default": 10 }
        } },
        "limits": { "group": {
            "rps": { "kind": "integer", "default": 100, "live": true },
            "burst": { "kind": "integer", "default": 10, "live": true }
        } }
    });
    Declaration::from_json(declaration).unwrap()
}

// contract: CFG-13, CFG-06, CFG-09
#[test]
fn a_declaration_as_data_reads_the_layers_rules_secrets_and_explain_a_typed_one_does() {
    let f = fixture();
    let yaml = file(&f, "app.yaml", "port: 8000\norigins: [a.com, b.com]\n");
    let secret = file(&f, "db", "postgres://user:password@host\n").display().to_string();
    let variables = [("APP_PORT", "9000"), ("APP_DB_URL_FILE", secret.as_str()), ("APP_PROT", "1")];
    let typed = f.store.config("typed", Settings::default()).file(yaml.as_path()).env(env(&variables)).open().unwrap();
    let data = f.store.config("data", settings_as_data()).file(yaml.as_path()).env(env(&variables)).open().unwrap();
    assert_eq!(data.explain(), typed.explain(), "every line and every note alike, the secret ***");
    let read: Settings = serde_json::from_value((*data.get()).clone()).unwrap();
    assert_eq!(read, *typed.get(), "a span reads as serde writes a Duration");

    data.update(|value| value["limits"]["rps"] = json!(200)).unwrap();
    assert_eq!(data.get()["limits"]["rps"], 200);
    assert!(refused(data.update(|value| value["port"] = json!(1))).to_string().contains("port is not live"));
    let error = refused(data.update(|value| value["db"]["url"] = json!("x"))).to_string();
    assert!(error.contains("db.url is a secret, which only a layer gives"), "{error}");
    let error = refused(f.store.config("data", settings_as_data()).env(env(&[("APP_PORT", "http")])).open());
    assert!(error.to_string().contains(r#"APP_PORT: "http" is not a number"#), "{error}");
    let error = refused(load("data", settings_as_data()).open());
    assert!(error.to_string().contains("limits.burst is live"), "{error}");
    let error = Declaration::from_json(json!({ "db": { "group": { "pool": { "kind": "integer" } } } })).unwrap_err();
    assert!(error.to_string().starts_with("the declaration: db.pool: a setting has a default"), "{error}");
}

// contract: CFG-15, CFG-13, CFG-06
#[test]
fn a_setting_with_no_value_as_data_reads_and_explains_as_a_typed_option_left_none() {
    let f = fixture();
    let as_data = || {
        let password = json!({ "kind": "text", "secret": true, "default": null });
        let declaration =
            json!({ "db": { "group": { "host": { "kind": "text", "default": "" }, "password": password } } });
        Declaration::from_json(declaration).unwrap()
    };
    for (name, toml) in [("unset", "[db]\nhost = \"db\"\n"), ("given", "[db]\nhost = \"db\"\npassword = \"s3cret\"\n")]
    {
        let toml = file(&f, &format!("{name}.toml"), toml);
        let typed = load("trusted", Trusted::default()).file(toml.as_path()).env(env(&[])).open().unwrap();
        let data = load("trusted", as_data()).file(toml.as_path()).env(env(&[])).open().unwrap();
        assert_eq!(data.explain(), typed.explain(), "{name}");
        let read: Trusted = serde_json::from_value((*data.get()).clone()).unwrap();
        assert_eq!(read.db.password.is_none(), typed.get().db.password.is_none(), "{name}");
    }
    let toml = file(&f, "unset.toml", "[db]\nhost = \"db\"\n");
    let typed = load("trusted", Trusted::default()).file(toml).open().unwrap();
    let password = typed.explain().settings.into_iter().find(|line| line.path == "db.password").unwrap();
    assert_eq!((password.value.as_str(), password.from.as_str()), ("unset", "the declaration"));

    #[derive(Clone, Default, Serialize, Deserialize)]
    struct Proxy {
        proxy: Option<String>,
    }
    let proxy = load("proxy", Proxy::default()).open().unwrap().explain().to_string();
    assert_eq!(proxy.trim_end(), "proxy  unset  the declaration", "an Option left None says unset too");
}

// contract: CFG-13, CFG-15
#[test]
fn an_option_of_text_left_none_reads_a_variable_as_text_and_one_of_a_number_as_a_number() {
    #[derive(Clone, Default, Serialize, Deserialize)]
    #[serde(default)]
    struct Account {
        id: Option<String>,
        port: Option<u16>,
        extra: Option<Value>,
    }
    let variables = [("APP_ID", "123456"), ("APP_PORT", "8080"), ("APP_EXTRA", "[1, 2]")];
    let config = load("account", Account::default()).env(env(&variables)).open().unwrap();
    let account = config.get();
    assert_eq!((account.id.as_deref(), account.port), (Some("123456"), Some(8080)));
    assert_eq!(account.extra, Some(json!([1, 2])), "a type that takes any value still does");

    let as_data = json!({ "id": { "kind": "text", "default": null } });
    let data = load("account", Declaration::from_json(as_data).unwrap()).env(env(&[("APP_ID", "123456")])).open();
    assert_eq!(data.unwrap().get()["id"], json!("123456"), "as data the same");
}

/// dashbin's instances, whose settings are the group of the plugin each one's
/// `plugin` names, a shape it learns once its plugins are loaded.
fn plugins() -> Declaration {
    let postgres = json!({
        "host": { "kind": "text", "default": "localhost" },
        "port": { "kind": "integer", "default": 5432, "min": 1, "max": 65535 },
        "database": { "kind": "text", "required": true },
        "user": { "kind": "text", "required": true },
        "password": { "kind": "text", "secret": true, "required": true }
    });
    let mysql = json!({
        "host": { "kind": "text", "default": "localhost" },
        "port": { "kind": "integer", "default": 3306 },
        "password": { "kind": "text", "secret": true, "required": true }
    });
    let stripe = json!({ "key": { "kind": "text", "secret": true, "required": true } });
    Declaration::from_json(json!({
        "instance": { "entries": {
            "plugin": { "kind": "text", "required": true },
            "settings": { "chosenBy": "plugin", "groups": { "postgres": postgres, "mysql": mysql, "stripe": stripe } }
        } }
    }))
    .unwrap()
}

/// The issue's file, its host misspelt.
const AIMEE: &str = r#"
[instance.aimee]
plugin = "postgres"
[instance.aimee.settings]
hots = "127.0.0.1"
database = "aimee"
user = "dashbin_ro"
password = "s3cret"
"#;

fn instances(toml: &std::path::Path, variables: &[(&str, &str)]) -> Result<Config<Value>> {
    let env = Env::prefix("DASHBIN").from(variables.iter().map(|&(name, value)| (name, value)));
    load("dashbin", plugins()).file(toml).env(env).open()
}

// contract: CFG-14, CFG-13, CFG-03, CFG-06
#[test]
fn an_entrys_settings_are_read_checked_masked_and_named_by_the_group_its_plugin_chooses() {
    let f = fixture();
    let error = refused(instances(&file(&f, "misspelt.toml", AIMEE), &[])).to_string();
    let hint = "instance.aimee.settings.hots: no such setting; did you mean instance.aimee.settings.host?";
    assert!(error.contains(hint), "{error}");

    let toml = file(&f, "dashbin.toml", &AIMEE.replace("hots", "host"));
    let config = instances(&toml, &[]).unwrap();
    let settings = json!({
        "host": "127.0.0.1", "port": 5432, "database": "aimee", "user": "dashbin_ro", "password": "s3cret"
    });
    assert_eq!(config.get()["instance"]["aimee"]["settings"], settings, "the port's default applies");
    let printed = config.explain().to_string();
    assert!(printed.contains("instance.aimee.settings.password  ***") && !printed.contains("s3cret"), "{printed}");
    assert!(config.explain().notes.is_empty(), "{:?}", config.explain().notes);

    let secret = file(&f, "pg", "of-a-file\n").display().to_string();
    let variables = [
        ("DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT", "15432"),
        ("DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD_FILE", secret.as_str()),
    ];
    let config = instances(&toml, &variables).unwrap();
    assert_eq!(config.get()["instance"]["aimee"]["settings"]["port"], 15432);
    assert_eq!(config.get()["instance"]["aimee"]["settings"]["password"], "of-a-file");
    let password = line_of(&config, "instance.aimee.settings.password");
    let from_file = env_origin("DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD_FILE", None, Some(&secret));
    assert_eq!(
        password.given,
        [gave("***", from_file), gave("***", Origin::File { path: toml.display().to_string() })]
    );
    let error = refused(instances(&toml, &[("DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT", "70000")])).to_string();
    assert!(error.contains("DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT: 70000 is above 65535"), "{error}");

    let misnamed = file(&f, "misnamed.toml", &AIMEE.replace("hots", "host").replace("\"postgres\"", "\"postgress\""));
    let error = refused(instances(&misnamed, &[])).to_string();
    let named = r#""postgress" is not one of mysql, postgres, stripe; did you mean postgres?"#;
    assert!(error.contains(&format!("instance.aimee.plugin: {named}")), "{error}");
    let error = refused(instances(&toml, &[("DASHBIN_INSTANCE_AIMEE_PLUGIN", "postgress")])).to_string();
    assert!(error.contains(&format!("DASHBIN_INSTANCE_AIMEE_PLUGIN: {named}")), "{error}");
}

// contract: CFG-14, CFG-03
#[test]
fn a_group_not_chosen_takes_no_field_and_each_group_has_its_own_required_fields() {
    let f = fixture();
    let stripe =
        file(&f, "stripe.toml", "[instance.pay]\nplugin = \"stripe\"\n[instance.pay.settings]\nhost = \"a\"\n");
    let error = refused(instances(&stripe, &[])).to_string();
    assert!(error.contains("instance.pay.settings.host: no such setting"), "{error}");
    let stripe = file(&f, "pay.toml", "[instance.pay]\nplugin = \"stripe\"\n");
    let error = refused(instances(&stripe, &[])).to_string();
    let missing = "instance.pay.settings.key is required: instance.pay.settings.key in";
    assert!(error.contains(missing) && error.contains("set DASHBIN_INSTANCE_PAY_SETTINGS_KEY"), "{error}");
    let config = instances(&stripe, &[("DASHBIN_INSTANCE_PAY_SETTINGS_KEY", "sk_test")]).unwrap();
    assert_eq!(config.get()["instance"]["pay"], json!({ "plugin": "stripe", "settings": { "key": "sk_test" } }));
    let unread = [("DASHBIN_INSTANCE_PAY_SETTINGS_KEY", "sk_test"), ("DASHBIN_INSTANCE_PAY_SETTINGS_HOST", "a")];
    let notes = instances(&stripe, &unread).unwrap().explain().notes;
    let said = "DASHBIN_INSTANCE_PAY_SETTINGS_HOST: a variable with the prefix that names no setting";
    assert!(notes.iter().any(|note| note.starts_with(said)), "{notes:?}");
}

// contract: CFG-14
#[test]
fn a_later_layer_that_chooses_another_group_drops_what_earlier_layers_gave_the_first_and_says_so() {
    let f = fixture();
    let toml = file(&f, "dashbin.toml", &AIMEE.replace("hots", "host"));
    let variables =
        [("DASHBIN_INSTANCE_AIMEE_PLUGIN", "mysql"), ("DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD", "of-a-variable")];
    let config = instances(&toml, &variables).unwrap();
    let mysql = json!({ "host": "localhost", "port": 3306, "password": "of-a-variable" });
    assert_eq!(config.get()["instance"]["aimee"]["settings"], mysql, "the file's postgres settings are not mysql's");
    let dropped = format!(
        "instance.aimee.settings: what {} gave as postgres is dropped: instance.aimee.plugin is mysql, from \
         DASHBIN_INSTANCE_AIMEE_PLUGIN",
        toml.display()
    );
    assert_eq!(config.explain().notes, [dropped]);
    let back = [("DASHBIN_INSTANCE", r#"{"aimee":{"plugin":"mysql"}}"#), ("DASHBIN_INSTANCE_AIMEE_PLUGIN", "postgres")];
    let config = instances(&toml, &back).unwrap();
    assert_eq!(config.get()["instance"]["aimee"]["settings"]["database"], "aimee", "chosen again, the file's stay");

    let declared = Declaration::from_json(json!({
        "plugin": { "kind": "text", "default": "postgres" },
        "settings": { "chosenBy": "plugin", "groups": {
            "postgres": {
                "port": { "kind": "integer", "default": 5432, "live": false },
                "timeout": { "kind": "duration", "default": "30s" }
            },
            "stripe": { "key": { "kind": "text", "default": "sk_test" } }
        } },
        "rps": { "kind": "integer", "default": 100, "live": true }
    }))
    .unwrap();
    let config = f.store.config("plugin", declared).open().unwrap();
    let settings = json!({ "port": 5432, "timeout": { "secs": 30, "nanos": 0 } });
    assert_eq!(config.get()["settings"], settings, "a span as serde writes a Duration");
    let error = refused(config.update(|value| value["settings"]["port"] = json!(1))).to_string();
    assert!(error.contains("settings is not live: the layers give it"), "{error}");
    let error = refused(config.update(|value| value["plugin"] = json!("stripe"))).to_string();
    assert!(error.contains("plugin is not live"), "{error}");
}

fn line_of(config: &Config<Value>, path: &str) -> Line {
    config.explain().settings.into_iter().find(|line| line.path == path).unwrap()
}
