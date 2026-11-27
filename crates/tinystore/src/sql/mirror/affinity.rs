//! What a table makes of the values a list gives it, so that a mirror
//! compares a given row with a held one as the file keeps it. SQLite converts
//! a value by its column's affinity as it stores it, and compares a key's
//! text by its index's collation; a mirror that compared the values as given
//! would hold `"4"` apart from a `4` its text key was given, and insert the
//! row again at every call, where its unique index refuses it.
//!
//! | Declared type holds         | Affinity | `4` given | `"12"` given | `"12 apples"` given |
//! |-----------------------------|----------|-----------|--------------|---------------------|
//! | INT                         | integer  | 4         | 12           | "12 apples"         |
//! | CHAR, CLOB or TEXT          | text     | "4"       | "12"         | "12 apples"         |
//! | BLOB, nothing, strict's ANY | none     | 4         | "12"         | "12 apples"         |
//! | REAL, FLOA or DOUB          | real     | 4.0       | 12.0         | "12 apples"         |
//! | anything else               | numeric  | 4         | 12           | "12 apples"         |

use rusqlite::Connection;

use crate::sql::values::Value;
use crate::sqlite::query_cached;
use crate::{Error, Result};

/// A real as text, as SQLite writes one into a text column.
const AS_TEXT: &str = "select cast(?1 as text)";
/// A number's text as a number, as SQLite stores it in a numeric column.
const AS_NUMERIC: &str = "select cast(?1 as numeric)";
/// A number's text as a real, as SQLite stores it in a real column.
const AS_REAL: &str = "select cast(?1 as real)";

/// What a column converts a value to as it stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Affinity {
    Integer,
    Text,
    None,
    Real,
    Numeric,
}

impl Affinity {
    /// A declared type's affinity, by SQLite's rules in their order.
    pub(crate) fn of(declared: &str, strict: bool) -> Affinity {
        let declared = declared.to_ascii_uppercase();
        let holds = |part: &str| declared.contains(part);
        if strict && declared == "ANY" {
            Affinity::None
        } else if holds("INT") {
            Affinity::Integer
        } else if holds("CHAR") || holds("CLOB") || holds("TEXT") {
            Affinity::Text
        } else if holds("BLOB") || declared.is_empty() {
            Affinity::None
        } else if holds("REAL") || holds("FLOA") || holds("DOUB") {
            Affinity::Real
        } else {
            Affinity::Numeric
        }
    }

    /// `value` as the column keeps it, or `None` when it keeps it as given.
    pub(crate) fn kept(self, connection: &Connection, value: &Value) -> Result<Option<Value>> {
        let cast = |sql| cast(connection, sql, value).map(Some);
        match (self, value) {
            (Affinity::Text, Value::Integer(n)) => Ok(Some(Value::Text(n.to_string()))),
            (Affinity::Text, Value::Real(_)) => cast(AS_TEXT),
            (Affinity::Integer | Affinity::Numeric, Value::Text(text)) if is_number(text) => cast(AS_NUMERIC),
            (Affinity::Real, Value::Text(text)) if is_number(text) => cast(AS_REAL),
            #[expect(clippy::cast_precision_loss, reason = "a real column keeps the integer as the nearest real")]
            (Affinity::Real, Value::Integer(n)) => Ok(Some(Value::Real(*n as f64))),
            _ => Ok(None),
        }
    }
}

fn cast(connection: &Connection, sql: &str, value: &Value) -> Result<Value> {
    let what = "a value as its column keeps it";
    let cast: Vec<Value> = query_cached(connection, (sql, what), [value], |row| Ok(Value::of(row.get_ref(0)?)))?;
    cast.into_iter().next().ok_or_else(|| Error::internal(format!("{what}: no row")))
}

/// Whether text is a number whole, as SQLite reads one before it converts
/// it: blanks around, a sign, digits with a point among them, an exponent.
///
/// ```text
/// " 12 ", "-1.5", ".5", "5.", "1e3"   → a number
/// "12 apples", "0x10", "1e", ".", ""  → text
/// ```
fn is_number(text: &str) -> bool {
    let blank = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r');
    let text = text.trim_matches(blank);
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(whole) && digits(fraction) && whole.len() + fraction.len() > 0;
    let exponent_ok = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    mantissa_ok && exponent_ok
}

/// How a key's index compares its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Collation {
    #[default]
    Binary,
    /// ASCII's capitals as their small letters, and nothing else.
    NoCase,
    /// Spaces at the end ignored.
    RTrim,
}

impl Collation {
    pub(crate) fn named(name: &str) -> Result<Collation> {
        match name.to_ascii_uppercase().as_str() {
            "BINARY" => Ok(Collation::Binary),
            "NOCASE" => Ok(Collation::NoCase),
            "RTRIM" => Ok(Collation::RTrim),
            other => Err(Error::invalid(format!(
                "a key's collation {other}, which a mirror cannot compare by: binary, nocase or rtrim"
            ))),
        }
    }

    /// The text two texts the collation calls one share.
    pub(crate) fn folded(self, text: &str) -> String {
        match self {
            Collation::Binary => text.to_owned(),
            Collation::NoCase => text.to_ascii_lowercase(),
            Collation::RTrim => text.trim_end_matches(' ').to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_number;

    #[test]
    fn a_number_is_whole_text_sqlite_reads_as_one() {
        for number in [" 12 ", "-1.5", "+.5", "5.", "1e3", "1E-3", "007"] {
            assert!(is_number(number), "{number:?}");
        }
        for text in ["12 apples", "0x10", "1e", ".", "", "-", "1.2.3", "inf", "NaN", "1 2"] {
            assert!(!is_number(text), "{text:?}");
        }
    }
}
