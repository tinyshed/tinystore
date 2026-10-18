//! The Python side: a message's fields in the SDK's codec language,
//! sdk/python/src/tinystore/_wire/codec.py, which its readers and the tests
//! read, its reader and writer written out a field at a time; the methods'
//! numbers; and every message by its name in the schema.
//!
//! ```text
//! kv.Branch { 1 handle: uint, 2 under: [key] }
//!
//! def _write_kv_branch(w, v):                 def _read_kv_branch(r):
//!     head = w.open_map()                         read = {}
//!     n = 0                                       seen = set()
//!     if (x := v.get("handle")) is not None:      for _ in range(r.map()):
//!         w.uint(1)                                   match r.field(seen):
//!         w.uint(x)                                       case 1:
//!         n += 1                                              read["handle"] = r.uint()
//!     …                                               …
//!     w.close_map(head, n)                        r.leave()
//! ```

use std::fmt::Write as _;

use super::names::{python_field, type_name};
use super::schema::{Field, Kind, Message, Schema};

/// The widest line the SDK's lint takes.
const WIDTH: usize = 120;

pub(crate) fn write(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("\"\"\"Every message and method of the protocol.\n\n");
    out.push_str("Written by crates/protocol from protocol/*.wire; `just protocol` writes it again.\n\"\"\"\n\n");
    // a list of lists is read by a loop, which the lint would have a comprehension
    out.push_str("# ruff: noqa: PERF401, RUF100\n");
    out.push_str("from __future__ import annotations\n\n");
    out.push_str("from typing import TYPE_CHECKING, Any\n\n");
    out.push_str(
        "from .codec import Message, bin_, bool_, float_, int_, key, kv_value, list_, names, sql_value, str_, uint\n\n",
    );
    out.push_str(
        "if TYPE_CHECKING:\n    from collections.abc import Mapping\n\n    from .msgpack import Reader, Writer\n\n",
    );
    out.push_str("methods: dict[str, int] = {\n");
    for method in &schema.methods {
        write_comment(&mut out, &method.doc, "    ");
        let _ = writeln!(out, "    \"{}\": 0x{:04X},", method.name, method.id);
    }
    out.push_str("}\n");
    let ordered = in_order(schema);
    for message in &ordered {
        out.push_str("\n\n");
        write_message(&mut out, message);
    }
    out.push_str("\n\nmessages: dict[str, Message] = {\n");
    for message in &ordered {
        let _ = writeln!(out, "    \"{}\": {},", message.name, type_name(&message.name));
    }
    out.push_str("}\n\"\"\"Every message by its name in the schema.\"\"\"\n");
    out
}

fn write_message(out: &mut String, message: &Message) {
    let name = type_name(&message.name);
    let function = python_field(&name);
    let mut fields: Vec<&Field> = message.fields.iter().collect();
    fields.sort_by_key(|field| field.number);
    write_writer(out, &function, &fields);
    out.push_str("\n\n");
    write_reader(out, &function, &fields);
    out.push_str("\n\n");
    write_comment(out, &message.doc, "");
    let _ = writeln!(out, "{name} = Message(");
    let _ = writeln!(out, "    \"{}\",", message.name);
    if message.fields.is_empty() {
        out.push_str("    {},\n");
    } else {
        out.push_str("    {\n");
        for field in &message.fields {
            if let Some(doc) = &field.doc {
                write_comment(out, std::slice::from_ref(doc), "        ");
            }
            let _ =
                writeln!(out, "        \"{}\": ({}, {}),", python_field(&field.name), field.number, codec(&field.kind));
        }
        out.push_str("    },\n");
    }
    let _ = writeln!(out, "    _write_{function},\n    _read_{function},\n)");
}

/// The writer: a field that is not None written after its number, the map's
/// count kept in a byte until the fields are written.
fn write_writer(out: &mut String, function: &str, fields: &[&Field]) {
    if fields.is_empty() {
        let _ = writeln!(out, "def _write_{function}(w: Writer, v: Mapping[str, Any]) -> None:");
        out.push_str("    w.map(0)\n");
        return;
    }
    let _ = writeln!(out, "def _write_{function}(w: Writer, v: Mapping[str, Any]) -> None:");
    out.push_str("    head = w.open_map()\n    n = 0\n");
    for field in fields {
        let _ = writeln!(out, "    if (x := v.get(\"{}\")) is not None:", python_field(&field.name));
        let _ = writeln!(out, "        w.uint({})", field.number);
        write_value(out, &field.kind, "x", 2, 0);
        out.push_str("        n += 1\n");
    }
    out.push_str("    w.close_map(head, n)\n");
}

/// The lines that write `value` of a kind, `indent` levels in; a list's
/// items are named by how deep the list is.
fn write_value(out: &mut String, kind: &Kind, value: &str, indent: usize, depth: usize) {
    let pad = "    ".repeat(indent);
    match kind {
        Kind::List(item) => {
            let each = format!("item{depth}");
            let _ = writeln!(out, "{pad}w.array(len({value}))");
            let _ = writeln!(out, "{pad}for {each} in {value}:");
            write_value(out, item, &each, indent + 1, depth + 1);
        }
        Kind::Names(item) => {
            let each = format!("name{depth}");
            let _ = writeln!(out, "{pad}w.map(len({value}))");
            // a str's code points sort as its UTF-8 bytes do, the profile's order
            let _ = writeln!(out, "{pad}for {each} in sorted({value}):");
            let _ = writeln!(out, "{pad}    w.str({each})");
            write_value(out, item, &format!("{value}[{each}]"), indent + 1, depth + 1);
        }
        _ => {
            let _ = writeln!(out, "{pad}{}", write_one(kind, value));
        }
    }
}

fn write_one(kind: &Kind, value: &str) -> String {
    match kind {
        Kind::Bool => format!("w.bool({value})"),
        Kind::Uint | Kind::Duration => format!("w.uint({value})"),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => format!("w.int({value})"),
        Kind::Float => format!("w.float({value})"),
        Kind::Str | Kind::Json => format!("w.str({value})"),
        Kind::Bin => format!("w.bin({value})"),
        Kind::Key => format!("key.write(w, {value})"),
        Kind::Value => format!("kv_value.write(w, {value})"),
        Kind::Cell => format!("sql_value.write(w, {value})"),
        Kind::Message(name) => format!("{}.write(w, {value})", type_name(name)),
        Kind::List(_) | Kind::Names(_) => unreachable!("a list or a map is written by its lines"),
    }
}

/// The reader: each field's value taken from the body as it comes, a key
/// twice refused and a key the schema does not have read past.
fn write_reader(out: &mut String, function: &str, fields: &[&Field]) {
    let _ = writeln!(out, "def _read_{function}(r: Reader) -> dict[str, Any]:");
    out.push_str("    read: dict[str, Any] = {}\n    seen: set[int] = set()\n");
    out.push_str("    for _ in range(r.map()):\n");
    if fields.is_empty() {
        out.push_str("        r.field(seen)\n        r.skip()\n");
    } else {
        out.push_str("        match r.field(seen):\n");
        for field in fields {
            let _ = writeln!(out, "            case {}:", field.number);
            read_into(out, &field.kind, &format!("read[\"{}\"]", python_field(&field.name)), 4, 0);
        }
        out.push_str("            case _:\n                r.skip()\n");
    }
    out.push_str("    r.leave()\n    return read\n");
}

/// The lines that read a value of a kind into `place`, `indent` levels in.
fn read_into(out: &mut String, kind: &Kind, place: &str, indent: usize, depth: usize) {
    let pad = "    ".repeat(indent);
    match kind {
        // the range is taken once, entering the list, before its items are read
        Kind::List(item) if !matches!(**item, Kind::List(_) | Kind::Names(_)) => {
            let items = format!("items{depth}");
            let _ = writeln!(out, "{pad}{items} = [{} for _ in range(r.array())]", read_one(item));
            let _ = writeln!(out, "{pad}r.leave()");
            assign(out, &pad, place, &items);
        }
        Kind::List(item) => {
            let items = format!("items{depth}");
            let _ = writeln!(out, "{pad}{items}: list[Any] = []");
            let _ = writeln!(out, "{pad}for _ in range(r.array()):");
            read_into(out, item, &format!("{items}.append"), indent + 1, depth + 1);
            let _ = writeln!(out, "{pad}r.leave()");
            assign(out, &pad, place, &items);
        }
        Kind::Names(item) => {
            let (named, name, seen) = (format!("named{depth}"), format!("name{depth}"), format!("names{depth}"));
            let _ = writeln!(out, "{pad}{named}: dict[str, Any] = {{}}");
            let _ = writeln!(out, "{pad}{seen}: set[str] = set()");
            let _ = writeln!(out, "{pad}for _ in range(r.map()):");
            let _ = writeln!(out, "{pad}    {name} = r.name({seen})");
            read_into(out, item, &format!("{named}[{name}]"), indent + 1, depth + 1);
            let _ = writeln!(out, "{pad}r.leave()");
            assign(out, &pad, place, &named);
        }
        _ => assign(out, &pad, place, &read_one(kind)),
    }
}

/// `place = value`, or `place(value)` where the place is a list's append.
fn assign(out: &mut String, pad: &str, place: &str, value: &str) {
    if place.ends_with(".append") {
        let _ = writeln!(out, "{pad}{place}({value})");
    } else {
        let _ = writeln!(out, "{pad}{place} = {value}");
    }
}

fn read_one(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "r.bool()".to_owned(),
        Kind::Uint | Kind::Duration => "r.uint()".to_owned(),
        Kind::Int | Kind::Int64 | Kind::Time | Kind::Nanos => "r.int()".to_owned(),
        Kind::Float => "r.float()".to_owned(),
        Kind::Str | Kind::Json => "r.str()".to_owned(),
        Kind::Bin => "r.bin()".to_owned(),
        Kind::Key => "key.read(r)".to_owned(),
        Kind::Value => "kv_value.read(r)".to_owned(),
        Kind::Cell => "sql_value.read(r)".to_owned(),
        Kind::Message(name) => format!("{}.read(r)", type_name(name)),
        Kind::List(_) | Kind::Names(_) => unreachable!("a list or a map is read by its lines"),
    }
}

/// A doc as comment lines, each within the lint's width.
fn write_comment(out: &mut String, doc: &[String], indent: &str) {
    for paragraph in doc {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && indent.len() + 2 + line.len() + 1 + word.len() > WIDTH {
                let _ = writeln!(out, "{indent}# {line}");
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if line.is_empty() {
            let _ = writeln!(out, "{indent}#");
        } else {
            let _ = writeln!(out, "{indent}# {line}");
        }
    }
}

/// A field's codec in the table: what its readers go by.
fn codec(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "bool_".to_owned(),
        Kind::Uint | Kind::Duration => "uint".to_owned(),
        Kind::Int | Kind::Time | Kind::Int64 | Kind::Nanos => "int_".to_owned(),
        Kind::Float => "float_".to_owned(),
        Kind::Str | Kind::Json => "str_".to_owned(),
        Kind::Bin => "bin_".to_owned(),
        Kind::Key => "key".to_owned(),
        Kind::Value => "kv_value".to_owned(),
        Kind::Cell => "sql_value".to_owned(),
        Kind::List(item) => format!("list_({})", codec(item)),
        Kind::Names(item) => format!("names({})", codec(item)),
        Kind::Message(name) => type_name(name),
    }
}

/// The messages with each declared after the ones it names, as a name must be.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_breaks_within_the_lints_width() {
        let mut out = String::new();
        let long = "word ".repeat(40);
        write_comment(&mut out, &[long.trim().to_owned(), String::new()], "    ");
        assert!(out.lines().all(|line| line.len() <= WIDTH), "{out}");
        assert!(out.ends_with("    #\n"), "an empty line of the doc is an empty comment");
        assert_eq!(codec(&Kind::List(Box::new(Kind::Names(Box::new(Kind::Key))))), "list_(names(key))");
    }
}
