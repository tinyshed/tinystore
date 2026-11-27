//! A document as its index reads it: the text of each field, its words and
//! their stems counted by part, and the values of each filter.

use std::collections::BTreeMap;

use serde_json::Value;

use super::aliases::Aliases;
use super::analyzer::{self, Word};
use super::filter;
use super::score::Part;
use super::settings::{Field, Filter, shown};
use super::synonyms::Synonyms;
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

/// What a document gives an index besides its fields' own words: the
/// index's synonyms, and the document's own aliases.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Besides<'a> {
    pub(crate) synonyms: &'a Synonyms,
    pub(crate) aliases: &'a Aliases,
}

/// The words of a document's fields, and its filters' values, each a key
/// held once; and the words its synonyms and its aliases add, which count
/// in no length. An alias stands for its field's text once however many
/// strings the field holds, and for nothing where it holds none.
pub(crate) fn analyze(
    document: &Value,
    fields: &[Field],
    filters: &[Filter],
    besides: Besides<'_>,
) -> Result<Analyzed> {
    let mut terms: BTreeMap<(Part, String), u32> = BTreeMap::new();
    let mut lengths: BTreeMap<Part, u32> = BTreeMap::new();
    for field in fields {
        let parts = (Part::written(field.number), Part::stems(field.number));
        let texts = texts(document, &field.path)?;
        if !texts.is_empty() {
            besides.aliases.of_field(&field.path).cloned().for_each(|alias| count(&mut terms, parts, alias));
        }
        for (_, text) in texts {
            let words = analyzer::words(text);
            if !besides.synonyms.is_empty() {
                besides.synonyms.extra(&words).into_iter().cloned().for_each(|word| count(&mut terms, parts, word));
            }
            for word in words {
                *lengths.entry(parts.0).or_default() += 1;
                if word.stem.is_some() {
                    *lengths.entry(parts.1).or_default() += 1;
                }
                count(&mut terms, parts, word);
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

/// [`analyze`] of a document without synonyms or aliases.
#[cfg(test)]
pub(crate) fn analyze_plain(document: &Value, fields: &[Field], filters: &[Filter]) -> Result<Analyzed> {
    analyze(document, fields, filters, Besides { synonyms: &Synonyms::default(), aliases: &Aliases::default() })
}

/// Counts `word` once more in the field of parts `written` and `stems`.
fn count(terms: &mut BTreeMap<(Part, String), u32>, (written, stems): (Part, Part), word: Word) {
    if let Some(stem) = word.stem {
        *terms.entry((stems, stem)).or_default() += 1;
    }
    *terms.entry((written, word.text)).or_default() += 1;
}

/// The text at `path`: a string, or the strings of a list, each with its
/// place in the list; nothing where the document has no such field or holds
/// null there.
pub(crate) fn texts<'v>(document: &'v Value, path: &str) -> Result<Vec<(Option<usize>, &'v str)>> {
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
        Value::String(text) => Ok(vec![(None, text)]),
        Value::Array(items) => (0..)
            .zip(items)
            .filter(|(_, item)| !item.is_null())
            .map(|(place, item)| {
                let text = item.as_str().ok_or_else(|| not_text("a list holding more than strings"))?;
                Ok((Some(place), text))
            })
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
        let analyzed = analyze_plain(&document, &fields, &[]).unwrap();
        let (title, stems, tags) = (Part::written(0), Part::stems(0), Part::written(1));
        assert_eq!(analyzed.lengths, [(title, 3), (stems, 2), (tags, 2), (Part::written(2), 1)]);
        assert_eq!(analyzed.terms.iter().filter(|(part, _, _)| *part == stems).count(), 1);
        assert!(analyzed.terms.contains(&(stems, "кошк".to_owned(), 2)));
        assert!(analyzed.terms.contains(&(title, "и".to_owned(), 1)));
    }

    #[test]
    fn a_string_document_is_its_own_text_and_a_field_of_no_text_is_refused() {
        let analyzed = analyze_plain(&json!("hello world"), &[field("", 0)], &[]).unwrap();
        assert_eq!(analyzed.lengths, [(Part::written(0), 2)]);
        let refused = analyze_plain(&json!({ "year": 2024 }), &[field("year", 0)], &[]).unwrap_err();
        assert_eq!(refused.to_string(), "field year holds a number, not text: invalid");
    }
}
