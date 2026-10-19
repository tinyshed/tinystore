use serde_json::{Value, json};

use super::*;
use crate::kv::config::declaration::Declaration;

const VECTORS: &str = include_str!("../../../../../testdata/config/env.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).unwrap()
}

// contract: CFG-08
#[test]
fn a_settings_variable_is_its_prefix_and_its_path_in_upper_snake_case_as_every_language_spells_it() {
    for name in vectors()["names"].as_array().unwrap() {
        let (prefix, path, want) = (name[0].as_str().unwrap(), name[1].as_str().unwrap(), name[2].as_str().unwrap());
        assert_eq!(prefixed(prefix, path), want, "{prefix} {path}");
    }
}

// contract: CFG-08
#[test]
fn a_dotenv_file_reads_as_every_language_reads_it() {
    for case in vectors()["dotenv"].as_array().unwrap() {
        let (name, text) = (&case["name"], case["text"].as_str().unwrap());
        match case.get("refused") {
            Some(line) => {
                let refused = dotenv(text).unwrap_err();
                assert!(refused.starts_with(&format!("line {line}:")), "{name}: {refused}");
            }
            None => {
                let read: serde_json::Map<String, Value> =
                    dotenv(text).unwrap().into_iter().map(|(name, value)| (name, Value::String(value))).collect();
                assert_eq!(Value::Object(read), case["want"], "{name}");
            }
        }
    }
}

fn declaration(value: Value) -> Declaration {
    Declaration::of_marked(&value).unwrap()
}

#[test]
fn a_variable_reads_as_its_settings_kind_and_one_that_does_not_is_named() {
    let declared = declaration(json!({ "port": 8080, "origins": ["localhost"], "debug": false, "name": "x" }));
    let env = Env::prefix("APP").from([
        ("APP_PORT", " 9000 "),
        ("APP_ORIGINS", "a.com, b.com"),
        ("APP_DEBUG", "TRUE"),
        ("APP_NAME", " spaced "),
    ]);
    let read = read(&env, &declared).unwrap();
    let given: Vec<(String, Value)> = read.given.into_iter().map(|(path, value, _, _)| (path, value)).collect();
    assert_eq!(
        given,
        [
            ("debug".to_owned(), json!(true)),
            ("name".to_owned(), json!(" spaced ")),
            ("origins".to_owned(), json!(["a.com", "b.com"])),
            ("port".to_owned(), json!(9000)),
        ]
    );
    let refused = read_refused(&Env::prefix("APP").from([("APP_PORT", "eighty"), ("APP_DEBUG", "yes")]), &declared);
    assert_eq!(refused, [r#"APP_DEBUG: "yes" is not true or false"#, r#"APP_PORT: "eighty" is not a number"#]);
}

fn read_refused(env: &Env, declared: &Declaration) -> Vec<String> {
    let mut refused = read(env, declared).unwrap_err();
    refused.sort();
    refused
}

// contract: CFG-03
#[test]
fn a_name_given_a_setting_replaces_its_prefixed_one_and_two_settings_never_share_one() {
    let declared = declaration(json!({ "port": 8080, "db": { "url": "" }, "dbUrl": "" }));
    let env = Env::prefix("APP").var("port", "PORT").var("db.url", "DATABASE_URL").from([
        ("PORT", "3000"),
        ("APP_PORT", "1"),
        ("DATABASE_URL", "postgres://x"),
    ]);
    let read = read(&env, &declared).unwrap();
    let shown: Vec<&str> = read.given.iter().map(|(_, _, shown, _)| shown.as_str()).collect();
    assert_eq!(shown, ["DATABASE_URL", "PORT"]);
    let unused: Vec<String> = read.unused.iter().map(ToString::to_string).collect();
    assert_eq!(
        unused,
        ["APP_PORT: a variable with the prefix that names no setting"],
        "a prefixed variable no setting reads is said"
    );
    let clash = read_refused(&Env::prefix("APP"), &declared);
    assert_eq!(clash, ["APP_DB_URL would give both db.url and dbUrl"]);
    let typo = read_refused(&Env::prefix("APP").var("db.ulr", "X"), &declaration(json!({ "db": { "url": "" } })));
    assert_eq!(typo, ["a variable is named for db.ulr, which is no setting; did you mean db.url?"]);
}

#[test]
fn name_file_gives_its_files_text_without_its_last_newline_and_both_set_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("db");
    std::fs::write(&secret, "postgres://x\r\n").unwrap();
    let declared = declaration(json!({ "db": { "url": "" } }));
    let path = secret.display().to_string();
    let read = read(&Env::prefix("APP").from([("APP_DB_URL_FILE", path.as_str())]), &declared).unwrap();
    let (_, value, shown, origin) = &read.given[0];
    assert_eq!(
        (value, shown.as_str(), origin),
        (&json!("postgres://x"), "APP_DB_URL_FILE", &Origin::File(path.clone()))
    );
    let both = Env::prefix("APP").from([("APP_DB_URL_FILE", path.as_str()), ("APP_DB_URL", "y")]);
    assert_eq!(read_refused(&both, &declared), ["APP_DB_URL and APP_DB_URL_FILE are both set"]);
}

// contract: CFG-03
#[test]
fn a_dotenv_files_variables_count_where_the_process_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(".env");
    std::fs::write(&file, "APP_PORT=1\nAPP_NAME=from-file\nAPP_PROT=2\n").unwrap();
    let declared = declaration(json!({ "port": 8080, "name": "" }));
    let env = Env::prefix("APP").files([file.clone(), dir.path().join("absent.env")]).from([("APP_PORT", "2")]);
    let read = read(&env, &declared).unwrap();
    let given: Vec<(&str, &Value, &Origin)> =
        read.given.iter().map(|(path, value, _, origin)| (path.as_str(), value, origin)).collect();
    let shown = file.display().to_string();
    assert_eq!(given, [("name", &json!("from-file"), &Origin::DotEnv(shown)), ("port", &json!(2), &Origin::Process)]);
    let unused: Vec<String> = read.unused.iter().map(ToString::to_string).collect();
    assert_eq!(unused, ["APP_PROT: a variable with the prefix that names no setting; did you mean APP_PORT?"]);
}

#[test]
fn a_settings_name_file_variable_that_another_setting_reads_is_refused() {
    let declared = declaration(json!({ "db": "", "dbFile": "" }));
    let refused = read_refused(&Env::prefix("APP"), &declared);
    assert_eq!(refused, ["APP_DB_FILE would give both db and dbFile"]);
}
