//! A document as its index reads it: the text of each field, its words and
//! their stems counted by part, and the values of each filter.

use std::collections::BTreeMap;

use serde_json::Value;

use super::analyzer;
use super::filter;
use super::score::Part;
use super::settings::{Field, Filter, shown};
use crate::{Error, Result};

/// The most words a part's length counts: a length is kept in two bytes.
const MOST_LENGTH: u32 = u16::MAX as u32;

/// A document's keys and how many words each part of it has.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Analyzed {
    /// Each key once, in order, with how often the document holds it.
    pub(crate) terms: Vec<(Part, String, u32)>,
    /// Each part with words, in order, with how many.
    pub(crate) lengths: Vec<(Part, u32)>,
}

impl Analyzed {
    /// The length of `part`, as a posting of it keeps it.
    pub(crate) fn length(&self, part: Part) -> u32 {
        self.lengths.iter().find(|(of, _)| *of == part).map_or(0, |(_, len)| *len)
    }
}

/// The words of a document's fields, and its filters' values, each a key
/// held once.
pub(crate) fn analyze(document: &Value, fields: &[Field], filters: &[Filter]) -> Result<Analyzed> {
    let mut terms: BTreeMap<(Part, String), u32> = BTreeMap::new();
    let mut lengths: BTreeMap<Part, u32> = BTreeMap::new();
    for field in fields {
        let (written, stems) = (Part::written(field.number), Part::stems(field.number));
        for text in texts(document, &field.path)? {
            for word in analyzer::words(text) {
                *lengths.entry(written).or_default() += 1;
                if let Some(stem) = word.stem {
                    *lengths.entry(stems).or_default() += 1;
                    *terms.entry((stems, stem)).or_default() += 1;
                }
                *terms.entry((written, word.text)).or_default() += 1;
            }
        }
    }
    for filter in filters {
        for term in filter::held(document, &filter.path)? {
            terms.insert((Part::filter(filter.number), term), 1);
        }
    }
    Ok(Analyzed {
        terms: terms.into_iter().map(|((part, term), tf)| (part, term, tf)).collect(),
        lengths: lengths.into_iter().map(|(part, len)| (part, len.min(MOST_LENGTH))).collect(),
    })
}

/// The text at `path`: a string, or the strings of a list; nothing where
/// the document has no such field or holds null there.
fn texts<'v>(document: &'v Value, path: &str) -> Result<Vec<&'v str>> {
    let mut at = document;
    for name in path.split('.').filter(|name| !name.is_empty()) {
        match at.get(name) {
            Some(value) => at = value,
            None => return Ok(Vec::new()),
        }
    }
    let not_text = |what: &str| Error::invalid(format!("field {} holds {what}, not text", shown(path)));
    match at {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(vec![text]),
        Value::Array(items) => items
            .iter()
            .filter(|item| !item.is_null())
            .map(|item| item.as_str().ok_or_else(|| not_text("a list of more than strings")))
            .collect(),
        Value::Bool(_) => Err(not_text("a boolean")),
        Value::Number(_) => Err(not_text("a number")),
        Value::Object(_) => Err(not_text("an object")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn field(path: &str, number: u32) -> Field {
        Field { path: path.to_owned(), weight: 1.0, number }
    }

    #[test]
    fn a_document_counts_its_words_and_stems_by_part() {
        let document = json!({ "title": "Кошки и кошка", "tags": ["cat", null, "cats"], "author": { "name": "Ann" } });
        let fields = [field("title", 0), field("tags", 1), field("author.name", 2), field("missing", 3)];
        let analyzed = analyze(&document, &fields, &[]).unwrap();
        let (title, stems, tags) = (Part::written(0), Part::stems(0), Part::written(1));
        assert_eq!(analyzed.lengths, [(title, 3), (stems, 2), (tags, 2), (Part::written(2), 1)]);
        assert_eq!(analyzed.terms.iter().filter(|(part, _, _)| *part == stems).count(), 1);
        assert!(analyzed.terms.contains(&(stems, "кошк".to_owned(), 2)));
        assert!(analyzed.terms.contains(&(title, "и".to_owned(), 1)));
    }

    #[test]
    fn a_string_document_is_its_own_text_and_a_field_of_no_text_is_refused() {
        let analyzed = analyze(&json!("hello world"), &[field("", 0)], &[]).unwrap();
        assert_eq!(analyzed.lengths, [(Part::written(0), 2)]);
        let refused = analyze(&json!({ "year": 2024 }), &[field("year", 0)], &[]).unwrap_err();
        assert_eq!(refused.to_string(), "field year holds a number, not text: invalid");
    }
}
