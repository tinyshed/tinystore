use serde_json::{Value, json};

use super::*;
use crate::config::{Config, Given, Origin, Sent};
use crate::kv::fixture::{Fixture, fixture};
use crate::{Clock, Error, ErrorKind};

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

/// The config `name` as an SDK sends it, with its declaration and layers.
fn sent(f: &Fixture, name: &str, declaration: &Value, layers: &Value) -> crate::Result<Config<Value>> {
    f.store.config_sent(&Sent { name, declaration, layers, strict: false, remember: true })
}

fn opened(f: &Fixture, layers: &Value) -> crate::Result<Config<Value>> {
    sent(f, "app", &declared(), layers)
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
    assert_eq!(
        refused(json!({ "kind": "text" })),
        r#"x: a setting has a default, "default": null for none, or is required"#
    );
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
    assert_eq!(explained.notes, ["APP_PROT: a variable with the prefix that names no setting; did you mean APP_PORT?"]);
}

// contract: CFG-09
#[test]
fn explain_gives_a_layer_sent_by_its_place_and_each_value_it_went_over() {
    let f = fixture();
    let layers = json!([
        { "object": { "level": "debug", "port": 8100 }, "place": 2 },
        { "file": { "name": "app.yaml", "text": "port: 9000\n" } },
        { "env": {
            "prefix": "APP",
            "vars": { "db.url": "DATABASE_URL" },
            "variables": { "APP_PORT": "9100", "DATABASE_URL_FILE": "/run/secrets/db" },
            "files": { "/run/secrets/db": "postgres://secret\n" },
            "dotenv": [{ "name": ".env", "text": "APP_TIMEOUT=1m\n" }]
        } }
    ]);
    let config = opened(&f, &layers).unwrap();
    config.update_data(config.get().version(), &json!({ "limits": { "rps": 200 } })).unwrap();
    let explained = config.explain();
    let given = |path: &str| explained.settings.iter().find(|line| line.path == path).unwrap().given.clone();
    let gave = |value: &str, from: Origin| Given { value: value.to_owned(), from };
    let env = |variable: &str, dotenv: Option<&str>, file: Option<&str>| Origin::Env {
        variable: variable.to_owned(),
        dotenv: dotenv.map(str::to_owned),
        file: file.map(str::to_owned),
    };
    assert_eq!(
        given("port"),
        [
            gave("9100", env("APP_PORT", None, None)),
            gave("9000", Origin::File { path: "app.yaml".to_owned() }),
            gave("8100", Origin::Layer { place: 2 }),
            gave("8080", Origin::Declaration),
        ]
    );
    assert_eq!(
        given("level"),
        [gave(r#""debug""#, Origin::Layer { place: 2 }), gave(r#""info""#, Origin::Declaration)]
    );
    assert_eq!(
        given("timeout"),
        [gave("1m", env("APP_TIMEOUT", Some(".env"), None)), gave("30s", Origin::Declaration)]
    );
    assert_eq!(given("db.url"), [gave("***", env("DATABASE_URL_FILE", None, Some("/run/secrets/db")))]);
    let change = Origin::Change { version: 1, at: f.clock.now() };
    assert_eq!(given("limits.rps"), [gave("200", change), gave("100", Origin::Declaration)]);
}

/// The issue's postgres plugin, whose database on trust or peer auth takes
/// no password, beside a setting with no value of every kind.
fn trusted() -> Value {
    json!({
        "db": { "group": {
            "host": { "kind": "text", "default": "localhost" },
            "password": { "kind": "text", "secret": true, "default": null }
        } },
        "port": { "kind": "integer", "default": null, "min": 1 },
        "ratio": { "kind": "number", "default": null },
        "tls": { "kind": "boolean", "default": null },
        "timeout": { "kind": "duration", "default": null },
        "hosts": { "kind": { "list": { "kind": "text" } }, "default": null, "max": 2 },
        "labels": { "kind": "object", "default": null },
        "level": { "kind": "text", "default": null, "oneOf": ["info", "debug"] },
        "rps": { "kind": "integer", "default": null, "live": true }
    })
}

// contract: CFG-15, CFG-06, CFG-09
#[test]
fn a_setting_sent_with_no_value_is_null_until_a_layer_gives_one_and_explain_says_unset() {
    let f = fixture();
    let config = sent(&f, "trusted", &trusted(), &json!([])).unwrap();
    let value = config.get();
    let unset = ["port", "ratio", "tls", "timeout", "hosts", "labels", "level", "rps"];
    assert_eq!((value["db"]["host"].clone(), value["db"]["password"].clone()), (json!("localhost"), Value::Null));
    assert!(unset.iter().all(|path| value[path].is_null()), "{:?}", *value);
    let explained = config.explain();
    let line = |path: &str| explained.settings.iter().find(|line| line.path == path).unwrap().clone();
    assert_eq!((line("db.password").value, line("db.password").from), ("unset".into(), "the declaration".into()));
    assert_eq!(line("port").given, [Given { value: "unset".to_owned(), from: Origin::Declaration }]);

    let layers = json!([
        { "file": { "name": "trusted.toml", "text": "port = 5432\nhosts = [\"a\"]\n[db]\npassword = \"s3cret\"\n" } },
        { "env": { "prefix": "APP", "variables": { "APP_TIMEOUT": "30s", "APP_LEVEL": "debug", "APP_TLS": "true" } } }
    ]);
    let config = sent(&f, "trusted", &trusted(), &layers).unwrap();
    let value = config.get();
    assert_eq!((value["port"].clone(), value["db"]["password"].clone()), (json!(5432), json!("s3cret")));
    assert_eq!((value["timeout"].clone(), value["level"].clone()), (json!(30_000), json!("debug")));
    let password = config.explain().settings.into_iter().find(|line| line.path == "db.password").unwrap();
    assert_eq!((password.value, password.over), ("***".to_owned(), vec!["unset from the declaration".to_owned()]));
    assert!(!format!("{:?}", config.explain()).contains("s3cret"));

    let bad = |variables: Value| {
        refused(sent(&f, "trusted", &trusted(), &json!([{ "env": { "prefix": "APP", "variables": variables } }])))
    };
    assert!(bad(json!({ "APP_PORT": "0" })).to_string().contains("APP_PORT: 0 is below 1"));
    assert!(bad(json!({ "APP_HOSTS": "a,b,c" })).to_string().contains("APP_HOSTS: 3 items is above 2 items"));
    assert!(bad(json!({ "APP_LEVEL": "verbose" })).to_string().contains(r#""verbose" is not one of info, debug"#));

    config.update_data(config.get().version(), &json!({ "rps": 200 })).unwrap();
    assert_eq!(config.get()["rps"], json!(200));
    config.update_data(config.get().version(), &json!({ "rps": null })).unwrap();
    assert_eq!(config.get()["rps"], Value::Null, "a live change gives back none");
}

// contract: CFG-15
#[test]
fn a_setting_sent_without_a_default_is_refused_naming_null_as_none() {
    let issue = |password: Value| declaration(&json!({ "db": { "group": { "password": password } } }));
    assert!(issue(json!({ "kind": "text", "secret": true, "default": null })).is_ok());
    let error = issue(json!({ "kind": "text", "secret": true })).unwrap_err();
    assert_eq!(error, r#"db.password: a setting has a default, "default": null for none, or is required"#);
    let item =
        json!({ "kind": { "list": { "group": { "port": { "kind": "integer", "default": null } } } }, "default": [{}] });
    assert_eq!(
        declaration(&json!({ "x": item })).unwrap().setting("x").unwrap().default,
        Some(json!([{ "port": null }]))
    );
    let chooser = json!({
        "plugin": { "kind": "text", "default": null },
        "settings": { "chosenBy": "plugin", "groups": { "postgres": { "port": { "kind": "integer", "default": 5432 } } } }
    });
    assert!(declaration(&chooser).is_ok(), "a chooser with no value chooses no group until a layer names one");
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

// contract: CFG-16, CFG-06
#[test]
fn a_file_sent_that_refuses_secrets_refuses_the_one_it_gives() {
    let f = fixture();
    let file = |secrets: bool| {
        let text = "[db]\nurl = \"postgres://secret\"\n";
        json!({ "file": { "name": "app.toml", "text": text, "secrets": secrets } })
    };
    let error = refused(opened(&f, &json!([file(false), { "env": { "prefix": "APP" } }]))).to_string();
    let said = "app.toml: db.url: a secret is not read from app.toml; set APP_DB_URL or APP_DB_URL_FILE";
    assert!(error.contains(said) && !error.contains("postgres://secret"), "{error}");
    assert_eq!(opened(&f, &json!([file(true)])).unwrap().get()["db"]["url"], "postgres://secret");
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

/// A map of entries as the SDKs write it: what an older release reads as a
/// map given whole, its entries' group beside it.
fn sites() -> Value {
    json!({
        "instance": {
            "kind": "object",
            "entries": {
                "plugin": { "kind": "text", "default": "postgres" },
                "settings": { "group": {
                    "host": { "kind": "text", "default": "localhost" },
                    "password": { "kind": "text", "secret": true, "required": true },
                    "timeout": { "kind": "duration", "default": "30s" }
                } }
            },
            "default": { "local": { "settings": { "password": "dev-only" } } }
        }
    })
}

// contract: CFG-11
#[test]
fn entries_sent_as_data_take_each_field_from_the_layers_and_kv_keeps_them_without_secrets() {
    let f = fixture();
    let declared = sites();
    let toml = "[instance.aimee.settings]\nhost = \"127.0.0.1\"\n";
    let layers = json!([
        { "file": { "name": "dashbin.toml", "text": toml, "optional": false } },
        { "env": { "prefix": "APP", "variables": { "APP_INSTANCE_AIMEE_SETTINGS_PASSWORD": "s3cret" } } }
    ]);
    let config = sent(&f, "dashbin", &declared, &layers).unwrap();
    let aimee =
        json!({ "plugin": "postgres", "settings": { "host": "127.0.0.1", "password": "s3cret", "timeout": 30_000 } });
    assert_eq!(config.get()["instance"]["aimee"], aimee);
    assert_eq!(config.get()["instance"]["local"]["settings"]["password"], "dev-only");
    let password = config.explain().settings.into_iter().find(|line| line.path == "instance.aimee.settings.password");
    assert_eq!(password.unwrap().value, "***");

    let read = declaration(&declared).unwrap();
    let kept = read.to_json();
    assert_eq!(kept["instance"]["default"], json!({ "local": { "settings": {} } }), "no secret is kept");
    let again = declaration(&kept).unwrap();
    assert!(again.setting("instance.kazan.settings.password").is_some_and(|setting| setting.secret));
    assert_eq!(again.setting("instance.kazan.plugin"), read.setting("instance.kazan.plugin"));

    let refused = |instance: Value| declaration(&json!({ "instance": instance })).unwrap_err();
    let live = json!({ "kind": "object", "entries": { "size": { "kind": "integer", "default": 1, "live": true } } });
    assert_eq!(refused(live), "instance.*.size: an entry's field is not live: the layers give it");
    let item =
        json!({ "kind": { "list": { "group": { "inner": { "kind": "object", "entries": {} } } } }, "default": [] });
    assert_eq!(refused(item), "instance[].inner: entries stand among settings, not in an entry or a list's item");
}

/// dashbin's instances as the SDKs write them: each entry's settings are the
/// group of the plugin its `plugin` names.
fn plugins() -> Value {
    json!({
        "instance": {
            "kind": "object",
            "default": {},
            "entries": {
                "plugin": { "kind": "text", "required": true },
                "settings": {
                    "kind": "object",
                    "default": {},
                    "chosenBy": "plugin",
                    "groups": {
                        "postgres": {
                            "host": { "kind": "text", "default": "localhost" },
                            "port": { "kind": "integer", "default": 5432 },
                            "password": { "kind": "text", "secret": true, "required": true }
                        },
                        "stripe": { "key": { "kind": "text", "secret": true, "required": true } }
                    }
                }
            }
        }
    })
}

// contract: CFG-14
#[test]
fn a_chosen_group_sent_as_data_reads_back_as_kv_keeps_it_and_an_older_release_reads_a_map() {
    let read = declaration(&plugins()).unwrap();
    assert!(read.setting("instance.aimee.settings.password").is_some_and(|setting| setting.secret));
    let plugin = read.setting("instance.aimee.plugin").unwrap();
    assert_eq!(plugin.rules.one_of, Some(vec!["postgres".to_owned(), "stripe".to_owned()]), "its groups' names");
    let kept = read.to_json();
    let settings = &kept["instance"]["entries"]["settings"];
    assert_eq!((&settings["kind"], &settings["default"]), (&json!("object"), &json!({})), "a map given whole");
    assert_eq!(kept["instance"]["entries"]["plugin"]["oneOf"], json!(["postgres", "stripe"]));
    assert_eq!(declaration(&kept).unwrap(), read, "what kv.db keeps reads back the same");
    let mut bare = plugins();
    let bare_settings = bare["instance"]["entries"]["settings"].as_object_mut().unwrap();
    bare_settings.remove("kind");
    bare_settings.remove("default");
    assert_eq!(declaration(&bare).unwrap(), read, "kind and default are an older release's alone");
}

// contract: CFG-14
#[test]
fn a_chosen_group_sent_refuses_what_means_nothing() {
    let refused = |site: Value| declaration(&json!({ "instance": { "entries": site } })).unwrap_err();
    let text = json!({ "kind": "text", "default": "a" });
    let chosen = json!({ "chosenBy": "plugin", "groups": { "a": {} } });
    assert_eq!(
        refused(json!({ "plugn": text, "settings": chosen })),
        "instance.*.settings: instance.*.plugin, which chooses its group, is no setting; did you mean instance.*.plugn?"
    );
    let secret = json!({ "kind": "text", "secret": true, "required": true });
    assert_eq!(
        refused(json!({ "plugin": secret, "settings": chosen })),
        "instance.*.settings: instance.*.plugin chooses its group, so it is text, neither secret nor live"
    );
    let allowed = json!({ "kind": "text", "default": "a", "oneOf": ["a", "b"] });
    assert_eq!(
        refused(json!({ "plugin": allowed, "settings": chosen })),
        r#"instance.*.plugin: "b" names none of the groups of instance.*.settings"#
    );
    let mysql = json!({ "kind": "text", "default": "mysql" });
    assert_eq!(
        refused(json!({ "plugin": mysql, "settings": chosen })),
        r#"instance.*.plugin: its default: "mysql" is not one of a"#
    );
    let size = json!({ "kind": "integer", "default": 1, "live": true });
    let live = json!({ "chosenBy": "plugin", "groups": { "a": { "size": size } } });
    assert_eq!(
        refused(json!({ "plugin": text, "settings": live })),
        "instance.*.settings(a).size: a chosen group's field is not live: the layers give it"
    );
    assert_eq!(
        refused(json!({ "plugin": text, "settings": { "chosenBy": "plugin" } })),
        "instance.*.settings: its groups are a map from each value of plugin to its group"
    );
    let nested = json!({ "chosenBy": "plugin", "groups": { "a": { "inner": chosen } } });
    assert_eq!(
        refused(json!({ "plugin": text, "settings": nested })),
        "instance.*.settings(a).inner: a chosen group stands among settings, not in a list's item or a chosen group"
    );
    let item = json!({ "kind": { "list": { "group": { "plugin": text, "settings": chosen } } }, "default": [] });
    assert_eq!(
        declaration(&json!({ "sites": item })).unwrap_err(),
        "sites[].settings: a chosen group stands among settings, not in a list's item or a chosen group"
    );
}

// contract: CFG-14, CFG-06
#[test]
fn a_chosen_group_sent_gives_its_value_as_data_read_as_the_group_its_chooser_names() {
    let f = fixture();
    let declared = json!({
        "plugin": { "kind": "text", "default": "postgres" },
        "settings": plugins()["instance"]["entries"]["settings"].clone(),
        "instance": plugins()["instance"].clone()
    });
    let toml = "plugin = \"stripe\"\n[instance.aimee]\nplugin = \"postgres\"\n[instance.aimee.settings]\nport = 6432\n";
    let variables = json!({ "APP_SETTINGS_KEY": "sk_test", "APP_INSTANCE_AIMEE_SETTINGS_PASSWORD": "s3cret" });
    let layers = json!([
        { "file": { "name": "dashbin.toml", "text": toml } },
        { "env": { "prefix": "APP", "variables": variables } }
    ]);
    let config = sent(&f, "dashbin", &declared, &layers).unwrap();
    assert_eq!(config.get()["settings"], json!({ "key": "sk_test" }));
    let aimee =
        json!({ "plugin": "postgres", "settings": { "host": "localhost", "port": 6432, "password": "s3cret" } });
    assert_eq!(config.get()["instance"]["aimee"], aimee);
    let printed = config.explain().to_string();
    assert!(printed.contains("settings.key") && !printed.contains("sk_test"), "{printed}");
    assert!(!printed.contains("s3cret"), "{printed}");
}
