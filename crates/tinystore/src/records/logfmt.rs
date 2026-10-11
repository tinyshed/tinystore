//! A logfmt line's pairs, kept as an event's fields when they spell the line
//! again byte for byte; a line of fewer than two pairs is not taken for one.
//! A bare value is kept as the JSON number, `true`, `false` or `null` it
//! spells, or else as a JSON string, and a quoted one as the JSON string it
//! is:
//!
//! ```text
//! level=info msg="slow request" ms=1200   → level "info", msg "slow request", ms 1200
//! level=info  msg=ok                      → stays text: two spaces between pairs
//! level=info msg="ok"                     → stays text: ok is spelled bare
//! ```

use serde::de::IgnoredAny;

use super::event::MAX_KEYS;
use crate::console::quoted;

pub(super) fn logfmt_fields(text: &str) -> Option<Vec<(String, String)>> {
    let mut fields = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if fields.len() == MAX_KEYS {
            return None;
        }
        let (key, value, next) = next_pair(rest)?;
        fields.push((key.to_owned(), value));
        rest = match next.strip_prefix(' ') {
            Some(after) => after,
            None if next.is_empty() => next,
            None => return None,
        };
    }
    (fields.len() >= 2 && spell(&fields) == text).then_some(fields)
}

/// The `key=value` `text` begins with, the value as its field keeps it, and
/// what follows: a bare value runs to the next space, a quoted one to its
/// closing quote.
fn next_pair(text: &str) -> Option<(&str, String, &str)> {
    let equals = text.find('=').filter(|equals| *equals >= 1 && is_bare(&text[..*equals]))?;
    let (key, text) = (&text[..equals], &text[equals + 1..]);
    if text.starts_with('"') {
        let end = closing_quote(text)?;
        let value = &text[..=end];
        return is_json(value).then(|| (key, value.to_owned(), &text[end + 1..]));
    }
    let end = text.find(' ').unwrap_or(text.len());
    is_bare(&text[..end]).then(|| (key, bare_value(&text[..end]), &text[end..]))
}

/// Text logfmt writes without quotes: no space, quote or control byte.
fn is_bare(text: &str) -> bool {
    !text.bytes().any(|byte| byte <= b' ' || byte == b'"' || byte == 0x7f)
}

/// Where the quoted text `text` begins with ends, past its escapes.
fn closing_quote(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 1,
            b'"' => return Some(at),
            _ => {}
        }
        at += 1;
    }
    None
}

fn is_json(text: &str) -> bool {
    serde_json::from_str::<IgnoredAny>(text).is_ok()
}

fn bare_value(bare: &str) -> String {
    let number = bare.starts_with(|first: char| first == '-' || first.is_ascii_digit()) && is_json(bare);
    if matches!(bare, "true" | "false" | "null") || number { bare.to_owned() } else { quoted(bare) }
}

/// Fields as a logfmt line: a JSON string bare when what it holds is bare
/// text, and quoted as it is otherwise; every other value as it is.
pub(super) fn spell(fields: &[(String, String)]) -> String {
    let pairs: Vec<String> = fields.iter().map(|(key, value)| format!("{key}={}", logfmt_value(value))).collect();
    pairs.join(" ")
}

fn logfmt_value(value: &str) -> String {
    let Some(held) = unquote(value) else { return value.to_owned() };
    let held = if held.contains('\\') {
        match serde_json::from_str::<String>(value) {
            Ok(decoded) => decoded,
            Err(_) => return value.to_owned(),
        }
    } else {
        held.to_owned()
    };
    if is_bare(&held) { held } else { value.to_owned() }
}

/// What a value in quotes holds, its escapes as they are.
pub(super) fn unquote(value: &str) -> Option<&str> {
    value.strip_prefix('"')?.strip_suffix('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logfmt_line_keeps_its_pairs_when_they_spell_it_again() {
        let pairs = |pairs: &[(&str, &str)]| -> Option<Vec<(String, String)>> {
            Some(pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect())
        };
        let lines = [
            (
                r#"level=info msg="slow request" ms=1200"#,
                pairs(&[("level", r#""info""#), ("msg", r#""slow request""#), ("ms", "1200")]),
            ),
            ("level=info  msg=ok", None),
            (r#"level=info msg="ok""#, None),
            (
                "t=2026-09-26T12:00:01Z path=/a?b=c empty= flag=true n=-1.5e3 zero=007",
                pairs(&[
                    ("t", r#""2026-09-26T12:00:01Z""#),
                    ("path", r#""/a?b=c""#),
                    ("empty", r#""""#),
                    ("flag", "true"),
                    ("n", "-1.5e3"),
                    ("zero", r#""007""#),
                ]),
            ),
            (
                r#"msg="say \"hi\"\tthere" dir=C:\tmp"#,
                pairs(&[("msg", r#""say \"hi\"\tthere""#), ("dir", r#""C:\\tmp""#)]),
            ),
            ("just text", None),
            ("one=pair", None),
            ("a=1 b=2 ", None),
            (r#"a=1 "b"=2"#, None),
            (r#"a=1 b="\x""#, None),
            (r#"a=1 b="unclosed"#, None),
        ];
        for (line, expected) in lines {
            let found = logfmt_fields(line);
            assert_eq!(found, expected, "{line}");
            if let Some(fields) = found {
                assert_eq!(spell(&fields), line, "{line} spells itself again");
            }
        }
    }
}
