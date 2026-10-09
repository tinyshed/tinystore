//! Every value and refusal of testdata/wire/vectors.json, which the SDKs read
//! too.

use serde_json::Value as Json;

use super::*;

fn vectors() -> Json {
    let text = include_str!("../../../../testdata/wire/vectors.json");
    serde_json::from_str(text).unwrap()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap()).collect()
}

/// A vector's value in the file's notation, as a profile value.
fn expected(json: &Json) -> Value {
    match json {
        Json::Null => Value::Nil,
        Json::Bool(flag) => Value::Bool(*flag),
        Json::Object(typed) => {
            let (kind, inner) = typed.iter().next().unwrap();
            match (kind.as_str(), inner) {
                ("uint", Json::String(text)) => Value::Uint(text.parse().unwrap()),
                ("int", Json::String(text)) => signed(text.parse().unwrap()),
                ("float", Json::String(bits)) => Value::Float(f64::from_bits(u64::from_str_radix(bits, 16).unwrap())),
                ("str", Json::String(text)) => Value::Str(text.clone()),
                ("bin", Json::String(bytes)) => Value::Bin(hex(bytes)),
                ("array", Json::Array(items)) => Value::Array(items.iter().map(expected).collect()),
                ("map", Json::Array(pairs)) => {
                    Value::Map(pairs.iter().map(|pair| (expected(&pair[0]), expected(&pair[1]))).collect())
                }
                other => panic!("a value of kind {other:?}"),
            }
        }
        other => panic!("a value {other}"),
    }
}

/// A decoded value read as the type `want` names, as a message's field is.
fn read_as(value: &Value, want: &str) -> Option<Value> {
    match want {
        "any" => Some(value.clone()),
        "uint" => value.as_uint().map(Value::Uint),
        "int" => value.as_int().map(signed),
        "float" => value.as_float().map(Value::Float),
        "str" => value.as_str().map(|text| Value::Str(text.to_owned())),
        "bin" => value.as_bin().map(|bytes| Value::Bin(bytes.to_vec())),
        "map" => matches!(value, Value::Map(_)).then(|| value.clone()),
        other => panic!("a type {other}"),
    }
}

fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Object(typed) => match typed.keys().next().unwrap().as_str() {
            "uint" => "uint",
            "int" => "int",
            "float" => "float",
            "str" => "str",
            "bin" => "bin",
            _ => "any",
        },
        _ => "any",
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b)),
        (Value::Map(a), Value::Map(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|((ak, av), (bk, bv))| same(ak, bk) && same(av, bv))
        }
        _ => a == b,
    }
}

#[test]
fn every_value_vector_decodes_and_encodes_canonically() {
    for vector in vectors()["values"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let bytes = hex(vector["hex"].as_str().unwrap());
        let want = expected(&vector["value"]);
        let decoded = decode(&bytes).unwrap_or_else(|refusal| panic!("{name}: refused, {refusal}"));
        let read = read_as(&decoded, kind_of(&vector["value"])).unwrap_or_else(|| panic!("{name}: not its type"));
        assert!(same(&read, &want), "{name}: {read:?} is not {want:?}");
        let canonical = vector.get("canonical").and_then(Json::as_str).unwrap_or(vector["hex"].as_str().unwrap());
        assert_eq!(encode(&want), hex(canonical), "{name}: encodes canonically");
    }
}

#[test]
fn every_refused_vector_is_refused() {
    for vector in vectors()["refused"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let bytes = hex(vector["hex"].as_str().unwrap());
        let want = vector["as"].as_str().unwrap();
        let accepted = decode(&bytes).ok().and_then(|value| read_as(&value, want));
        assert!(accepted.is_none(), "{name}: accepted as {want}: {accepted:?}");
    }
}
