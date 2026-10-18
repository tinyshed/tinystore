use serde_json::{Value, json};

use super::*;

fn declaration() -> Declaration {
    let value = json!({ "port": 8080, "origins": ["localhost"], "limits": { "rps": 100 }, "name": "x" });
    Declaration::of_marked(&value).unwrap()
}

fn read_text(name: &str, text: &str) -> Result<Vec<(String, Value)>, Vec<String>> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, text).unwrap();
    read(&File::from(path), &declaration()).map(|read| read.given)
}

#[test]
fn toml_yaml_and_json_with_comments_give_the_same_settings() {
    let want = vec![
        ("limits.rps".to_owned(), json!(5)),
        ("name".to_owned(), json!("no")),
        ("origins".to_owned(), json!(["a.com"])),
        ("port".to_owned(), json!(9000)),
    ];
    let toml = "port = 9000\nname = \"no\"\norigins = [\"a.com\"]\n[limits]\nrps = 5\n";
    let yaml = "port: 9000\nname: no # YAML 1.2: text, not false\norigins: [a.com]\nlimits:\n  rps: 5\n";
    let jsonc = r#"{
  // the port
  "port": 9000, "name": "no",
  /* a list */ "origins": ["a.com",],
  "limits": { "rps": 5, },
}
"#;
    assert_eq!(read_text("app.toml", toml), Ok(want.clone()));
    assert_eq!(read_text("app.yaml", yaml), Ok(want.clone()));
    assert_eq!(read_text("app.jsonc", jsonc), Ok(want.clone()));
    assert_eq!(read_text("app.json", jsonc), Ok(want), "a .json file takes comments too");
}

#[test]
fn a_key_no_setting_has_and_a_value_of_another_kind_are_each_named() {
    let refused = read_text("app.yaml", "prot: 1\nport: eighty\nlimits: 3\n").unwrap_err();
    let shown: Vec<String> =
        refused.iter().map(|problem| problem.rsplit_once(".yaml: ").unwrap().1.to_owned()).collect();
    assert_eq!(
        shown,
        [
            r#"limits: 3 is not a group of settings"#,
            r#"port: "eighty" is not a whole number"#,
            "prot: no such setting; did you mean port?"
        ]
    );
}

#[test]
fn an_empty_key_declares_nothing_and_an_unknown_extension_is_refused() {
    assert_eq!(read_text("app.yaml", "port:\nname: n\n"), Ok(vec![("name".to_owned(), json!("n"))]));
    assert_eq!(read_text("app.yaml", ""), Ok(Vec::new()));
    let refused = read_text("app.ini", "port = 1").unwrap_err();
    assert!(refused[0].ends_with("app.ini: not .toml, .yaml, .yml, .json or .jsonc"), "{refused:?}");
}

#[test]
fn a_missing_file_is_refused_unless_optional() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.yaml");
    let refused = read(&File::from(path.clone()), &declaration()).unwrap_err();
    assert!(refused[0].ends_with("absent.yaml: no such file; File::optional lets it be absent"), "{refused:?}");
    assert!(read(&File::optional(path), &declaration()).unwrap().absent);
}

#[test]
fn comments_and_trailing_commas_leave_every_line_where_it_was() {
    let text = "{\n  \"a\": \"// not a comment\", // one\n  /* two\n  lines */ \"b\": [1,],\n}";
    let cleaned = without_comments(text);
    assert_eq!(cleaned.lines().count(), text.lines().count());
    let value: Value = serde_json::from_str(&cleaned).unwrap();
    assert_eq!(value, json!({ "a": "// not a comment", "b": [1] }));
}

#[test]
fn an_infinite_toml_number_is_refused_rather_than_read_as_unset() {
    let refused = read_text("app.toml", "port = inf\n").unwrap_err();
    assert!(refused[0].contains("port"), "{refused:?}");
}
