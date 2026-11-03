//! A file that refuses secrets, as a program marks one committed beside its
//! code: every secret comes from the environment, and the file's hint for a
//! missing one is never the file.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::Result;
use crate::config::fixture::{env, written};
use crate::config::{Config, Declaration, Entries, Env, File, Secret, load};

/// The issue's settings, a plugin's as dashbin declares them.
fn declaration() -> Declaration {
    Declaration::from_json(json!({
        "db": { "group": {
            "host": { "kind": "text", "default": "localhost" },
            "password": { "kind": "text", "secret": true, "required": true }
        } }
    }))
    .unwrap()
}

fn opened(file: File, variables: &[(&str, &str)]) -> Result<Config<Value>> {
    load("app", declaration()).file(file).env(env(variables)).open()
}

// contract: CFG-16, CFG-06, CFG-03
#[test]
fn a_file_that_refuses_secrets_names_the_variable_that_gives_one_and_never_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    let toml = written(dir.path(), "app.toml", "[db]\nhost = \"db\"\npassword = \"s3cret\"\n");
    let shown = toml.display();
    let error = opened(File::optional(&toml).secrets(false), &[]).unwrap_err().to_string();
    let said = format!(
        "config app: {shown}: db.password: a secret is not read from {shown}; set APP_DB_PASSWORD or APP_DB_PASSWORD_FILE"
    );
    assert!(error.starts_with(&said) && !error.contains("s3cret"), "{error}");
    let config = opened(File::optional(&toml), &[]).unwrap();
    assert_eq!(config.get()["db"]["password"], "s3cret", "a file not marked may hold a secret");

    let toml = written(dir.path(), "app.toml", "[db]\nhost = \"db\"\n");
    let error = opened(File::from(toml.as_path()).secrets(false), &[]).unwrap_err().to_string();
    let missing = "db.password is required: set APP_DB_PASSWORD or APP_DB_PASSWORD_FILE: invalid";
    assert!(error.ends_with(missing), "the hint names no file that refuses secrets: {error}");
    let config = opened(File::from(toml.as_path()).secrets(false), &[("APP_DB_PASSWORD", "s3cret")]).unwrap();
    let db = &config.get()["db"];
    assert_eq!((db["host"].clone(), db["password"].clone()), (json!("db"), json!("s3cret")));
}

// contract: CFG-16
#[test]
fn a_secret_refused_names_every_other_layer_that_may_give_it() {
    let dir = tempfile::tempdir().unwrap();
    let committed = written(dir.path(), "app.toml", "[db]\npassword = \"s3cret\"\n");
    let local = written(dir.path(), "app.local.toml", "");
    let error = load("app", declaration())
        .file(File::from(committed.as_path()).secrets(false))
        .file(local.as_path())
        .env(env(&[]))
        .open()
        .unwrap_err()
        .to_string();
    let others = format!("db.password in {}, or set APP_DB_PASSWORD or APP_DB_PASSWORD_FILE", local.display());
    assert!(error.contains(&format!("a secret is not read from {}; {others}", committed.display())), "{error}");
    let alone = load("app", declaration()).file(File::from(committed.as_path()).secrets(false)).open();
    let error = alone.unwrap_err().to_string();
    let alone_said = ", and no other layer gives it: read the environment";
    assert!(error.contains(alone_said) && !error.contains("s3cret"), "{error}");
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Typed {
    token: Secret<String>,
    password: Option<Secret<String>>,
    site: Entries<Site>,
    mirrors: BTreeMap<String, Site>,
    port: u16,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Site {
    host: String,
    key: Secret<String>,
}

// contract: CFG-16, CFG-03
#[test]
fn a_file_that_refuses_secrets_refuses_each_secret_it_gives_whatever_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let opened = |toml: &str, variables: &[(&str, &str)]| {
        let toml = written(dir.path(), "typed.toml", toml);
        load("typed", Typed::default()).file(File::from(toml.as_path()).secrets(false)).env(env(variables)).open()
    };
    let toml = "token = \"t0ken\"\npassword = 12345\nport = 8080\n[site.aimee]\nhost = \"a\"\nkey = \"k3y\"\n";
    let error = opened(toml, &[]).err().unwrap().to_string();
    for path in ["token", "password", "site.aimee.key"] {
        assert!(error.contains(&format!("{path}: a secret is not read from")), "{path}: {error}");
    }
    assert!(!["t0ken", "12345", "k3y"].iter().any(|secret| error.contains(secret)), "{error}");
    assert!(error.contains("set APP_SITE_AIMEE_KEY or APP_SITE_AIMEE_KEY_FILE"), "{error}");

    let variables = [("APP_TOKEN", "t0ken")];
    let config = opened("port = 8080\n[mirrors.kazan]\nhost = \"k\"\n", &variables).unwrap();
    assert_eq!(config.get().mirrors["kazan"].host, "k", "a map that gives no secret its type holds is read");
    let error = opened("[mirrors.kazan]\nhost = \"k\"\nkey = \"m1rror\"\n", &variables).err().unwrap().to_string();
    let said = "mirrors: a secret is not read from";
    assert!(error.contains(said) && error.contains("set APP_MIRRORS or APP_MIRRORS_FILE"), "{error}");
    assert!(!error.contains("m1rror"), "{error}");
}

// contract: CFG-16, CFG-14
#[test]
fn a_file_that_refuses_secrets_refuses_one_a_chosen_group_holds() {
    let dir = tempfile::tempdir().unwrap();
    let postgres = json!({
        "host": { "kind": "text", "default": "localhost" },
        "password": { "kind": "text", "secret": true, "default": null }
    });
    let declaration = Declaration::from_json(json!({
        "instance": { "entries": {
            "plugin": { "kind": "text", "required": true },
            "settings": { "chosenBy": "plugin", "groups": { "postgres": postgres } }
        } }
    }))
    .unwrap();
    let text =
        "[instance.aimee]\nplugin = \"postgres\"\n[instance.aimee.settings]\nhost = \"db\"\npassword = \"s3cret\"\n";
    let toml = written(dir.path(), "dashbin.toml", text);
    let env = Env::prefix("DASHBIN").from([("DASHBIN_X", "1")]);
    let error = load("dashbin", declaration.clone()).file(File::from(toml.as_path()).secrets(false)).env(env).open();
    let error = error.unwrap_err().to_string();
    let variable = "DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD";
    let said = format!(
        "instance.aimee.settings.password: a secret is not read from {}; set {variable} or {variable}_FILE",
        toml.display()
    );
    assert!(error.contains(&said) && !error.contains("s3cret"), "{error}");
    let toml = written(dir.path(), "dashbin.toml", &text.replace("password = \"s3cret\"\n", ""));
    let config = load("dashbin", declaration).file(File::from(toml.as_path()).secrets(false)).open().unwrap();
    assert_eq!(config.get()["instance"]["aimee"]["settings"]["password"], Value::Null);
}
