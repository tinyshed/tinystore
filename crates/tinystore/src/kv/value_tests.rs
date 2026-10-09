use serde::{Deserialize, Serialize};

use super::*;

fn round_trip<V: Value + PartialEq + fmt::Debug>(value: V, row: Raw) {
    let encoded = encode(&value).unwrap();
    assert_eq!(encoded, row, "{value:?} is kept as {row:?}");
    assert_eq!(decode::<V>(&encoded).unwrap(), value);
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
    round_trip(1u64 << 63, Raw::Bytes(vec![0x80, 0, 0, 0, 0, 0, 0, 0]));
    round_trip("hé".to_owned(), Raw::Bytes(vec![0x68, 0xc3, 0xa9]));
    round_trip(String::new(), Raw::Bytes(Vec::new()));
    round_trip(Bytes(vec![0, 1, 255]), Raw::Bytes(vec![0, 1, 255]));
    round_trip(UserId(42), Raw::Int(42));
    round_trip(None::<i64>, Raw::None);
    round_trip(Some(5i64), Raw::Int(5));
}

#[test]
fn a_float_keeps_its_bits() {
    round_trip(-0.0f64, Raw::Bytes(vec![0x80, 0, 0, 0, 0, 0, 0, 0]));
    round_trip(f64::INFINITY, Raw::Bytes(f64::INFINITY.to_bits().to_be_bytes().to_vec()));
    round_trip(1.5f32, Raw::Bytes(1.5f32.to_bits().to_be_bytes().to_vec()));

    let quiet_nan_with_payload = f64::from_bits(0x7ff8_0000_0000_0001);
    let kept = decode::<f64>(&encode(&quiet_nan_with_payload).unwrap()).unwrap();
    assert_eq!(kept.to_bits(), 0x7ff8_0000_0000_0001);
}

#[test]
fn a_compound_value_is_kept_as_json() {
    let session = Session { user: 42, device: "phone".to_owned() };
    round_trip(session, Raw::Bytes(br#"{"user":42,"device":"phone"}"#.to_vec()));
    round_trip(Plan::Free, Raw::Bytes(br#""Free""#.to_vec()));
    round_trip(Plan::Paid { seats: 3 }, Raw::Bytes(br#"{"Paid":{"seats":3}}"#.to_vec()));
    round_trip(vec![1u8, 2], Raw::Bytes(b"[1,2]".to_vec()));
}

#[test]
fn a_row_that_is_not_the_type_says_what_it_holds() {
    let mismatch = decode::<String>(&Raw::Int(3)).unwrap_err();
    assert_eq!(mismatch.0, "text from a row holding an integer");
    let mismatch = decode::<f64>(&Raw::Bytes(vec![1, 2])).unwrap_err();
    assert_eq!(mismatch.0, "an f64 from 2 bytes rather than 8");
    assert!(decode::<bool>(&Raw::Int(2)).is_err());
    assert!(decode::<u8>(&Raw::Int(300)).is_err());
}

#[test]
fn json_with_anything_after_the_value_is_refused() {
    assert!(decode::<Session>(&Raw::Bytes(br#"{"user":1,"device":"a"} x"#.to_vec())).is_err());
}

#[test]
fn a_dynamic_value_reads_whatever_the_row_holds() {
    let any: serde_json::Value = decode(&Raw::Bytes(br#"{"a":[1,2]}"#.to_vec())).unwrap();
    assert_eq!(any["a"][1], 2);
    let any: serde_json::Value = decode(&Raw::Int(9)).unwrap();
    assert_eq!(any, 9);
}
