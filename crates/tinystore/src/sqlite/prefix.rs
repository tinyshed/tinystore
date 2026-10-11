//! A prefix as a range of text, which an index walks rather than every row.

/// Bytes bound as text, which SQLite compares with a text column byte by
/// byte. A blob would not do: every text is less than every blob.
#[derive(Debug)]
pub(crate) struct TextBytes<'a>(pub(crate) &'a [u8]);

impl rusqlite::ToSql for TextBytes<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(rusqlite::types::ValueRef::Text(self.0)))
    }
}

/// The first text after every text starting with `prefix`, in SQLite's byte
/// order; none when the prefix is empty or only bytes that cannot grow.
///
/// ```text
/// "api-"  →  "api."
/// ```
pub(crate) fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = prefix.as_bytes().to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::prefix_end;

    #[test]
    fn a_prefix_ends_at_the_first_text_past_it() {
        assert_eq!(prefix_end("chat:42:"), Some(b"chat:42;".to_vec()));
        assert_eq!(prefix_end("a\u{e9}"), Some(vec![b'a', 0xc3, 0xaa]));
        assert_eq!(prefix_end(""), None);
    }
}
