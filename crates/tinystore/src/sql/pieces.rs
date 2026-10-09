//! The pieces a query's conditions are made of, each a piece of SQL with its
//! values, and the lexing that keeps them apart: a `?` counted outside quotes
//! and comments, an `and` or an `or` that would bind with a neighbour's.
//!
//! The Bun SDK's `sql` helpers write the same pieces; testdata/sql/queries.json
//! holds both to the same text.

use serde::Serialize;

use super::statement::Sql;
use super::values::{Value, to_value};

/// Conditions joined with `or`, in parentheses; an empty `or` finds nothing.
pub fn or<I>(conditions: I) -> Sql
where
    I: IntoIterator,
    I::Item: Into<Sql>,
{
    group(conditions, "or", "0")
}

/// Conditions joined with `and`, in parentheses, for a group inside an `or`;
/// an empty `and` finds everything.
pub fn and<I>(conditions: I) -> Sql
where
    I: IntoIterator,
    I::Item: Into<Sql>,
{
    group(conditions, "and", "1")
}

/// A column equal to a value, its name quoted; `None` is `is null`.
pub fn eq<T: Serialize + ?Sized>(column: &str, value: &T) -> Sql {
    let column = match quote(column) {
        Ok(column) => column,
        Err(why) => return Sql::refused(why),
    };
    match to_value(value) {
        Ok(Value::Null) => Sql::piece(format!("{column} is null"), Vec::new(), true),
        Ok(value) => Sql::piece(format!("{column} = ?"), vec![value], true),
        Err(why) => Sql::refused(format!("{column}: {why}")),
    }
}

/// A column holding one of `values`: `in (…)`, written as one value SQLite
/// reads with `json_each`, so a list of any length is the same statement and
/// an empty one finds nothing.
pub fn is_in<T: Serialize>(column: &str, values: &[T]) -> Sql {
    let column = match quote(column) {
        Ok(column) => column,
        Err(why) => return Sql::refused(why),
    };
    let listed = list(values);
    let (text, values, refused) = listed.into_parts();
    match refused {
        Some(why) => Sql::refused(why),
        None => Sql::piece(format!("{column} in {text}"), values, true),
    }
}

/// A list for `in (…)`, as one JSON value SQLite reads with `json_each`.
pub fn list<T: Serialize>(values: &[T]) -> Sql {
    match serde_json::to_string(values) {
        Ok(json) => Sql::piece("(select value from json_each(?))".to_owned(), vec![Value::Text(json)], false),
        Err(error) => Sql::refused(format!("a list's JSON: {error}")),
    }
}

/// A value kept as its JSON.
pub fn json<T: Serialize + ?Sized>(value: &T) -> Sql {
    match serde_json::to_string(value) {
        Ok(json) => Sql::piece("?".to_owned(), vec![Value::Text(json)], true),
        Err(error) => Sql::refused(format!("a value's JSON: {error}")),
    }
}

/// A name, quoted, never SQL: a column a request chooses.
pub fn ident(name: &str) -> Sql {
    match quote(name) {
        Ok(quoted) => Sql::piece(quoted, Vec::new(), true),
        Err(why) => Sql::refused(why),
    }
}

/// A JSON list in `column` holding `value`.
pub fn has<T: Serialize + ?Sized>(column: &str, value: &T) -> Sql {
    let column = match quote(column) {
        Ok(column) => column,
        Err(why) => return Sql::refused(why),
    };
    match to_value(value) {
        Ok(value) => {
            Sql::piece(format!("exists (select 1 from json_each({column}) where value = ?)"), vec![value], false)
        }
        Err(why) => Sql::refused(format!("{column}'s value: {why}")),
    }
}

/// A value as a piece of SQL, for a column an update sets.
pub fn value<T: Serialize + ?Sized>(value: &T) -> Sql {
    match to_value(value) {
        Ok(value) => Sql::piece("?".to_owned(), vec![value], true),
        Err(why) => Sql::refused(why),
    }
}

fn group<I>(conditions: I, joiner: &str, empty: &str) -> Sql
where
    I: IntoIterator,
    I::Item: Into<Sql>,
{
    let pieces: Vec<Sql> = conditions.into_iter().map(|condition| checked(condition.into())).collect();
    if pieces.is_empty() {
        return Sql::piece(empty.to_owned(), Vec::new(), false);
    }
    let joined = joined_with(pieces, joiner);
    let (text, values, refused) = joined.into_parts();
    match refused {
        Some(why) => Sql::refused(why),
        None => Sql::piece(format!("({text})"), values, false),
    }
}

/// A piece whose values are as many as its `?`s, counted outside quotes and
/// comments, and which names none; another is refused, since it would shift
/// the values of the pieces after it.
pub(crate) fn checked(piece: Sql) -> Sql {
    if piece.why_refused().is_some() {
        return piece;
    }
    let counted = placeholders(piece.text());
    if !counted.named.is_empty() || counted.numbered {
        return Sql::refused(format!("a piece of SQL takes its values at ? alone: {}", piece.text()));
    }
    if counted.positional != piece.values().len() {
        return Sql::refused(format!(
            "{} placeholders and {} values: each ? takes one value",
            counted.positional,
            piece.values().len()
        ));
    }
    piece
}

/// Pieces joined with `and` or `or`, each in parentheses unless it stands
/// alone; the first refused piece refuses them all.
pub(crate) fn joined_with(pieces: Vec<Sql>, joiner: &str) -> Sql {
    let several = pieces.len() > 1;
    let mut texts = Vec::with_capacity(pieces.len());
    let mut values = Vec::new();
    for piece in pieces {
        let wrapped = several && !piece.is_bare() && !stands_alone(piece.text());
        let (text, piece_values, refused) = piece.into_parts();
        if let Some(why) = refused {
            return Sql::refused(why);
        }
        texts.push(if wrapped { format!("({text})") } else { text });
        values.extend(piece_values);
    }
    Sql::piece(texts.join(&format!(" {joiner} ")), values, false)
}

/// A name as SQL quotes it: `"u"."created_at"`, each part on its own.
pub(crate) fn quote(name: &str) -> Result<String, String> {
    if name.is_empty() || name.contains('\0') {
        return Err(format!("the name {name:?}: a name is text without NUL"));
    }
    Ok(name.split('.').map(|part| format!("\"{}\"", part.replace('"', "\"\""))).collect::<Vec<_>>().join("."))
}

/// What a text's placeholders are, outside quotes and comments.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Counted {
    pub(crate) positional: usize,
    pub(crate) named: Vec<String>,
    pub(crate) numbered: bool,
}

pub(crate) fn placeholders(text: &str) -> Counted {
    let mut counted = Counted::default();
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        match c {
            '\'' | '"' | '`' => at = closing(&chars, at, c),
            '[' => at = find(&chars, at + 1, &[']']).unwrap_or(chars.len()),
            '-' if chars.get(at + 1) == Some(&'-') => at = find(&chars, at, &['\n']).unwrap_or(chars.len()),
            '/' if chars.get(at + 1) == Some(&'*') => at = find_pair(&chars, at + 2).map_or(chars.len(), |end| end + 1),
            '?' if chars.get(at + 1).is_some_and(char::is_ascii_digit) => counted.numbered = true,
            '?' => counted.positional += 1,
            ':' | '@' | '$' if chars.get(at + 1).is_some_and(|next| next.is_ascii_alphabetic() || *next == '_') => {
                let name: String =
                    chars[at + 1..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == '_').collect();
                at += name.chars().count();
                if !counted.named.contains(&name) {
                    counted.named.push(name);
                }
            }
            _ => {}
        }
        at += 1;
    }
    counted
}

/// Whether a piece keeps its meaning beside others in an `and` or an `or`:
/// one group, or text with no `and` or `or` of its own outside quotes,
/// comments and parentheses, which would bind with its neighbours'.
pub(crate) fn stands_alone(text: &str) -> bool {
    if grouped(text) {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut depth = 0i32;
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        match c {
            '\'' | '"' | '`' => at = closing(&chars, at, c),
            '-' if chars.get(at + 1) == Some(&'-') => at = find(&chars, at, &['\n']).unwrap_or(chars.len()),
            '/' if chars.get(at + 1) == Some(&'*') => at = find_pair(&chars, at + 2).map_or(chars.len(), |end| end + 1),
            '(' => depth += 1,
            ')' => depth -= 1,
            c if depth == 0
                && c.is_ascii_alphabetic()
                && !word_char(at.checked_sub(1).and_then(|at| chars.get(at))) =>
            {
                let word: String =
                    chars[at..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '$').collect();
                if word.eq_ignore_ascii_case("and") || word.eq_ignore_ascii_case("or") {
                    return false;
                }
                at += word.chars().count() - 1;
            }
            _ => {}
        }
        at += 1;
    }
    true
}

/// Whether text is one group in parentheses, or a bare `0` or `1`.
pub(crate) fn grouped(text: &str) -> bool {
    if !text.starts_with('(') || !text.ends_with(')') {
        return text == "0" || text == "1";
    }
    let mut depth = 0i32;
    let last = text.chars().count() - 1;
    for (at, c) in text.chars().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth == 0 && at < last {
            return false;
        }
    }
    true
}

fn word_char(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
}

/// The index of a quoted run's closing quote, a doubled quote inside kept.
fn closing(chars: &[char], at: usize, quote: char) -> usize {
    let mut next = at + 1;
    while next < chars.len() {
        if chars[next] == quote {
            if chars.get(next + 1) == Some(&quote) {
                next += 2;
                continue;
            }
            return next;
        }
        next += 1;
    }
    chars.len()
}

fn find(chars: &[char], from: usize, any: &[char]) -> Option<usize> {
    chars.get(from..)?.iter().position(|c| any.contains(c)).map(|at| from + at)
}

/// The index of the `*` of the `*/` that closes a comment.
fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&at| chars[at] == '*' && chars[at + 1] == '/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_mark_in_a_string_or_a_comment_is_no_placeholder() {
        let counted = placeholders("a = ? and b = '?' -- why?\n and c = :c and d = ?3 /* ? */");
        assert_eq!(counted, Counted { positional: 1, named: vec!["c".to_owned()], numbered: true });
    }

    #[test]
    fn a_piece_with_an_and_or_an_or_of_its_own_does_not_stand_alone() {
        assert!(stands_alone("amount >= ?"));
        assert!(stands_alone("(a or b)"));
        assert!(stands_alone("title = 'this or that'"));
        assert!(stands_alone("lower(a) = ? -- and"));
        assert!(!stands_alone("a = 1 or b = 2"));
        assert!(!stands_alone("total between ? and ?"));
        assert!(!stands_alone("(a) or (b)"));
    }
}
