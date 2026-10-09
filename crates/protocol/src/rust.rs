//! The Rust side: a struct a message, read whole or refused through
//! `wire::codec`, and a constant a method.

use std::fmt::Write as _;

use super::names::{const_name, rust_field, type_name};
use super::schema::{Field, Kind, Message, Schema};

pub(crate) fn write(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.\n\n");
    out.push_str("use std::collections::BTreeMap;\n\n");
    out.push_str("use super::codec::{self, Message, Out, Row};\n");
    out.push_str("use super::message::Fields;\n");
    for message in &schema.messages {
        out.push('\n');
        write_message(&mut out, message);
    }
    out.push('\n');
    write_methods(&mut out, schema);
    out.push('\n');
    write_rewrite(&mut out, schema);
    out
}

fn write_message(out: &mut String, message: &Message) {
    let name = type_name(&message.name);
    let gate = feature_gate(&message.name);
    for line in &message.doc {
        let _ = writeln!(out, "/// {line}");
    }
    out.push_str(&gate);
    out.push_str("#[derive(Clone, Debug, Default, PartialEq)]\n");
    let _ = writeln!(out, "pub(crate) struct {name} {{");
    for field in &message.fields {
        if let Some(doc) = field_doc(field) {
            let _ = writeln!(out, "    /// {doc}");
        }
        let _ = writeln!(out, "    pub(crate) {}: {},", rust_field(&field.name), field_type(field));
    }
    out.push_str("}\n\n");
    out.push_str(&gate);
    let _ = writeln!(out, "impl Message for {name} {{");
    let _ = writeln!(out, "    const NAME: &'static str = \"{}\";", message.name);
    let keys: Vec<String> = message.fields.iter().map(|field| field.number.to_string()).collect();
    let _ = writeln!(out, "    const KEYS: &'static [u64] = &[{}];\n", keys.join(", "));
    write_read(out, &name, message);
    write_write(out, message);
    out.push_str("}\n");
}

fn write_read(out: &mut String, name: &str, message: &Message) {
    let fields = if message.fields.is_empty() { "_fields" } else { "fields" };
    let _ = writeln!(out, "    fn read({fields}: &Fields) -> Result<Self, Failure> {{");
    if message.fields.is_empty() {
        let _ = writeln!(out, "        Ok({name} {{}})");
    } else {
        let _ = writeln!(out, "        Ok({name} {{");
        for field in &message.fields {
            let read = format!("fields.get({}, \"{}\", {})?", field.number, field.name, reader(&field.kind));
            let read = if field.optional { read } else { format!("{read}.unwrap_or_default()") };
            let _ = writeln!(out, "            {}: {read},", rust_field(&field.name));
        }
        out.push_str("        })\n");
    }
    out.push_str("    }\n\n");
}

fn write_write(out: &mut String, message: &Message) {
    let to = if message.fields.is_empty() { "_out" } else { "out" };
    let _ = writeln!(out, "    fn write(&self, {to}: &mut Out) {{");
    for field in &message.fields {
        let member = format!("self.{}", rust_field(&field.name));
        let line = if field.optional {
            format!("out.given({}, {member}.as_ref().map({}));", field.number, writer(&field.kind))
        } else {
            format!("out.put({}, {});", field.number, written(&field.kind, &format!("&{member}")))
        };
        let _ = writeln!(out, "        {line}");
    }
    out.push_str("    }\n");
}

fn write_methods(out: &mut String, schema: &Schema) {
    out.push_str(
        "/// Every method's number, by its name in the schema.
",
    );
    out.push_str(
        "pub(crate) mod method {
",
    );
    for method in &schema.methods {
        out.push_str(&gated(&method.name, "    "));
        let _ = writeln!(out, "    pub(crate) const {}: u16 = {:#06x};", const_name(&method.name), method.id);
    }
    out.push_str(
        "}

",
    );
    out.push_str(
        "/// Every method, its name and number, for the test that the server answers each.
",
    );
    out.push_str(
        "#[cfg(test)]
",
    );
    out.push_str(
        "pub(crate) const METHODS: &[(&str, u16)] = &[
",
    );
    for method in &schema.methods {
        out.push_str(&gated(&method.name, "    "));
        let _ = writeln!(out, "    (\"{}\", {:#06x}),", method.name, method.id);
    }
    out.push_str(
        "];
",
    );
}

/// Reads a body by its message's name and writes it again: the test that every
/// vector's bytes are what this codec writes.
fn write_rewrite(out: &mut String, schema: &Schema) {
    out.push_str(
        "/// A body of the message `name` read and written again, for the test that the
",
    );
    out.push_str(
        "/// vectors' bytes are what this codec writes; `None` for a name it lacks.
",
    );
    out.push_str(
        "#[cfg(test)]
",
    );
    out.push_str(
        "pub(crate) fn rewrite(name: &str, body: &[u8]) -> Option<Result<Vec<u8>, Failure>> {
",
    );
    out.push_str(
        "    let rewritten = match name {
",
    );
    for message in &schema.messages {
        out.push_str(&gated(&message.name, "        "));
        let _ = writeln!(
            out,
            "        \"{}\" => {}::decode(body).map(|message| message.encode()),",
            message.name,
            type_name(&message.name)
        );
    }
    out.push_str(
        "        _ => return None,
",
    );
    out.push_str(
        "    };
",
    );
    out.push_str(
        "    Some(rewritten)
",
    );
    out.push_str(
        "}
",
    );
}

/// A field's doc, with the unit a number of time is in.
fn field_doc(field: &Field) -> Option<String> {
    let unit = match field.kind {
        Kind::Duration => Some("Milliseconds."),
        Kind::Time => Some("Unix milliseconds."),
        Kind::Nanos => Some("Unix nanoseconds."),
        _ => None,
    };
    match (&field.doc, unit) {
        (Some(doc), Some(unit)) => Some(format!("{}. {unit}", capitalized(doc))),
        (Some(doc), None) => Some(format!("{}.", capitalized(doc))),
        (None, unit) => unit.map(str::to_owned),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn field_type(field: &Field) -> String {
    let kind = kind_type(&field.kind);
    if field.optional { format!("Option<{kind}>") } else { kind }
}

fn kind_type(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "bool".to_owned(),
        Kind::Uint | Kind::Duration => "u64".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "i64".to_owned(),
        Kind::Float => "f64".to_owned(),
        Kind::Str | Kind::Key | Kind::Json => "String".to_owned(),
        Kind::Bin => "Vec<u8>".to_owned(),
        Kind::Value => "Row".to_owned(),
        Kind::List(item) => format!("Vec<{}>", kind_type(item)),
        Kind::Names(item) => format!("BTreeMap<String, {}>", kind_type(item)),
        Kind::Message(name) => type_name(name),
    }
}

/// The function that reads a kind from its value.
fn reader(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "codec::bool".to_owned(),
        Kind::Uint | Kind::Duration => "codec::uint".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "codec::int".to_owned(),
        Kind::Float => "codec::float".to_owned(),
        Kind::Str | Kind::Json => "codec::str".to_owned(),
        Kind::Key => "codec::key".to_owned(),
        Kind::Bin => "codec::bin".to_owned(),
        Kind::Value => "codec::row".to_owned(),
        Kind::List(item) => format!("codec::list({})", reader(item)),
        Kind::Names(item) => format!("codec::names({})", reader(item)),
        Kind::Message(name) => format!("codec::message::<{}>", type_name(name)),
    }
}

/// A kind's value written, from an expression that borrows it.
fn written(kind: &Kind, value: &str) -> String {
    match kind {
        Kind::List(item) => format!("codec::list_value({value}, {})", writer(item)),
        Kind::Names(item) => format!("codec::names_value({value}, {})", writer(item)),
        _ => format!("{}({value})", writer(kind)),
    }
}

/// What writes a kind as its value: a function, or a closure for a list.
fn writer(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "codec::bool_value".to_owned(),
        Kind::Uint | Kind::Duration => "codec::uint_value".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "codec::int_value".to_owned(),
        Kind::Float => "codec::float_value".to_owned(),
        Kind::Str | Kind::Json | Kind::Key => "codec::str_value".to_owned(),
        Kind::Bin => "codec::bin_value".to_owned(),
        Kind::Value => "codec::row_value".to_owned(),
        Kind::List(item) => format!("|items| codec::list_value(items, {})", writer(item)),
        Kind::Names(item) => format!("|names| codec::names_value(names, {})", writer(item)),
        Kind::Message(_) => "codec::message_value".to_owned(),
    }
}

/// An engine's messages and methods are its feature's: `kv.` names kv's.
fn feature_gate(name: &str) -> String {
    gated(name, "")
}

fn gated(name: &str, indent: &str) -> String {
    match name.split_once('.') {
        Some((engine, _)) => format!(
            "{indent}#[cfg(feature = \"{engine}\")]
"
        ),
        None => String::new(),
    }
}
