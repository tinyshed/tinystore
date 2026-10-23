//! The tables of docs/wire.md that say what the schema says: the engine each
//! method's high byte names, and each engine's methods. They are written
//! between markers the page holds, so that a client written from the page
//! calls the numbers the server answers:
//!
//! ```text
//! <!-- written from protocol/*.wire by `just protocol`: methods of kv -->
//! | method   | name           | may   | request         | answer     |
//! | `0x0101` | kv.bucket.open | read  | `kv.BucketOpen` | `Handle`   |
//! <!-- end of what `just protocol` writes -->
//! ```

use super::schema::{Access, Answer, Method, Schema};

const BEGIN: &str = "<!-- written from protocol/*.wire by `just protocol`: ";
const END: &str = "<!-- end of what `just protocol` writes -->";

/// The page with every block between its markers written again; a block that
/// names no engine of the schema, or an engine whose methods have no block, is
/// refused.
pub(crate) fn write(schema: &Schema, page: &str) -> Result<String, String> {
    let mut out = String::with_capacity(page.len());
    let mut written = Vec::new();
    let mut rest = page;
    while let Some(at) = rest.find(BEGIN) {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let (marker, after) = from.split_once("-->\n").ok_or("a marker that does not end")?;
        let what = marker[BEGIN.len()..].trim();
        let end = after.find(END).ok_or_else(|| format!("no end to the block of {what}"))?;
        out.push_str(marker);
        out.push_str("-->\n");
        out.push_str(&block(schema, what)?);
        out.push_str(END);
        written.push(what.to_owned());
        rest = &after[end + END.len()..];
    }
    out.push_str(rest);
    if let Some(missing) = engines(schema).into_iter().find(|engine| !written.contains(&format!("methods of {engine}")))
    {
        return Err(format!("no block for the methods of {missing}"));
    }
    Ok(out)
}

/// The table a block's marker names: `engines`, or `methods of <engine>`.
fn block(schema: &Schema, what: &str) -> Result<String, String> {
    if what == "engines" {
        let mut rows = Vec::new();
        for engine in engines(schema) {
            let high =
                schema.methods.iter().find(|method| engine_of(method) == engine).map_or(0, |method| method.id >> 8);
            rows.push(vec![format!("`{high:#04x}`"), engine.to_owned()]);
        }
        return Ok(table(&["high byte", "engine"], &rows));
    }
    let engine = what.strip_prefix("methods of ").ok_or_else(|| format!("a block of {what}, which is no table"))?;
    let methods: Vec<&Method> = schema.methods.iter().filter(|method| engine_of(method) == engine).collect();
    if methods.is_empty() {
        return Err(format!("a block of the methods of {engine}, which has none"));
    }
    let rows = methods.iter().map(|method| row(method)).collect::<Vec<_>>();
    Ok(table(&["method", "name", "may", "request", "answer"], &rows))
}

fn row(method: &Method) -> Vec<String> {
    let may = match method.access {
        Access::Read => "read",
        Access::Write => "write",
        Access::Admin => "admin",
    };
    let answer = match &method.answer {
        Answer::Call(answer) => format!("`{answer}`"),
        Answer::Download { item, trailer } => format!("download `{item}` until `{trailer}`"),
        Answer::Handover(answer) => format!("handover `{answer}`"),
        Answer::Exchange { out, back } => format!("exchange `{out}` for `{back}`"),
    };
    let id = method.id;
    vec![format!("`{id:#06x}`"), method.name.clone(), may.to_owned(), format!("`{}`", method.request), answer]
}

/// The engines that have methods, in the order of their high bytes.
fn engines(schema: &Schema) -> Vec<&str> {
    let mut methods: Vec<&Method> = schema.methods.iter().collect();
    methods.sort_by_key(|method| method.id);
    let mut engines: Vec<&str> = Vec::new();
    for method in methods {
        let engine = engine_of(method);
        if !engines.contains(&engine) {
            engines.push(engine);
        }
    }
    engines
}

/// A method's engine: its name's first word, `kv` of `kv.bucket.open`.
fn engine_of(method: &Method) -> &str {
    method.name.split('.').next().unwrap_or_default()
}

/// A markdown table, its cells padded so that the pipes line up.
fn table(heads: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = heads.iter().map(|head| head.len()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    let line = |cells: &mut dyn Iterator<Item = String>| {
        let padded: Vec<String> = cells.zip(&widths).map(|(cell, width)| format!(" {cell:width$} ")).collect();
        format!("|{}|\n", padded.join("|"))
    };
    let mut out = line(&mut heads.iter().map(|head| (*head).to_owned()));
    out.push_str(&format!("|{}|\n", widths.iter().map(|width| "-".repeat(width + 2)).collect::<Vec<_>>().join("|")));
    for row in rows {
        out.push_str(&line(&mut row.iter().cloned()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax;

    fn schema(text: &str) -> Schema {
        let mut schema = Schema::default();
        schema.add(&syntax::parse(text).unwrap());
        schema
    }

    #[test]
    fn a_block_is_written_from_the_schema_and_the_page_around_it_kept() {
        let schema = schema(
            "message Empty {}\nmessage kv.Call {\n  1 handle: uint\n}\n\nread 0x0102 kv.get(kv.Call) -> Empty\n",
        );
        let page = format!("before\n{BEGIN}methods of kv -->\nstale\n{END}\nafter\n");
        let written = write(&schema, &page).unwrap();
        let expected = "| method   | name   | may  | request   | answer  |\n\
                        |----------|--------|------|-----------|---------|\n\
                        | `0x0102` | kv.get | read | `kv.Call` | `Empty` |\n";
        assert_eq!(written, format!("before\n{BEGIN}methods of kv -->\n{expected}{END}\nafter\n"));
        assert_eq!(write(&schema, &written).unwrap(), written, "written again, the same");
    }

    #[test]
    fn an_engine_whose_methods_have_no_block_is_refused() {
        let schema = schema("message Empty {}\n\nread 0x0102 kv.get(Empty) -> Empty\n");
        assert_eq!(write(&schema, "nothing\n").unwrap_err(), "no block for the methods of kv");
    }
}
