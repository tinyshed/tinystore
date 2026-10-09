//! Every message of testdata/wire/protocol.json, which crates/protocol writes
//! from the schema and every SDK reads too, and the codec's refusals.

use serde_json::Value as Json;

use super::*;
use crate::wire::protocol::{self, KvCall, KvEntry};

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
        assert_eq!(rewritten.unwrap(), bytes, "{name}");
    }
}

#[test]
fn a_field_of_another_type_is_refused_by_its_name() {
    let body = msgpack::encode(&Value::Map(vec![(Value::Uint(3), Value::Bool(true))]));
    let refused = KvCall::decode(&body).unwrap_err();
    assert_eq!((refused.code.as_str(), refused.message.as_str()), ("invalid", "key is not a str, a bin or an integer"));
}

#[test]
fn a_value_reads_as_the_row_any_language_wrote() {
    let read = |value: Value| {
        let body = msgpack::encode(&Value::Map(vec![(Value::Uint(2), value)]));
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
    let pairs = |entry: &KvEntry| match msgpack::decode(&entry.encode()).unwrap() {
        Value::Map(pairs) => pairs,
        other => panic!("{other:?}"),
    };
    assert_eq!(pairs(&entry), vec![(Value::Uint(2), Value::Uint(0))], "found is false, the value 0 was given");
    assert_eq!(pairs(&KvEntry::default()), vec![]);
}
