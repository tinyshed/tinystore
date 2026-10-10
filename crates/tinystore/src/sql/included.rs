//! An included query's rows inside its row: how they are written by SQLite
//! and read back.
//!
//! The rows of a query a row includes come as one value of that row, each a
//! list of SQL literals in parentheses, `(1,'it''s',NULL),(2,X'00ff',1.5)`.
//! SQLite's `quote()` writes an integer, a float, a text and bytes so that
//! each reads back as it is kept, where JSON would lose bytes and the last
//! digits of a float. The Bun SDK writes and reads the same text;
//! testdata/sql/queries.json holds both to it.

use super::values::Value;

/// A column as an SQL literal that keeps it whole. `quote()` cuts a text at a
/// zero byte, so such a text goes as its bytes behind a `T`.
pub(crate) fn literal_of(column: &str) -> String {
    let c = format!("\"{}\"", column.replace('"', "\"\""));
    format!(
        "case when typeof({c}) = 'text' and instr(cast({c} as blob), x'00') > 0 \
         then 'T' || quote(cast({c} as blob)) else quote({c}) end"
    )
}

/// The names of the columns a select's text gives, in their order: a column's
/// own name, or what follows its `as`. An expression without a name is
/// refused, since its rows could not be given their fields.
pub(crate) fn names_of(select: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for item in items_of(select) {
        let Some(name) = aliased(item).or_else(|| column(item)) else {
            return Err(format!("the column {item:?} needs a name: write it as … as name"));
        };
        if names.contains(&name) {
            return Err(format!("two columns named {name}"));
        }
        names.push(name);
    }
    Ok(names)
}

/// A select's items: its text cut at the commas outside quotes and
/// parentheses.
fn items_of(select: &str) -> Vec<&str> {
    let bytes = select.as_bytes();
    let (mut items, mut depth, mut from, mut at) = (Vec::new(), 0i32, 0, 0);
    while at < bytes.len() {
        match bytes[at] {
            quote @ (b'\'' | b'"' | b'`') => match bytes[at + 1..].iter().position(|byte| *byte == quote) {
                Some(closing) => at += closing + 1,
                None => break,
            },
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                items.push(select[from..at].trim());
                from = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    items.push(select[from..].trim());
    items
}

/// The name after an item's `as`, when the item ends with one: `… as name`,
/// whatever the space around the word.
fn aliased(item: &str) -> Option<String> {
    let start = last_name_start(item)?;
    let before = item[..start].trim_end();
    let word = before.len().checked_sub(2).and_then(|at| before.get(at..))?;
    let apart = before.len() < start && before[..before.len() - 2].ends_with(char::is_whitespace);
    if apart && word.eq_ignore_ascii_case("as") { name(&item[start..]) } else { None }
}

/// Where the name an item ends with starts: a quoted one at its opening
/// quote, past the doubled quotes inside it, a bare one at its first
/// character.
fn last_name_start(item: &str) -> Option<usize> {
    let bytes = item.as_bytes();
    if bytes.last() != Some(&b'"') {
        let before = item.trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$');
        return (before.len() < item.len()).then_some(before.len());
    }
    let mut at = bytes.len() - 1;
    while at > 0 {
        at -= 1;
        if bytes[at] != b'"' {
            continue;
        }
        if at == 0 || bytes[at - 1] != b'"' {
            return Some(at);
        }
        at -= 1;
    }
    None
}

/// A plain column's own name: `title`, `p.title`, `"p"."title"`.
fn column(item: &str) -> Option<String> {
    match split_qualified(item) {
        Some((table, column)) => name(table).and(name(column)),
        None => name(item),
    }
}

/// A qualified name's two parts, cut at the dot outside its quotes.
fn split_qualified(item: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    for (at, c) in item.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '.' if !quoted => return Some((&item[..at], &item[at + 1..])),
            _ => {}
        }
    }
    None
}

/// A name as SQL writes one, bare or in double quotes, without its quotes.
fn name(text: &str) -> Option<String> {
    if let Some(inner) = text.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
        let bare = inner.replace("\"\"", "\"");
        return (!inner.is_empty() && !inner.replace("\"\"", "").contains('"')).then_some(bare);
    }
    let mut chars = text.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    (first && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')).then(|| text.to_owned())
}

/// The rows an included query gave, read from their literals.
pub(crate) fn literal_rows(text: &str) -> Result<Vec<Vec<Value>>, String> {
    let mut reader = Reader { text, at: 0 };
    let mut rows = Vec::new();
    while reader.at < text.len() {
        reader.expect('(')?;
        let mut row = vec![reader.literal()?];
        while reader.peek() == Some(',') {
            reader.at += 1;
            row.push(reader.literal()?);
        }
        reader.expect(')')?;
        rows.push(row);
        if reader.at < text.len() {
            reader.expect(',')?;
        }
    }
    Ok(rows)
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.at..].chars().next()
    }

    fn expect(&mut self, wanted: char) -> Result<(), String> {
        if self.peek() != Some(wanted) {
            return Err(self.broken());
        }
        self.at += wanted.len_utf8();
        Ok(())
    }

    fn broken(&self) -> String {
        let shown: String = self.text[self.at..].chars().take(20).collect();
        format!("included rows that do not read at {}: {shown}", self.at)
    }

    fn literal(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('\'') => self.quoted().map(Value::Text),
            Some('X') => self.hex().map(Value::Blob),
            Some('T') => {
                self.at += 1;
                String::from_utf8(self.hex()?).map(Value::Text).map_err(|_| self.broken())
            }
            _ if self.text[self.at..].starts_with("NULL") => {
                self.at += 4;
                Ok(Value::Null)
            }
            _ => self.number(),
        }
    }

    /// A text in single quotes, a quote inside it doubled.
    fn quoted(&mut self) -> Result<String, String> {
        let mut value = String::new();
        self.at += 1;
        loop {
            let Some(end) = self.text[self.at..].find('\'') else {
                return Err(self.broken());
            };
            value.push_str(&self.text[self.at..self.at + end]);
            self.at += end + 1;
            if self.peek() != Some('\'') {
                return Ok(value);
            }
            value.push('\'');
            self.at += 1;
        }
    }

    /// Bytes as `X'00ff'`.
    fn hex(&mut self) -> Result<Vec<u8>, String> {
        if !self.text[self.at..].starts_with("X'") {
            return Err(self.broken());
        }
        let digits = &self.text[self.at + 2..];
        let Some(end) = digits.find('\'') else {
            return Err(self.broken());
        };
        let bytes: Option<Vec<u8>> = (0..end)
            .step_by(2)
            .map(|at| digits.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
            .collect();
        let bytes = bytes.ok_or_else(|| self.broken())?;
        self.at += 2 + end + 1;
        Ok(bytes)
    }

    /// An integer, or a float, which `quote()` writes with a point or an
    /// exponent, an infinity as `9.0e+999`.
    fn number(&mut self) -> Result<Value, String> {
        let rest = &self.text[self.at..];
        let end = rest.find([',', ')']).unwrap_or(rest.len());
        let word = &rest[..end];
        let value = match word.parse::<i64>() {
            Ok(whole) => Value::Integer(whole),
            Err(_) => Value::Real(word.parse::<f64>().map_err(|_| self.broken())?),
        };
        self.at += end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selects_columns_are_named_by_themselves_or_by_their_as() {
        let names = names_of(r#"p.id, "p"."the title", upper(p.status) as status, count(*) AS "n", 'a, b' as c"#);
        assert_eq!(names.unwrap(), ["id", "the title", "status", "n", "c"]);
        assert!(names_of("p.id, count(*)").unwrap_err().contains("needs a name"));
        assert!(names_of("p.id, o.id").unwrap_err().contains("two columns named id"));
        assert!(names_of("cast(p.id as text)").is_err(), "an as inside an expression names nothing");
        assert_eq!(names_of("p.id  AS\n\"a \"\"b\"\"\", x\tas y").unwrap(), ["a \"b\"", "y"]);
        assert!(names_of("p.title t").is_err(), "a name without its as is no name");
        assert!(names_of("p.*").is_err());
    }

    #[test]
    fn rows_read_back_from_their_literals() {
        let rows =
            literal_rows("(1,'it''s, (odd)',NULL),(-9223372036854775808,X'00FF27',1.5),(2,TX'610062',-9.0e+999)");
        assert_eq!(
            rows.unwrap(),
            [
                vec![Value::Integer(1), Value::Text("it's, (odd)".into()), Value::Null],
                vec![Value::Integer(i64::MIN), Value::Blob(vec![0, 255, 39]), Value::Real(1.5)],
                vec![Value::Integer(2), Value::Text("a\0b".into()), Value::Real(f64::NEG_INFINITY)],
            ]
        );
        assert_eq!(literal_rows("").unwrap(), Vec::<Vec<Value>>::new());
        assert!(literal_rows("(1,'open").is_err());
        assert!(literal_rows("(1)(2)").is_err());
    }
}
