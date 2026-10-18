use serde::{Deserialize, Serialize};

use super::*;

fn round_trip<V: Value + PartialEq + fmt::Debug>(value: V, row: Raw) {
    let encoded = encode(&value).unwrap();
    assert_eq!(encoded, row, "{value:?} is kept as {row:?}");
    assert_eq!(decode::<V>(&encoded).unwrap(), value);
}

/// A row of bytes: its kind's first byte, then `body`.
fn row(tag: u8, body: &[u8]) -> Raw {
    Raw::Bytes([&[tag][..], body].concat())
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    user: i64,
    device: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct UserId(i64);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Plan {
    Free,
    Paid { seats: u32 },
}

#[test]
fn a_primitive_is_kept_as_its_row() {
    round_trip((), Raw::None);
    round_trip(true, Raw::Int(1));
    round_trip(-7i64, Raw::Int(-7));
    round_trip(u32::MAX, Raw::Int(4_294_967_295));
    round_trip(1u64 << 63, row(5, &[0x80, 0, 0, 0, 0, 0, 0, 0]));
    round_trip("hé".to_owned(), row(1, &[0x68, 0xc3, 0xa9]));
    round_trip(String::new(), row(1, &[]));
    round_trip(Bytes(vec![0, 1, 255]), row(2, &[0, 1, 255]));
    round_trip(UserId(42), Raw::Int(42));
    round_trip(None::<i64>, Raw::None);
    round_trip(Some(5i64), Raw::Int(5));
}

#[test]
fn a_float_keeps_its_bits() {
    round_trip(-0.0f64, row(4, &[0x80, 0, 0, 0, 0, 0, 0, 0]));
    round_trip(f64::INFINITY, row(4, &f64::INFINITY.to_bits().to_be_bytes()));
    round_trip(1.5f32, row(6, &1.5f32.to_bits().to_be_bytes()));

    let quiet_nan_with_payload = f64::from_bits(0x7ff8_0000_0000_0001);
    let kept = decode::<f64>(&encode(&quiet_nan_with_payload).unwrap()).unwrap();
    assert_eq!(kept.to_bits(), 0x7ff8_0000_0000_0001);
}

#[test]
fn a_compound_value_is_kept_as_json() {
    let session = Session { user: 42, device: "phone".to_owned() };
    round_trip(session, row(3, br#"{"user":42,"device":"phone"}"#));
    round_trip(Plan::Free, row(3, br#""Free""#));
    round_trip(Plan::Paid { seats: 3 }, row(3, br#"{"Paid":{"seats":3}}"#));
    round_trip(vec![1u8, 2], row(3, b"[1,2]"));
}

#[test]
fn a_row_that_is_not_the_type_says_what_it_holds() {
    let mismatch = decode::<String>(&Raw::Int(3)).unwrap_err();
    assert_eq!(mismatch.0, "text from a row holding an integer");
    let mismatch = decode::<f64>(&row(4, &[1, 2])).unwrap_err();
    assert_eq!(mismatch.0, "an f64 from 2 bytes rather than 8");
    assert!(decode::<bool>(&Raw::Int(2)).is_err());
    assert!(decode::<u8>(&Raw::Int(300)).is_err());
}

#[test]
fn a_value_read_as_another_kind_than_it_was_written_is_refused_not_guessed() {
    let session = encode(&Session { user: 1, device: "a".to_owned() }).unwrap();
    assert_eq!(decode::<String>(&session).unwrap_err().0, "text from a row of JSON");
    assert_eq!(decode::<Bytes>(&session).unwrap_err().0, "bytes from a row of JSON");
    let text = encode(&"{\"user\":1,\"device\":\"a\"}".to_owned()).unwrap();
    assert_eq!(decode::<Session>(&text).unwrap_err().0, "JSON from a row of text");
    assert_eq!(decode::<Bytes>(&text).unwrap_err().0, "bytes from a row of text");
    let float = encode(&1.0f64).unwrap();
    assert_eq!(decode::<u64>(&float).unwrap_err().0, "a u64 from a row of an f64");
}

#[test]
fn json_with_anything_after_the_value_is_refused() {
    assert!(decode::<Session>(&row(3, br#"{"user":1,"device":"a"} x"#)).is_err());
}

#[test]
fn a_dynamic_value_reads_whatever_the_row_holds() {
    let any: serde_json::Value = decode(&row(3, br#"{"a":[1,2]}"#)).unwrap();
    assert_eq!(any["a"][1], 2);
    let any: serde_json::Value = decode(&Raw::Int(9)).unwrap();
    assert_eq!(any, 9);
    let any: serde_json::Value = decode(&row(1, b"hi")).unwrap();
    assert_eq!(any, "hi", "text reads as a string, not as JSON");
}

const VECTORS: &str = include_str!("../../../../testdata/kv/values.json");

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

#[test]
fn every_language_writes_and_reads_the_same_rows() {
    let vectors: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
    for case in vectors["values"].as_array().unwrap() {
        let row = Raw::Bytes(unhex(case["row"].as_str().unwrap()));
        match case["kind"].as_str().unwrap() {
            "text" => round_trip(case["text"].as_str().unwrap().to_owned(), row),
            "bytes" => round_trip(Bytes(unhex(case["bytes"].as_str().unwrap())), row),
            "json" => round_trip(case["json"].clone(), row),
            "float" => round_trip(case["float"].as_f64().unwrap(), row),
            other => panic!("a kind {other}"),
        }
    }
    for case in vectors["refused"].as_array().unwrap() {
        let row = Raw::Bytes(unhex(case["row"].as_str().unwrap()));
        let refused = match case["read_as"].as_str().unwrap() {
            "text" => decode::<String>(&row).unwrap_err(),
            "json" => decode::<Session>(&row).unwrap_err(),
            "bytes" => decode::<Bytes>(&row).unwrap_err(),
            "float" => decode::<f64>(&row).unwrap_err(),
            other => panic!("a kind {other}"),
        };
        assert!(refused.0.contains(case["says"].as_str().unwrap()), "{}", refused.0);
    }
}
