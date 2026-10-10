//! The Rust side: a struct a message, read whole or refused a field at a time
//! from its body and written a field at a time into its bytes, through
//! `wire::codec`, and a constant a method.

use std::fmt::Write as _;

use super::names::{const_name, rust_field, type_name};
use super::schema::{Field, Kind, Message, Schema};

pub(crate) fn write(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.\n\n");
    out.push_str("use std::collections::BTreeMap;\n\n");
    out.push_str("use super::codec::{self, Map, Message, Row};\n");
    out.push_str(&cell_import(schema));
    out.push_str("use super::msgpack::Reader;\n");
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
    let _ = writeln!(out, "    const NAME: &'static str = \"{}\";\n", message.name);
    let fields = by_number(message);
    write_read(out, &fields);
    write_write(out, &fields);
    write_size(out, &fields);
    write_is_zero(out, &fields);
    out.push_str("}\n");
}

/// A message's fields in the order of their numbers, which a map's
/// canonical order is.
fn by_number(message: &Message) -> Vec<&Field> {
    let mut fields: Vec<&Field> = message.fields.iter().collect();
    fields.sort_by_key(|field| field.number);
    fields
}

fn write_read(out: &mut String, fields: &[&Field]) {
    out.push_str("    fn read(r: &mut Reader<'_>) -> Result<Self, Failure> {\n");
    if fields.is_empty() {
        out.push_str("        match codec::fields(r, Self::NAME)? {\n");
        out.push_str("            0 => Ok(Self {}),\n");
        out.push_str("            _ => Err(codec::unknown(codec::field(r, Self::NAME, &mut 0)?, Self::NAME)),\n");
        out.push_str("        }\n    }\n\n");
        return;
    }
    out.push_str("        let mut message = Self::default();\n");
    out.push_str("        let mut seen = 0;\n");
    out.push_str("        for _ in 0..codec::fields(r, Self::NAME)? {\n");
    out.push_str("            match codec::field(r, Self::NAME, &mut seen)? {\n");
    for field in fields {
        let read = read_with(&field.kind, &format!("\"{}\"", field.name));
        let read = if field.optional { format!("Some({read})") } else { read };
        let _ = writeln!(out, "                {} => message.{} = {read},", field.number, rust_field(&field.name));
    }
    out.push_str("                number => return Err(codec::unknown(number, Self::NAME)),\n");
    out.push_str("            }\n        }\n        Ok(message)\n    }\n\n");
}

fn write_write(out: &mut String, fields: &[&Field]) {
    if fields.is_empty() {
        out.push_str("    fn write(&self, out: &mut Vec<u8>) {\n        Map::open(out).close(out);\n    }\n\n");
        return;
    }
    out.push_str("    fn write(&self, out: &mut Vec<u8>) {\n");
    out.push_str("        let mut map = Map::open(out);\n");
    for field in fields {
        let member = rust_field(&field.name);
        if field.optional {
            let _ = writeln!(out, "        if let Some({member}) = &self.{member} {{");
            let _ = writeln!(out, "            map.field(out, {});", field.number);
            let _ = writeln!(out, "            {};", write_with(&field.kind, &member));
        } else {
            let _ = writeln!(out, "        if {} {{", nonzero(&field.kind, &format!("self.{member}")));
            let _ = writeln!(out, "            map.field(out, {});", field.number);
            let _ = writeln!(out, "            {};", write_with(&field.kind, &format!("&self.{member}")));
        }
        out.push_str("        }\n");
    }
    out.push_str("        map.close(out);\n    }\n\n");
}

/// The most bytes the message writes as: a map's head of three, and each
/// field's number and value at their widest.
fn write_size(out: &mut String, fields: &[&Field]) {
    let mut parts = vec!["3".to_owned()];
    for field in fields {
        let member = rust_field(&field.name);
        let part = match (fixed(&field.kind), field.optional) {
            (Some(width), false) => format!("{}", 1 + width),
            (Some(width), true) => format!("self.{member}.map_or(0, |_| {})", 1 + width),
            (None, false) => format!("1 + {}", size(&field.kind, &format!("self.{member}"), false)),
            (None, true) => {
                let each = size(&field.kind, &member, true);
                format!("self.{member}.as_ref().map_or(0, |{member}| 1 + {each})")
            }
        };
        parts.push(part);
    }
    let _ = writeln!(out, "    fn size(&self) -> usize {{\n        {}\n    }}\n", parts.join("\n            + "));
}

/// The bytes a value of a kind of fixed width writes as at most: a bool one,
/// a number nine.
fn fixed(kind: &Kind) -> Option<usize> {
    match kind {
        Kind::Bool => Some(1),
        Kind::Uint | Kind::Duration | Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos | Kind::Float => Some(9),
        _ => None,
    }
}

/// The most bytes a value of a kind writes as, from an expression that is
/// the value's place, or a reference to it when `borrowed`.
fn size(kind: &Kind, value: &str, borrowed: bool) -> String {
    let reference = if borrowed { value.to_owned() } else { format!("&{value}") };
    match kind {
        Kind::Bool => "1".to_owned(),
        Kind::Uint | Kind::Duration | Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos | Kind::Float => {
            "9".to_owned()
        }
        Kind::Str | Kind::Json | Kind::Key | Kind::Bin => format!("5 + {value}.len()"),
        Kind::Value => format!("codec::row_size({reference})"),
        Kind::Cell => format!("codec::cell_size({reference})"),
        Kind::List(item) => format!("codec::list_size({reference}, {})", item_size(item)),
        Kind::Names(item) => format!("codec::names_size({reference}, {})", item_size(item)),
        Kind::Message(_) => format!("{value}.size()"),
    }
}

/// What sizes an item of a list or a map: a function, or a closure.
fn item_size(kind: &Kind) -> String {
    match kind {
        Kind::Value => "codec::row_size".to_owned(),
        Kind::Cell => "codec::cell_size".to_owned(),
        Kind::Message(_) => "codec::message_size".to_owned(),
        _ => format!("|item| {}", size(kind, "item", true)),
    }
}

fn write_is_zero(out: &mut String, fields: &[&Field]) {
    let zero: Vec<String> = fields
        .iter()
        .map(|field| {
            let member = format!("self.{}", rust_field(&field.name));
            if field.optional { format!("{member}.is_none()") } else { zero(&field.kind, &member) }
        })
        .collect();
    let zero = if zero.is_empty() { "true".to_owned() } else { zero.join("\n            && ") };
    let _ = writeln!(out, "    fn is_zero(&self) -> bool {{\n        {zero}\n    }}");
}

/// `method::reads`: the methods the schema marks `read`, an arm each behind
/// its engine's feature, which `matches!` cannot gate.
fn write_reads(out: &mut String, schema: &Schema) {
    out.push_str(
        "
    /// Whether a method only reads, so that a connection admitted to read only
    /// may call it; a method the schema does not mark `read` writes.
    #[allow(clippy::match_like_matches_macro, reason = \"an arm a method, behind its engine's feature\")]
    pub(crate) fn reads(method: u16) -> bool {
        match method {
",
    );
    for method in schema.methods.iter().filter(|method| method.reads) {
        out.push_str(&gated(&method.name, "            "));
        let _ = writeln!(out, "            {} => true,", const_name(&method.name));
    }
    out.push_str(
        "            _ => false,
        }
    }
",
    );
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
    write_reads(out, schema);
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
/// vector's bytes are what this codec writes, in no more than its size.
fn write_rewrite(out: &mut String, schema: &Schema) {
    out.push_str(
        "/// A body of the message `name` read and written again, and the size the message
",
    );
    out.push_str(
        "/// says it takes, for the test that the vectors' bytes are what this codec writes
",
    );
    out.push_str(
        "/// within that size; `None` for a name it lacks.
",
    );
    out.push_str(
        "#[cfg(test)]
",
    );
    out.push_str(
        "pub(crate) fn rewrite(name: &str, body: &[u8]) -> Option<Result<(Vec<u8>, usize), Failure>> {
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
            "        \"{}\" => {}::decode(body).map(|message| (message.encode(), message.size())),",
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
        Kind::Cell => "Cell".to_owned(),
        Kind::List(item) => format!("Vec<{}>", kind_type(item)),
        Kind::Names(item) => format!("BTreeMap<String, {}>", kind_type(item)),
        Kind::Message(name) => type_name(name),
    }
}

/// A field's value read from the reader `r`, the field named `name` in a
/// refusal.
fn read_with(kind: &Kind, name: &str) -> String {
    match kind {
        Kind::List(item) => format!("codec::list(r, {name}, {})?", item_reader(item)),
        Kind::Names(item) => format!("codec::names(r, {name}, {})?", item_reader(item)),
        Kind::Message(message) => format!("{}::read(r)?", type_name(message)),
        _ => format!("{}(r, {name})?", item_reader(kind)),
    }
}

/// What reads an item of a list or a map: a function, or a closure for a
/// list in a list.
fn item_reader(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "codec::bool".to_owned(),
        Kind::Uint | Kind::Duration => "codec::uint".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "codec::int".to_owned(),
        Kind::Float => "codec::float".to_owned(),
        Kind::Str | Kind::Json => "codec::str".to_owned(),
        Kind::Key => "codec::key".to_owned(),
        Kind::Bin => "codec::bin".to_owned(),
        Kind::Value => "codec::row".to_owned(),
        Kind::Cell => "codec::cell".to_owned(),
        Kind::List(item) => format!("|r, name| codec::list(r, name, {})", item_reader(item)),
        Kind::Names(item) => format!("|r, name| codec::names(r, name, {})", item_reader(item)),
        Kind::Message(name) => format!("codec::message::<{}>", type_name(name)),
    }
}

/// A field's value written, from an expression that borrows it.
fn write_with(kind: &Kind, value: &str) -> String {
    match kind {
        Kind::List(item) => format!("codec::write_list(out, {value}, {})", item_writer(item)),
        Kind::Names(item) => format!("codec::write_names(out, {value}, {})", item_writer(item)),
        _ => format!("{}(out, {value})", item_writer(kind)),
    }
}

/// What writes an item of a list or a map: a function, or a closure for a
/// list in a list.
fn item_writer(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "codec::write_bool".to_owned(),
        Kind::Uint | Kind::Duration => "codec::write_uint".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "codec::write_int".to_owned(),
        Kind::Float => "codec::write_float".to_owned(),
        Kind::Str | Kind::Json | Kind::Key => "codec::write_str".to_owned(),
        Kind::Bin => "codec::write_bin".to_owned(),
        Kind::Value => "codec::write_row".to_owned(),
        Kind::Cell => "codec::write_cell".to_owned(),
        Kind::List(item) => format!("|out, items| codec::write_list(out, items, {})", item_writer(item)),
        Kind::Names(item) => format!("|out, names| codec::write_names(out, names, {})", item_writer(item)),
        Kind::Message(_) => "codec::write_message".to_owned(),
    }
}

/// Whether a field that is not optional holds more than its zero value,
/// which the profile writes and a field at zero leaves out: false, 0, a float
/// whose bits are all 0, nothing in a text, bytes or a list, a message all
/// of whose fields are at zero.
fn nonzero(kind: &Kind, member: &str) -> String {
    match kind {
        Kind::Bool => member.to_owned(),
        Kind::Uint | Kind::Duration | Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => format!("{member} != 0"),
        Kind::Float => format!("{member}.to_bits() != 0"),
        Kind::Str | Kind::Json | Kind::Key | Kind::Bin | Kind::List(_) | Kind::Names(_) => {
            format!("!{member}.is_empty()")
        }
        Kind::Value => format!("!codec::row_is_zero(&{member})"),
        Kind::Cell => format!("!codec::cell_is_zero(&{member})"),
        Kind::Message(_) => format!("!{member}.is_zero()"),
    }
}

/// Whether a field that is not optional holds its zero value: what
/// `nonzero` says the other way round.
fn zero(kind: &Kind, member: &str) -> String {
    match kind {
        Kind::Bool => format!("!{member}"),
        Kind::Uint | Kind::Duration | Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => format!("{member} == 0"),
        Kind::Float => format!("{member}.to_bits() == 0"),
        Kind::Str | Kind::Json | Kind::Key | Kind::Bin | Kind::List(_) | Kind::Names(_) => {
            format!("{member}.is_empty()")
        }
        Kind::Value => format!("codec::row_is_zero(&{member})"),
        Kind::Cell => format!("codec::cell_is_zero(&{member})"),
        Kind::Message(_) => format!("{member}.is_zero()"),
    }
}

/// The engines, each a feature of the library; the connection's messages and
/// the server's own are built always.
const ENGINES: [&str; 6] = ["kv", "jobs", "sql", "blobs", "records", "metrics"];

/// The import of `Cell`, gated by the engines whose messages hold one, so
/// that a build without them imports nothing it does not use.
fn cell_import(schema: &Schema) -> String {
    let mut engines: Vec<&str> = Vec::new();
    for message in &schema.messages {
        if !message.fields.iter().any(|field| holds_cell(&field.kind)) {
            continue;
        }
        match message.name.split_once('.') {
            Some((engine, _)) if ENGINES.contains(&engine) => {
                if !engines.contains(&engine) {
                    engines.push(engine);
                }
            }
            _ => return "use super::codec::Cell;\n".to_owned(),
        }
    }
    let features: Vec<String> = engines.iter().map(|engine| format!("feature = \"{engine}\"")).collect();
    match features.as_slice() {
        [] => String::new(),
        [feature] => format!("#[cfg({feature})]\nuse super::codec::Cell;\n"),
        several => format!("#[cfg(any({}))]\nuse super::codec::Cell;\n", several.join(", ")),
    }
}

fn holds_cell(kind: &Kind) -> bool {
    match kind {
        Kind::Cell => true,
        Kind::List(item) | Kind::Names(item) => holds_cell(item),
        _ => false,
    }
}

/// An engine's messages and methods are its feature's: `kv.` names kv's.
fn feature_gate(name: &str) -> String {
    gated(name, "")
}

fn gated(name: &str, indent: &str) -> String {
    match name.split_once('.') {
        Some((engine, _)) if ENGINES.contains(&engine) => format!("{indent}#[cfg(feature = \"{engine}\")]\n"),
        _ => String::new(),
    }
}
