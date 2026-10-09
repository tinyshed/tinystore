//! testdata/wire/protocol.json: a vector of every message, each field at a
//! value of its type and then none at all, its bytes as the profile writes
//! them canonically. The server and every SDK decode each vector's bytes to
//! its fields and encode the fields back to the bytes.
//!
//! The bytes are written here, by an encoder of the generator's own, so that
//! the server's codec is checked against the vectors as every SDK's is.

use serde_json::{Map, Value as Json, json};

use super::schema::{Field, Kind, Message, Schema};

/// A value of a field, as a vector holds it.
#[derive(Clone, Debug)]
enum Sample {
    Bool(bool),
    Uint(u64),
    Int(i64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
    List(Vec<Sample>),
    Names(Vec<(String, Sample)>),
    Fields(Vec<(u64, String, Sample)>),
}

pub(crate) fn write(schema: &Schema) -> String {
    let methods: Map<String, Json> =
        schema.methods.iter().map(|method| (method.name.clone(), json!(method.id))).collect();
    let mut vectors = Vec::new();
    for message in &schema.messages {
        vectors.push(vector(&format!("{} with every field", message.name), message, every_field(schema, message)));
        vectors.push(vector(&format!("{} with no field", message.name), message, Sample::Fields(Vec::new())));
    }
    let file = json!({
        "about": "Every message of protocol 2, written by crates/protocol from protocol/*.wire: its bytes, and its \
                  fields by their names in the schema, each typed as bool, uint, int, float (its bits in hex), str, \
                  bin (hex), list, names or fields, a nested message. A field a vector leaves out is absent from its \
                  bytes, and reads back at its zero value. `just protocol` writes it again.",
        "protocol": 2,
        "methods": methods,
        "messages": vectors,
    });
    let mut text = serde_json::to_string_pretty(&file).expect("a vector is JSON");
    text.push('\n');
    text
}

fn vector(name: &str, message: &Message, sample: Sample) -> Json {
    let mut bytes = Vec::new();
    encode(&mut bytes, &sample);
    json!({ "name": name, "message": message.name, "hex": hex(&bytes), "fields": fields_json(&sample) })
}

fn every_field(schema: &Schema, message: &Message) -> Sample {
    Sample::Fields(
        message
            .fields
            .iter()
            .map(|field| (field.number, field.name.clone(), sample(schema, field, &field.kind)))
            .collect(),
    )
}

/// A field's value in a vector: one of its type, the same each time, so that a
/// vector changes only when the schema does.
fn sample(schema: &Schema, field: &Field, kind: &Kind) -> Sample {
    match kind {
        Kind::Bool => Sample::Bool(true),
        Kind::Uint => Sample::Uint(field.number * 100 + 7),
        Kind::Duration => Sample::Uint(1500),
        Kind::Int => Sample::Int(-7),
        Kind::Time => Sample::Int(1_790_000_000_000),
        // past what a JavaScript number holds exactly
        Kind::Int64 => Sample::Int(9_007_199_254_740_993),
        Kind::Nanos => Sample::Int(1_790_000_000_000_123_456),
        Kind::Float => Sample::Float(1.5),
        Kind::Str => Sample::Str(field.name.clone()),
        Kind::Json => Sample::Str("{\"a\":1}".to_owned()),
        Kind::Key => Sample::Str("k".to_owned()),
        Kind::Bin | Kind::Value => Sample::Bin(vec![0x01, 0xff]),
        Kind::Cell => Sample::Str(field.name.clone()),
        Kind::List(item) => Sample::List(vec![sample(schema, field, item)]),
        Kind::Names(item) => Sample::Names(vec![("a".to_owned(), sample(schema, field, item))]),
        Kind::Message(name) => every_field(schema, schema.message(name)),
    }
}

fn fields_json(sample: &Sample) -> Json {
    let Sample::Fields(fields) = sample else {
        unreachable!("a vector's value is a message");
    };
    fields.iter().map(|(_, name, value)| (name.clone(), typed(value))).collect::<Map<_, _>>().into()
}

/// A value in the vectors' typed notation; integers as text, past what a
/// JavaScript number holds.
fn typed(sample: &Sample) -> Json {
    match sample {
        Sample::Bool(flag) => json!({ "bool": flag }),
        Sample::Uint(value) => json!({ "uint": value.to_string() }),
        Sample::Int(value) => json!({ "int": value.to_string() }),
        Sample::Float(value) => json!({ "float": format!("{:016x}", value.to_bits()) }),
        Sample::Str(text) => json!({ "str": text }),
        Sample::Bin(bytes) => json!({ "bin": hex(bytes) }),
        Sample::List(items) => json!({ "list": items.iter().map(typed).collect::<Vec<_>>() }),
        Sample::Names(pairs) => {
            json!({ "names": pairs.iter().map(|(name, value)| (name.clone(), typed(value))).collect::<Map<_, _>>() })
        }
        Sample::Fields(_) => json!({ "fields": fields_json(sample) }),
    }
}

/// The profile's canonical bytes: the shortest form of every integer, length
/// and count, every float in eight bytes, a map's keys ascending.
fn encode(out: &mut Vec<u8>, sample: &Sample) {
    match sample {
        Sample::Bool(flag) => out.push(if *flag { 0xc3 } else { 0xc2 }),
        Sample::Uint(value) => uint(out, *value),
        Sample::Int(value) => match u64::try_from(*value) {
            Ok(value) => uint(out, value),
            Err(_) => int(out, *value),
        },
        Sample::Float(value) => {
            out.push(0xcb);
            out.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        Sample::Str(text) => {
            head(out, text.len(), (0xa0, 31), [0xd9, 0xda, 0xdb]);
            out.extend_from_slice(text.as_bytes());
        }
        Sample::Bin(bytes) => {
            head(out, bytes.len(), (0, 0), [0xc4, 0xc5, 0xc6]);
            out.extend_from_slice(bytes);
        }
        Sample::List(items) => {
            head(out, items.len(), (0x90, 15), [0, 0xdc, 0xdd]);
            items.iter().for_each(|item| encode(out, item));
        }
        Sample::Names(pairs) => {
            let mut sorted: Vec<&(String, Sample)> = pairs.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            head(out, sorted.len(), (0x80, 15), [0, 0xde, 0xdf]);
            for (name, value) in sorted {
                encode(out, &Sample::Str(name.clone()));
                encode(out, value);
            }
        }
        Sample::Fields(fields) => {
            let mut sorted: Vec<&(u64, String, Sample)> = fields.iter().collect();
            sorted.sort_by_key(|(number, _, _)| *number);
            head(out, sorted.len(), (0x80, 15), [0, 0xde, 0xdf]);
            for (number, _, value) in sorted {
                uint(out, *number);
                encode(out, value);
            }
        }
    }
}

fn head(out: &mut Vec<u8>, length: usize, fix: (u8, usize), markers: [u8; 3]) {
    let (fix_marker, fix_most) = fix;
    if fix_marker != 0 && length <= fix_most {
        out.push(fix_marker | length as u8);
    } else if markers[0] != 0 && length <= 0xff {
        out.extend_from_slice(&[markers[0], length as u8]);
    } else if length <= 0xffff {
        out.push(markers[1]);
        out.extend_from_slice(&(length as u16).to_be_bytes());
    } else {
        out.push(markers[2]);
        out.extend_from_slice(&(length as u32).to_be_bytes());
    }
}

fn uint(out: &mut Vec<u8>, value: u64) {
    match value {
        0..=0x7f => out.push(value as u8),
        0x80..=0xff => out.extend_from_slice(&[0xcc, value as u8]),
        0x100..=0xffff => {
            out.push(0xcd);
            out.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xce);
            out.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            out.push(0xcf);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn int(out: &mut Vec<u8>, value: i64) {
    match value {
        -32..=-1 => out.push(value as u8),
        -0x80..=-33 => out.extend_from_slice(&[0xd0, value as u8]),
        -0x8000..=-0x81 => {
            out.push(0xd1);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
        -0x8000_0000..=-0x8001 => {
            out.push(0xd2);
            out.extend_from_slice(&(value as i32).to_be_bytes());
        }
        _ => {
            out.push(0xd3);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vector_is_canonical_messagepack() {
        let mut bytes = Vec::new();
        let sample = Sample::Fields(vec![(2, "b".to_owned(), Sample::Int(-7)), (1, "a".to_owned(), Sample::Uint(300))]);
        encode(&mut bytes, &sample);
        assert_eq!(hex(&bytes), "8201cd012c02f9", "keys ascending, each integer at its shortest");
    }
}
