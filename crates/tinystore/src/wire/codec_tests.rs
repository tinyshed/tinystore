//! Every message of testdata/wire/protocol.json, which crates/protocol writes
//! from the schema and every SDK reads too, and the codec's refusals.

use serde_json::Value as Json;

use super::*;
use crate::wire::msgpack::tree::{self, Value};
use crate::wire::protocol::{self, KvCall, KvEntry, KvPage};

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

#[test]
fn every_vector_of_the_schema_reads_and_writes_its_own_bytes() {
    let vectors: Json = serde_json::from_str(include_str!("../../../../testdata/wire/protocol.json")).unwrap();
    let messages = vectors["messages"].as_array().unwrap();
    assert!(!messages.is_empty());
    for vector in messages {
        let (name, message) = (vector["name"].as_str().unwrap(), vector["message"].as_str().unwrap());
        let bytes = hex(vector["hex"].as_str().unwrap());
        let rewritten = protocol::rewrite(message, &bytes).unwrap_or_else(|| panic!("{name}: no message {message}"));
        let (rewritten, size) = rewritten.unwrap();
        assert_eq!(rewritten, bytes, "{name}");
        // what encode reserves holds the message, so that no answer's buffer grows as it is written
        assert!(rewritten.len() <= size, "{name}: {} bytes, past the {size} its size says", rewritten.len());
    }
}

#[test]
fn a_field_of_another_type_is_refused_by_its_name() {
    let body = tree::encode(&Value::Map(vec![(Value::Uint(3), Value::Bool(true))]));
    let refused = KvCall::decode(&body).unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("invalid", "key is not a str, a bin or an integer"));
}

#[test]
fn a_value_reads_as_the_row_any_language_wrote() {
    let read = |value: Value| {
        let body = tree::encode(&Value::Map(vec![(Value::Uint(2), value)]));
        KvEntry::decode(&body).map(|entry| entry.value)
    };
    assert_eq!(read(Value::Uint(5)).unwrap(), Some(Row::Int(5)));
    assert_eq!(read(Value::Int(-5)).unwrap(), Some(Row::Int(-5)));
    assert_eq!(read(Value::Bin(vec![1, 255])).unwrap(), Some(Row::Bin(vec![1, 255])));
    assert_eq!(read(Value::Nil).unwrap(), Some(Row::Nil));
    assert_eq!(read(Value::Uint(u64::MAX)).unwrap_err().code, "invalid", "past an int 64");
    assert_eq!(read(Value::Str("five".to_owned())).unwrap_err().code, "invalid", "a str is not a row");
}

#[test]
fn a_zero_is_left_out_unless_it_was_given() {
    let entry = KvEntry { found: false, value: Some(Row::Int(0)), ..KvEntry::default() };
    let pairs = |entry: &KvEntry| match tree::decode(&entry.encode()).unwrap() {
        Value::Map(pairs) => pairs,
        other => panic!("{other:?}"),
    };
    assert_eq!(pairs(&entry), vec![(Value::Uint(2), Value::Uint(0))], "found is false, the value 0 was given");
    assert_eq!(pairs(&KvEntry::default()), vec![]);
}

#[test]
fn a_message_is_refused_for_the_rule_its_body_breaks() {
    let unimplemented = format!("field 99 of kv.Call: not in tinystore {}", env!("CARGO_PKG_VERSION"));
    let cases: [(&str, &str, &str); 9] = [
        ("", "invalid", "an empty body"),
        ("01", "invalid", "a kv.Call that is not a map"),
        ("81a16b01", "invalid", "a kv.Call with a field named rather than numbered"),
        ("81c001", "invalid", "a key that is neither an unsigned integer nor a name"),
        ("8201010102", "invalid", "a key twice"),
        ("816301", "unimplemented", &unimplemented),
        ("810101c0", "invalid", "bytes after the value"),
        ("deffff01", "invalid", "a count past the bytes left"),
        ("8103a2c328", "invalid", "a str that is not UTF-8"),
    ];
    for (body, code, message) in cases {
        let refused = KvCall::decode(&hex(body)).unwrap_err();
        assert_eq!((refused.code.as_str(), refused.message.as_str()), (code, message), "{body}");
    }
}

#[test]
fn a_value_the_profile_refuses_is_refused_in_a_message_too() {
    let vectors: Json = serde_json::from_str(include_str!("../../../../testdata/wire/vectors.json")).unwrap();
    for vector in vectors["refused"].as_array().unwrap().iter().filter(|vector| vector["as"] == "any") {
        let mut body = vec![0x81, 0x03];
        body.extend(hex(vector["hex"].as_str().unwrap()));
        let refused = KvCall::decode(&body).expect_err(vector["name"].as_str().unwrap());
        assert_eq!(refused.code, "invalid", "{}", vector["name"]);
    }
}

#[test]
fn a_map_of_names_is_refused_for_a_name_twice_or_a_number() {
    let failure = |what: Vec<(Value, Value)>| {
        let body =
            Value::Map(vec![(Value::Uint(1), Value::Str("invalid".to_owned())), (Value::Uint(3), Value::Map(what))]);
        Failure::decode(&tree::encode(&body))
    };
    let named = failure(vec![(Value::Str("key".to_owned()), Value::Str("a".to_owned()))]).unwrap();
    assert_eq!(named.what, Some([("key".to_owned(), "a".to_owned())].into()));
    let numbered = failure(vec![(Value::Uint(1), Value::Str("a".to_owned()))]).unwrap_err();
    assert_eq!(numbered.message, "what is not a map whose keys are names");
    let name = |text: &str| Value::Str(text.to_owned());
    let twice = failure(vec![(name("key"), name("a")), (name("key"), name("b"))]).unwrap_err();
    assert_eq!(twice.message, "a key twice");
}

#[test]
fn a_map_of_more_than_fifteen_fields_writes_the_head_a_map_16_has() {
    let mut out = vec![0xff];
    let mut map = Map::open(&mut out);
    for number in 1..=16u8 {
        map.field(&mut out, number);
        msgpack::write_uint(&mut out, u64::from(number));
    }
    map.close(&mut out);
    let pairs = (1..=16).map(|number| (Value::Uint(number), Value::Uint(number))).collect();
    let mut expected = vec![0xff];
    expected.extend(tree::encode(&Value::Map(pairs)));
    assert_eq!(out, expected, "the fields moved up past a head of three bytes, what came before them kept");
}

#[test]
fn a_message_reserves_what_it_writes_however_large_its_values() {
    fn reserved(message: &impl Message, name: &str) {
        let (written, size) = (message.encode(), message.size());
        assert!(written.len() <= size, "{name}: {} bytes written, past the {size} reserved", written.len());
        assert_eq!(written.capacity(), size, "{name}: the buffer grew as it was written");
    }
    let text = |n: usize| "é".repeat(n);
    let entry = KvEntry {
        found: true,
        value: Some(Row::Bin(vec![7; 70_000])),
        version: Some(vec![1; 300]),
        expires_at: Some(-1),
        key: Some(text(40_000)),
    };
    reserved(&entry, "kv.Entry");
    reserved(&KvPage { entries: vec![entry; 20], next: Some(text(300)) }, "kv.Page");
    let call = KvCall {
        handle: u64::MAX,
        under: vec![text(100); 300],
        key: text(70_000),
        value: Some(Row::Int(i64::MIN)),
        if_version: Some(vec![0; 70_000]),
        n: Some(i64::MIN),
        ..KvCall::default()
    };
    reserved(&call, "kv.Call");
    let what = (0..300).map(|n| (format!("name {n}"), text(n))).collect();
    reserved(&Failure { code: text(20), message: text(70_000), what: Some(what) }, "Failure");
    #[cfg(feature = "sql")]
    {
        let row =
            vec![Cell::Str(text(300)), Cell::Bin(vec![1; 300]), Cell::Float(-0.0), Cell::Int(i64::MIN), Cell::Nil];
        let rows = protocol::SqlRows { columns: vec![text(30); 5], rows: vec![row; 2_000] };
        reserved(&rows, "sql.Rows");
    }
}
