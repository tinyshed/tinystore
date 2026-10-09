//! The TypeScript side: a declaration a message in the SDK's codec language,
//! sdk/js/src/wire/codec.ts, and the methods' numbers.

use std::fmt::Write as _;

use super::names::type_name;
use super::schema::{Kind, Message, Schema};

/// What sdk/js/src/wire/codec.ts reads and writes a field with, in the order
/// biome sorts them.
const CODECS: [&str; 12] =
    ["bin", "bool", "float", "int", "int64", "key", "kvValue", "list", "message", "names", "str", "uint"];

pub(crate) fn write(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.\n\n");
    // every codec, used or not, so that a schema's change never moves the header
    out.push_str("import {\n");
    for codec in CODECS {
        let _ = writeln!(out, "\t{codec},");
    }
    out.push_str("} from './codec.ts'\n\n");
    out.push_str("export const methods = {\n");
    for method in &schema.methods {
        let _ = writeln!(out, "\t'{}': {:#06x},", method.name, method.id);
    }
    out.push_str("} as const\n\nexport type Method = keyof typeof methods\n");
    for message in in_order(schema) {
        out.push('\n');
        write_message(&mut out, message);
    }
    out
}

fn write_message(out: &mut String, message: &Message) {
    if !message.doc.is_empty() {
        out.push_str("/**\n");
        for line in &message.doc {
            let _ = writeln!(out, " * {line}");
        }
        out.push_str(" */\n");
    }
    let name = type_name(&message.name);
    if message.fields.is_empty() {
        let _ = writeln!(out, "export const {name} = message('{}', {{}})", message.name);
        return;
    }
    let _ = writeln!(out, "export const {name} = message('{}', {{", message.name);
    for field in &message.fields {
        if let Some(doc) = &field.doc {
            let _ = writeln!(out, "\t/** {doc} */");
        }
        let _ = writeln!(out, "\t{}: [{}, {}],", field.name, field.number, codec(&field.kind));
    }
    out.push_str("})\n");
}

fn codec(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "bool".to_owned(),
        Kind::Uint | Kind::Duration => "uint".to_owned(),
        Kind::Int | Kind::Time => "int".to_owned(),
        Kind::Int64 | Kind::Nanos => "int64".to_owned(),
        Kind::Float => "float".to_owned(),
        Kind::Str | Kind::Json => "str".to_owned(),
        Kind::Bin => "bin".to_owned(),
        Kind::Key => "key".to_owned(),
        Kind::Value => "kvValue".to_owned(),
        Kind::List(item) => format!("list({})", codec(item)),
        Kind::Names(item) => format!("names({})", codec(item)),
        Kind::Message(name) => type_name(name),
    }
}

/// The messages with each declared after the ones it names, as a constant
/// must be.
fn in_order(schema: &Schema) -> Vec<&Message> {
    let mut written: Vec<&Message> = Vec::new();
    while written.len() < schema.messages.len() {
        let before = written.len();
        for message in &schema.messages {
            let done = |name: &str| written.iter().any(|written| written.name == name);
            let ready = message.fields.iter().filter_map(|field| field.kind.message()).all(done);
            if ready && !done(&message.name) {
                written.push(message);
            }
        }
        assert!(written.len() > before, "the schema's messages name each other in a cycle");
    }
    written
}
