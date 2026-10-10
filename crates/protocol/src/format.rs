//! The `.wire` files' one layout, which `just protocol` writes and CI checks,
//! so that a schema reads as a table: a run of methods with their calls in
//! one column and their answers in the next, and a file's field comments in
//! one column.
//!
//! ```text
//! read  0x0102 kv.get(kv.Call)        -> kv.Entry
//! # A comment in a run is the doc of the method below it.
//! write 0x0104 kv.set(kv.Call)        -> kv.Written
//!
//! message kv.Call {
//!   1 handle: uint
//!   2 under: [key]          # the branch's owners, outermost first
//!   6 expiresAt: time?      # the expiry a write gives, as a time
//! }
//! ```

use std::fmt::Write as _;

use super::syntax::{Field, File, Line, Method};

/// A schema file's lines in their layout: what is laid out already prints as
/// it was.
pub(crate) fn print(file: &File) -> String {
    let lines: Vec<&Line> = file.iter().map(|(_, line)| line).collect();
    let column = comment_column(&lines);
    let mut out = String::new();
    let mut indent = "";
    let mut run = None;
    for (at, line) in lines.iter().enumerate() {
        match line {
            Line::Blank => {}
            Line::Comment(text) if text.is_empty() => out.push_str(&format!("{indent}#")),
            Line::Comment(text) => out.push_str(&format!("{indent}# {text}")),
            Line::Message { name, empty: true } => out.push_str(&format!("message {name} {{}}")),
            Line::Message { name, empty: false } => out.push_str(&format!("message {name} {{")),
            Line::End => out.push('}'),
            Line::Field(field) => write_field(&mut out, field, column),
            Line::Method(method) => write_method(&mut out, method, *run.get_or_insert_with(|| run_width(&lines[at..]))),
        }
        out.push('\n');
        match line {
            Line::Message { empty: false, .. } => indent = "  ",
            Line::End => indent = "",
            _ => {}
        }
        if !matches!(line, Line::Method(_) | Line::Comment(_)) {
            run = None;
        }
    }
    out
}

fn write_field(out: &mut String, field: &Field, column: usize) {
    let code = code(field);
    let _ = match &field.doc {
        Some(doc) => write!(out, "{code:<column$}# {doc}"),
        None => write!(out, "{code}"),
    };
}

fn write_method(out: &mut String, method: &Method, width: usize) {
    let Method { access, id, answer, .. } = method;
    let _ = write!(out, "{:<5} {id:#06x} {:<width$} -> {answer}", access.word(), call(method));
}

/// A field without its comment: `  4 value: value?`
fn code(field: &Field) -> String {
    let Field { number, name, kind, optional, .. } = field;
    format!("  {number} {name}: {kind}{}", if *optional { "?" } else { "" })
}

/// A method's name and what it takes: `kv.get(kv.Call)`
fn call(method: &Method) -> String {
    format!("{}({})", method.name, method.request)
}

/// The column a file's field comments start at: two past its longest
/// commented field.
fn comment_column(lines: &[&Line]) -> usize {
    let commented = lines.iter().filter_map(|line| match line {
        Line::Field(field) if field.doc.is_some() => Some(code(field).len()),
        _ => None,
    });
    commented.max().unwrap_or(0) + 2
}

/// The width of the calls in the run of methods that starts the lines: its
/// methods and the comments among them, up to a blank line or a message.
fn run_width(lines: &[&Line]) -> usize {
    let run = lines.iter().take_while(|line| matches!(line, Line::Method(_) | Line::Comment(_)));
    let calls = run.filter_map(|line| match line {
        Line::Method(method) => Some(call(method).len()),
        _ => None,
    });
    calls.max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::syntax::parse;
    use super::print;

    fn format(text: &str) -> String {
        print(&parse(text).unwrap())
    }

    fn text(lines: &[&str]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    #[test]
    fn methods_line_up_their_answers_and_fields_their_comments() {
        let written = text(&[
            "message kv.Call {",
            "  1 handle: uint   # its handle",
            "  6 expiresAt:  time?  # when",
            "  7 n: int64?",
            "}",
            "",
            "read 0x0102 kv.get(kv.Call) -> kv.Entry",
            "write  0x010a kv.list(kv.List)   ->  download kv.Entry until kv.Page",
        ]);
        let laid = text(&[
            "message kv.Call {",
            "  1 handle: uint      # its handle",
            "  6 expiresAt: time?  # when",
            "  7 n: int64?",
            "}",
            "",
            "read  0x0102 kv.get(kv.Call)  -> kv.Entry",
            "write 0x010a kv.list(kv.List) -> download kv.Entry until kv.Page",
        ]);
        assert_eq!(format(&written), laid);
        assert_eq!(format(&laid), laid, "what is laid out comes back as it was");
    }

    #[test]
    fn a_run_of_methods_keeps_its_columns_past_a_methods_doc() {
        let written = text(&[
            "read 0x0102 kv.get(kv.Call) -> kv.Entry",
            "#  Lists a branch.",
            "read 0x010a kv.list(kv.List) -> kv.Page",
            "",
            "write 0x0140 kv.tx(kv.Tx) -> kv.TxResults",
        ]);
        let laid = text(&[
            "read  0x0102 kv.get(kv.Call)  -> kv.Entry",
            "#  Lists a branch.",
            "read  0x010a kv.list(kv.List) -> kv.Page",
            "",
            "write 0x0140 kv.tx(kv.Tx) -> kv.TxResults",
        ]);
        assert_eq!(format(&written), laid);
    }

    #[test]
    fn comments_and_blank_lines_stay_as_they_were() {
        let laid = text(&[
            "# kv, as its book has it",
            "#",
            "",
            "# a call",
            "message kv.Page {",
            "  # what follows it",
            "  1 next: key?",
            "}",
            "message Empty {}",
        ]);
        assert_eq!(format(&laid), laid);
    }
}
