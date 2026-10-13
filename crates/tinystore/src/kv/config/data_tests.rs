use serde_json::{Value, json};

use super::*;
use crate::kv::config::{Config, Sent};
use crate::kv::fixture::{Fixture, fixture};
use crate::{Error, ErrorKind};

/// A declaration as the Bun SDK writes one: every kind, rules and marks.
fn declared() -> Value {
    json!({
        "port": { "kind": "integer", "default": 8080, "min": 1, "max": 65535 },
        "level": { "kind": "text", "default": "info", "oneOf": ["info", "debug", "warn", "error"] },
        "timeout": { "kind": "duration", "default": "30s" },
        "hosts": { "kind": { "list": { "kind": "text" } }, "default": ["localhost"], "max": 3 },
        "upstreams": {
            "kind": { "list": { "group": {
                "host": { "kind": "text", "required": true },
                "port": { "kind": "integer", "default": 80 }
            } } },
            "default": []
        },
        "db": { "group": {
            "url": { "kind": "text", "secret": true, "required": true },
            "pool": { "kind": "integer", "default": 10 }
        } },
        "limits": { "group": { "rps": { "kind": "integer", "default": 100, "min": 0, "live": true } } }
    })
}

fn opened(f: &Fixture, layers: &Value) -> crate::Result<Config<Value>> {
    let declaration = declared();
    f.store.config_sent(&Sent { name: "app", declaration: &declaration, layers, strict: false, remember: true })
}

fn variables(pairs: &[(&str, &str)]) -> Value {
    let variables: serde_json::Map<String, Value> =
        pairs.iter().map(|(name, value)| ((*name).to_owned(), json!(value))).collect();
    json!([{ "env": { "prefix": "APP", "variables": variables } }])
}

fn refused<T>(result: crate::Result<T>) -> Error {
    match result {
        Ok(_) => panic!("refused, not opened"),
        Err(error) => error,
    }
}

#[test]
fn a_declaration_sent_as_data_reads_as_the_store_keeps_one() {
    let read = declaration(&declared()).unwrap();
    let port = read.setting("port").unwrap();
    assert_eq!((port.kind.clone(), port.rules.min, port.rules.max), (Kind::Integer, Some(1.0), Some(65535.0)));
    assert_eq!(read.setting("timeout").unwrap().default, Some(json!(30_000)), "a span's default kept as milliseconds");
    assert!(read.setting("db.url").unwrap().secret && read.setting("limits.rps").unwrap().live);
    let again = declaration(&read.to_json()).unwrap();
    let kept: Vec<String> = again.settings().into_iter().map(|(path, _)| path).collect();
    assert_eq!(kept, ["db.pool", "db.url", "hosts", "level", "limits.rps", "port", "timeout", "upstreams"]);
    assert_eq!(again.setting("level"), read.setting("level"), "what kv.db keeps reads back the same");
}

#[test]
fn a_declaration_sent_refuses_what_means_nothing() {
    let refused = |setting: Value| declaration(&json!({ "x": setting })).unwrap_err();
    assert_eq!(
        refused(json!({ "kind": "text", "secret": true, "live": true, "required": true })),
        "x: a secret is never live"
    );
    assert_eq!(refused(json!({ "kind": "integer", "secret": true, "required": true })), "x: a secret is text");
    assert_eq!(
        refused(json!({ "kind": "text", "required": true, "default": "a" })),
        "x: a required setting has no default"
    );
    assert_eq!(refused(json!({ "kind": "text" })), "x: a setting has a default, or is required");
    assert_eq!(refused(json!({ "kind": "colour", "default": 1 })), r#"x: "colour" is no kind"#);
    assert_eq!(refused(json!({ "kind": "integer", "default": 0, "min": 1 })), "x: its default: 0 is below 1");
    let item = json!({ "kind": { "list": { "group": { "a": { "kind": "text", "live": true } } } }, "default": [] });
    assert_eq!(refused(item), "x[].a: a list's item has no live or secret field");
}

#[test]
fn a_config_sent_reads_its_layers_as_sent_and_gives_its_value_as_data() {
    let f = fixture();
    let layers = json!([
        { "object": { "level": "debug" }, "place": 2 },
        { "file": { "name": "app.yaml", "text": "port: 9000\nupstreams:\n  - host: a.com\n" } },
        { "env": {
            "prefix": "APP",
            "vars": { "db.url": "DATABASE_URL" },
            "variables": { "APP_PORT": "9100", "DATABASE_URL_FILE": "/run/secrets/db", "APP_PROT": "1" },
            "files": { "/run/secrets/db": "postgres://secret\n" },
            "dotenv": [{ "name": ".env", "text": "APP_TIMEOUT=1m\n" }]
        } }
    ]);
    let config = opened(&f, &layers).unwrap();
    let value = config.get();
    assert_eq!(value["port"], json!(9100));
    assert_eq!((value["level"].clone(), value["timeout"].clone()), (json!("debug"), json!(60_000)));
    assert_eq!(value["upstreams"], json!([{ "host": "a.com", "port": 80 }]), "an item's default filled in");
    assert_eq!(value["db"]["url"], json!("postgres://secret"));
    let explained = config.explain();
    let from = |path: &str| explained.settings.iter().find(|line| line.path == path).unwrap().from.clone();
    assert_eq!(
        (from("level"), from("port"), from("timeout")),
        ("layer 2".into(), "APP_PORT".into(), "APP_TIMEOUT in .env".into())
    );
    assert_eq!(from("db.url"), "DATABASE_URL_FILE, /run/secrets/db");
    assert_eq!(explained.notes, ["APP_PROT: a variable with the prefix that names no setting"]);
}

#[test]
fn a_rule_sent_refuses_a_layers_value_naming_it() {
    let f = fixture();
    let error =
        refused(opened(&f, &variables(&[("APP_PORT", "70000"), ("APP_LEVEL", "verbose"), ("APP_DB_URL", "u")])));
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let message = error.to_string();
    assert!(message.contains("APP_PORT: 70000 is above 65535"), "{message}");
    assert!(message.contains(r#"APP_LEVEL: "verbose" is not one of info, debug, warn, error"#), "{message}");
    let error = refused(opened(&f, &variables(&[("APP_HOSTS", "a,b,c,d"), ("APP_DB_URL", "u")])));
    assert!(error.to_string().contains("APP_HOSTS: 4 items is above 3 items"), "{error}");
    let layers = json!([{ "object": { "prot": 1 }, "place": 2 }, { "env": { "prefix": "APP", "variables": { "APP_DB_URL": "u" } } }]);
    assert!(refused(opened(&f, &layers)).to_string().contains("layer 2: prot: no such setting"));
}

#[test]
fn a_file_sent_absent_is_refused_unless_optional() {
    let f = fixture();
    let env = json!({ "env": { "prefix": "APP", "variables": { "APP_DB_URL": "u" } } });
    let absent = json!([{ "file": { "name": "app.local.yaml" } }, env]);
    assert!(refused(opened(&f, &absent)).to_string().contains("app.local.yaml: no such file"));
    let optional = json!([{ "file": { "name": "app.local.yaml", "optional": true } }, env]);
    assert_eq!(opened(&f, &optional).unwrap().explain().notes, ["app.local.yaml: absent"]);
}

#[test]
fn a_change_sent_keeps_the_live_settings_it_names_on_the_version_it_was_made_on() {
    let f = fixture();
    let config = opened(&f, &variables(&[("APP_DB_URL", "u")])).unwrap();
    let version = config.get().version();
    config.update_data(version, &json!({ "limits": { "rps": 200 } })).unwrap();
    assert_eq!(config.get()["limits"]["rps"], json!(200));
    let moved = config.update_data(version, &json!({ "limits": { "rps": 300 } })).unwrap_err();
    assert_eq!(moved.kind(), ErrorKind::Conflict, "a change made since its version was read");
    let now = config.get().version();
    let refused = |changes: Value| config.update_data(now, &changes).unwrap_err().to_string();
    assert!(refused(json!({ "port": 1 })).contains("port is not live"));
    assert!(refused(json!({ "limits": { "rpx": 1 } })).contains("limits.rpx: no such setting"));
    assert!(refused(json!({ "limits": { "rps": -1 } })).contains("limits.rps: -1 is below 0"));
    assert!(refused(json!({ "limits": 5 })).contains("limits: 5 is not a group of settings"));
    assert_eq!(config.get()["limits"]["rps"], json!(200));
}

#[test]
fn a_connection_that_may_only_read_keeps_no_declaration() {
    let f = fixture();
    let (declaration, layers) =
        (declared(), json!([{ "env": { "prefix": "APP", "variables": { "APP_DB_URL": "u" } } }]));
    let sent = Sent { name: "app", declaration: &declaration, layers: &layers, strict: false, remember: false };
    f.store.config_sent(&sent).unwrap();
    assert_eq!(f.rows("_tinystore_kv_configs"), 0);
}
