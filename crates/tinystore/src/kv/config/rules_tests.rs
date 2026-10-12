use serde_json::{Value, json};

use super::*;

const VECTORS: &str = include_str!("../../../../../testdata/config/env.json");

fn setting(kind: Kind, rules: Rules) -> Setting {
    Setting { kind, rules, default: None, required: false, live: false, secret: false }
}

#[test]
fn a_span_reads_as_every_language_reads_it_and_spells_back() {
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();
    for span_case in vectors["spans"].as_array().unwrap() {
        let (text, want) = (span_case[0].as_str().unwrap(), span_case[1].as_u64());
        assert_eq!(span(text).ok(), want, "{text:?}");
        if let Some(millis) = want {
            assert_eq!(span(&spelled(millis)), Ok(millis), "{text:?} spelled back");
        }
    }
    assert_eq!(spelled(5_400_000), "1h30m");
    assert_eq!(spelled(90_500), "1m30s500ms");
}

#[test]
fn a_whole_number_is_whole_and_a_rule_says_what_it_refused() {
    let port = setting(Kind::Integer, Rules { min: Some(1.0), max: Some(65535.0), one_of: None });
    assert_eq!(check(&port, &json!(8080)), Ok(json!(8080)));
    assert_eq!(check(&port, &json!(8080.0)), Ok(json!(8080)), "a whole float is a whole number");
    assert_eq!(check(&port, &json!(80.5)), Err("80.5 is not a whole number".to_owned()));
    assert_eq!(check(&port, &json!(70000)), Err("70000 is above 65535".to_owned()));
    assert_eq!(check(&port, &json!(0)), Err("0 is below 1".to_owned()));
    assert_eq!(check(&port, &json!("8080")), Err(r#""8080" is not a whole number"#.to_owned()));
}

#[test]
fn one_of_names_the_set_and_a_span_its_bound_spelled() {
    let allowed = Some(vec!["info".to_owned(), "debug".to_owned()]);
    let level = setting(Kind::Text, Rules { one_of: allowed, ..Rules::default() });
    assert_eq!(check(&level, &json!("debug")), Ok(json!("debug")));
    assert_eq!(check(&level, &json!("verbose")), Err(r#""verbose" is not one of info, debug"#.to_owned()));
    let timeout = setting(Kind::Duration, Rules { max: Some(3_600_000.0), ..Rules::default() });
    assert_eq!(check(&timeout, &json!("30s")), Ok(json!(30_000)));
    assert_eq!(check(&timeout, &json!("2h")), Err("2h is above 1h".to_owned()));
    assert_eq!(check(&timeout, &json!("soon")), Err(r#""soon" is not a span, as 30s or 1h30m"#.to_owned()));
}

#[test]
fn a_lists_items_are_checked_each_its_length_counted_and_a_group_item_filled() {
    let items = Item::Value { kind: Kind::Integer, rules: Rules { min: Some(1.0), ..Rules::default() } };
    let ports = setting(Kind::List(Box::new(items)), Rules { max: Some(2.0), ..Rules::default() });
    assert_eq!(check(&ports, &json!([80, 443])), Ok(json!([80, 443])));
    assert_eq!(check(&ports, &json!([80, 0])), Err("[1]: 0 is below 1".to_owned()));
    assert_eq!(check(&ports, &json!([1, 2, 3])), Err("3 items is above 2 items".to_owned()));
    let host = Setting { required: true, ..setting(Kind::Text, Rules::default()) };
    let port = Setting { default: Some(json!(80)), ..setting(Kind::Integer, Rules::default()) };
    let group = Group::from([("host".to_owned(), Node::Setting(host)), ("port".to_owned(), Node::Setting(port))]);
    let upstreams = setting(Kind::List(Box::new(Item::Group(group))), Rules::default());
    assert_eq!(check(&upstreams, &json!([{ "host": "a" }])), Ok(json!([{ "host": "a", "port": 80 }])));
    assert_eq!(check(&upstreams, &json!([{ "port": 443 }])), Err("[0].host is required".to_owned()));
    assert_eq!(check(&upstreams, &json!([{ "host": "a", "prot": 1 }])), Err("[0].prot: no such setting".to_owned()));
}

#[test]
fn a_variables_text_reads_as_its_settings_kind() {
    let list =
        setting(Kind::List(Box::new(Item::Value { kind: Kind::Number, rules: Rules::default() })), Rules::default());
    assert_eq!(parse(&list, "1.5, 2"), Ok(json!([1.5, 2.0])));
    assert_eq!(parse(&list, "[3]"), Ok(json!([3])));
    assert_eq!(parse(&list, ""), Ok(json!([])));
    let whole = setting(Kind::Integer, Rules::default());
    assert_eq!(parse(&whole, "80.5"), Err(r#""80.5" is not a whole number"#.to_owned()));
    assert_eq!(parse(&whole, "eighty"), Err(r#""eighty" is not a number"#.to_owned()));
    let any = setting(Kind::Any, Rules::default());
    assert_eq!(parse(&any, r#"{"a":1}"#), Ok(json!({ "a": 1 })));
    assert_eq!(parse(&any, "plain"), Ok(json!("plain")));
    let flag = setting(Kind::Boolean, Rules::default());
    assert_eq!((parse(&flag, "1"), parse(&flag, "False")), (Ok(json!(true)), Ok(json!(false))));
}
