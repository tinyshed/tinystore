//! The TypeScript side: a message's fields in the SDK's codec language,
//! sdk/js/src/wire/codec.ts, which its types and the tests read, and its
//! reader and writer written out a field at a time; and the methods' numbers.

use std::fmt::Write as _;

use super::names::type_name;
use super::schema::{Field, Kind, Message, Schema};

/// What protocol.ts takes from sdk/js/src/wire/codec.ts, in the order biome
/// sorts them: every codec and helper, used or not, so that a schema's
/// change never moves the header.
const IMPORTS: [&str; 18] = [
    "bin",
    "bool",
    "float",
    "int",
    "int64",
    "key",
    "kvValue",
    "list",
    "message",
    "names",
    "type Raw",
    "type Read",
    "readNames",
    "type SqlValue",
    "sqlValue",
    "str",
    "uint",
    "writeNames",
];

pub(crate) fn write(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.\n\n");
    out.push_str("import {\n");
    for import in IMPORTS {
        let _ = writeln!(out, "\t{import},");
    }
    out.push_str("} from './codec.ts'\n\n");
    out.push_str("export const methods = {\n");
    for method in &schema.methods {
        write_doc(&mut out, &method.doc, "\t");
        let _ = writeln!(out, "\t'{}': {:#06x},", method.name, method.id);
    }
    out.push_str("} as const\n\nexport type Method = keyof typeof methods\n");
    for message in in_order(schema) {
        out.push('\n');
        write_message(&mut out, message);
    }
    out
}

/// A message's or a method's doc as a JSDoc block, nothing for none.
fn write_doc(out: &mut String, doc: &[String], indent: &str) {
    if doc.is_empty() {
        return;
    }
    let _ = writeln!(out, "{indent}/**");
    for line in doc {
        let _ = writeln!(out, "{indent} * {line}");
    }
    let _ = writeln!(out, "{indent} */");
}

fn write_message(out: &mut String, message: &Message) {
    write_doc(out, &message.doc, "");
    let name = type_name(&message.name);
    let _ = writeln!(out, "export const {name} = message(");
    let _ = writeln!(out, "\t'{}',", message.name);
    if message.fields.is_empty() {
        out.push_str("\t{},\n");
    } else {
        out.push_str("\t{\n");
        for field in &message.fields {
            if let Some(doc) = &field.doc {
                let _ = writeln!(out, "\t\t/** {doc} */");
            }
            let _ = writeln!(out, "\t\t{}: [{}, {}],", field.name, field.number, codec(&field.kind));
        }
        out.push_str("\t},\n");
    }
    out.push_str("\t{\n");
    let mut fields: Vec<&Field> = message.fields.iter().collect();
    fields.sort_by_key(|field| field.number);
    write_write(out, &fields);
    write_read(out, message, &fields);
    out.push_str("\t},\n)\n");
}

/// The writer: a field that is not undefined written after its number, the
/// map's count kept in a byte until the fields are written.
fn write_write(out: &mut String, fields: &[&Field]) {
    if fields.is_empty() {
        out.push_str("\t\twrite(w) {\n\t\t\tw.closeMap(w.openMap(), 0)\n\t\t},\n");
        return;
    }
    out.push_str("\t\twrite(w, v) {\n");
    out.push_str("\t\t\tconst head = w.openMap()\n");
    out.push_str("\t\t\tlet n = 0\n");
    for field in fields {
        let value = format!("v.{}", field.name);
        let _ = writeln!(out, "\t\t\tif ({value} !== undefined) {{");
        let _ = writeln!(out, "\t\t\t\tw.field({})", field.number);
        write_value(out, &field.kind, &value, 4, 0);
        out.push_str("\t\t\t\tn++\n\t\t\t}\n");
    }
    out.push_str("\t\t\tw.closeMap(head, n)\n\t\t},\n");
}

/// The lines that write `value` of a kind, `indent` tabs in; a list's items
/// are named by how deep the list is.
fn write_value(out: &mut String, kind: &Kind, value: &str, indent: usize, depth: usize) {
    let tabs = "\t".repeat(indent);
    match kind {
        Kind::List(item) => {
            let each = format!("item{depth}");
            let _ = writeln!(out, "{tabs}w.array({value}.length)");
            let _ = writeln!(out, "{tabs}for (const {each} of {value}) {{");
            write_value(out, item, &each, indent + 1, depth + 1);
            let _ = writeln!(out, "{tabs}}}");
        }
        Kind::Names(item) => {
            let _ = writeln!(out, "{tabs}writeNames(w, {value}, {}.write)", codec(item));
        }
        _ => {
            let _ = writeln!(out, "{tabs}{}", written(kind, value));
        }
    }
}

/// A value of a kind that is no list written, as one call.
fn written(kind: &Kind, value: &str) -> String {
    match kind {
        Kind::Bool => format!("w.bool({value})"),
        Kind::Uint | Kind::Duration => format!("w.uint({value})"),
        Kind::Int | Kind::Time | Kind::Int64 | Kind::Nanos => format!("w.int({value})"),
        Kind::Float => format!("w.float({value})"),
        Kind::Str | Kind::Json => format!("w.str({value})"),
        Kind::Bin => format!("w.bin({value})"),
        _ => format!("{}.write(w, {value})", codec(kind)),
    }
}

/// The reader: each field into a variable of its own, a key the message lacks
/// skipped, and one object of every field at the end, so that every message
/// read has the same shape.
fn write_read(out: &mut String, message: &Message, fields: &[&Field]) {
    out.push_str("\t\tread(r) {\n");
    for field in &message.fields {
        let _ = writeln!(out, "\t\t\tlet ${}: {} | undefined", field.name, ts_type(&field.kind));
    }
    out.push_str("\t\t\tfor (let n = r.message(); n > 0; n--) {\n");
    if fields.is_empty() {
        out.push_str("\t\t\t\tr.field()\n\t\t\t\tr.skip()\n");
    } else {
        out.push_str("\t\t\t\tswitch (r.field()) {\n");
        for field in fields {
            let target = format!("${}", field.name);
            match read(&field.kind) {
                Some(read) => {
                    let _ = writeln!(out, "\t\t\t\t\tcase {}:", field.number);
                    let _ = writeln!(out, "\t\t\t\t\t\t{target} = {read}");
                    out.push_str("\t\t\t\t\t\tbreak\n");
                }
                None => {
                    let _ = writeln!(out, "\t\t\t\t\tcase {}: {{", field.number);
                    read_list(out, &field.kind, 6, 0);
                    let _ = writeln!(out, "\t\t\t\t\t\t{target} = items0");
                    out.push_str("\t\t\t\t\t\tbreak\n\t\t\t\t\t}\n");
                }
            }
        }
        out.push_str("\t\t\t\t\tdefault:\n\t\t\t\t\t\tr.skip()\n\t\t\t\t}\n");
    }
    out.push_str("\t\t\t}\n\t\t\tr.leave()\n");
    if message.fields.is_empty() {
        out.push_str("\t\t\treturn {}\n");
    } else {
        let all: Vec<String> = message.fields.iter().map(|field| format!("{0}: ${0}", field.name)).collect();
        let one = format!("return {{ {} }}", all.join(", "));
        // three tabs of two columns each, as biome counts them, and its line width of 100
        if 6 + one.len() <= 100 {
            let _ = writeln!(out, "\t\t\t{one}");
        } else {
            out.push_str("\t\t\treturn {\n");
            for each in &all {
                let _ = writeln!(out, "\t\t\t\t{each},");
            }
            out.push_str("\t\t\t}\n");
        }
    }
    out.push_str("\t\t},\n");
}

/// The lines that read a list into a variable named by how deep it is,
/// `items0` for a field's, `indent` tabs in; a list in a list is read the
/// same way, one level down, and pushed whole.
fn read_list(out: &mut String, kind: &Kind, indent: usize, depth: usize) {
    let Kind::List(item) = kind else {
        unreachable!("read_list reads lists");
    };
    let tabs = "\t".repeat(indent);
    let (items, count) = (format!("items{depth}"), format!("i{depth}"));
    let _ = writeln!(out, "{tabs}const {items}: {} = []", ts_type(kind));
    let _ = writeln!(out, "{tabs}for (let {count} = r.array(); {count} > 0; {count}--) {{");
    match read(item) {
        Some(read) => {
            let _ = writeln!(out, "{tabs}\t{items}.push({read})");
        }
        None => {
            read_list(out, item, indent + 1, depth + 1);
            let _ = writeln!(out, "{tabs}\t{items}.push(items{})", depth + 1);
        }
    }
    let _ = writeln!(out, "{tabs}}}");
    let _ = writeln!(out, "{tabs}r.leave()");
}

/// A value of a kind read, as one call; `None` for a list, which takes lines.
fn read(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Bool => "r.bool()".to_owned(),
        Kind::Uint | Kind::Duration => "r.uint()".to_owned(),
        Kind::Int | Kind::Time => "r.int()".to_owned(),
        Kind::Int64 | Kind::Nanos => "r.int64()".to_owned(),
        Kind::Float => "r.float()".to_owned(),
        Kind::Str | Kind::Json => "r.str()".to_owned(),
        Kind::Bin => "r.bytes()".to_owned(),
        Kind::Names(item) => format!("readNames(r, {}.read)", codec(item)),
        Kind::List(_) => return None,
        _ => format!("{}.read(r)", codec(kind)),
    })
}

/// What a kind reads as in TypeScript, as codec.ts's codecs read it.
fn ts_type(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "boolean".to_owned(),
        Kind::Uint | Kind::Duration | Kind::Int | Kind::Time | Kind::Float => "number".to_owned(),
        Kind::Int64 | Kind::Nanos => "bigint".to_owned(),
        Kind::Str | Kind::Json => "string".to_owned(),
        Kind::Bin => "Uint8Array".to_owned(),
        Kind::Key => "string | Uint8Array".to_owned(),
        Kind::Value => "Raw".to_owned(),
        Kind::Cell => "SqlValue | boolean".to_owned(),
        Kind::List(item) => {
            let item = ts_type(item);
            if bare_union(&item) { format!("({item})[]") } else { format!("{item}[]") }
        }
        Kind::Names(item) => format!("Record<string, {}>", ts_type(item)),
        Kind::Message(name) => format!("Read<typeof {}.fields>", type_name(name)),
    }
}

/// Whether a type is a union outside any parentheses, which an array of it
/// needs around it: `string | Uint8Array`, not `(string | Uint8Array)[]`.
fn bare_union(text: &str) -> bool {
    let mut depth = 0;
    for char in text.chars() {
        match char {
            '(' | '<' => depth += 1,
            ')' | '>' => depth -= 1,
            '|' if depth == 0 => return true,
            _ => {}
        }
    }
    false
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
        Kind::Cell => "sqlValue".to_owned(),
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
