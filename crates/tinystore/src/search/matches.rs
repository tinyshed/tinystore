//! Where a search's words matched each of its hits, so that a search box
//! can show it: read from the hit's own text after the search's read, by
//! the keys the search found documents by, each with the query word it
//! came from.
//!
//! ```text
//! the search "алгебре"      reads алгебре, the stem алгебр, and алгебр as written
//! "Линейная алгебра и …"    reads линейная, алгебра (stem алгебр), и, …
//!                           so алгебра matched by its stem: "алгебра", 9..16 in characters
//! ```
//!
//! A hit's text is read again as its index read it, word by word, and a
//! word that holds a key the search read matched: whole, or the letters
//! typed of a word a prefix chose. A place in the normalized text reads back
//! as one in the text given through [`analyzer::mapped`], a grapheme at a
//! time; a text that does not fold alike that way gives no range at all,
//! rather than a wrong one.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::ops::Range;

use serde_json::Value;

use super::analyzer::{self, Word};
use super::document;
use super::score::Part;
use super::settings::Field;
use super::snippet::{Cut, Snippet};
use super::synonyms::{Standing, Synonyms};

/// Where a search's word matched a hit: a range of a field's text, and the
/// query's word that found it. `&text[at.clone()]` is the whole word for a
/// word found as written, by its stem or once corrected, and the letters
/// typed for a word a [`prefix`](super::SearchCall::prefix) began.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Match {
    /// The field's path, or a table's column; empty for a document that is
    /// its own text.
    pub field: String,
    /// The string's place in the list, for a field that holds a list.
    pub item: Option<usize>,
    /// The bytes of the string that matched.
    pub at: Range<usize>,
    /// The query's word, as the search's text wrote it.
    pub word: String,
}

/// What a match's range counts: bytes in Rust and Go, UTF-16 code units in
/// JavaScript, code points in Python, so that each slices its own strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(any(feature = "wire", test)),
    expect(dead_code, reason = "only the wire's clients count otherwise than Rust")
)]
pub(crate) enum Unit {
    Bytes,
    Utf16,
    Chars,
}

/// What a search asks of where its hits matched: its words as its text
/// wrote them, which each match names, what a range counts, whether each
/// hit lists its matches, and the snippet it cuts of each.
#[derive(Clone, Debug)]
pub(crate) struct Ask {
    pub(crate) words: Vec<String>,
    pub(crate) unit: Unit,
    pub(crate) listed: bool,
    pub(crate) snippet: Option<Cut>,
}

impl Ask {
    /// Each of `words`, the words of `text` normalized, as `text` wrote
    /// them; as the analyzer read them where `text` cannot be placed.
    pub(crate) fn new(text: &str, words: &[Word], unit: Unit) -> Ask {
        let mapped = analyzer::mapped(text);
        let written = words.iter().map(|word| match &mapped {
            Some(mapped) => text[mapped.original(word.at.clone())].to_owned(),
            None => word.text.clone(),
        });
        Ask { words: written.collect(), unit, listed: true, snippet: None }
    }
}

/// Where a search's words matched a hit, as it asked: its matches, when it
/// lists them, and its snippet.
#[derive(Debug, Default)]
pub(crate) struct Marked {
    pub(crate) matches: Vec<Match>,
    pub(crate) snippet: Option<Snippet>,
}

/// The keys a search found documents by, field by field, each with the
/// place of the query word it came from: what its hits' matches are read
/// against, gathered in its read and used after it.
#[derive(Debug)]
pub(crate) struct Sought {
    ask: Ask,
    fields: Vec<Keys>,
    /// The index's synonyms, whose words a search finds a text by too.
    synonyms: Synonyms,
}

/// The keys of one field a search read.
#[derive(Debug)]
struct Keys {
    path: String,
    number: u32,
    /// Words as written and stems, each with its query word's place.
    written: HashMap<String, usize>,
    stems: HashMap<String, usize>,
    /// The words a prefix chose, each with its query word's place and how
    /// many letters were typed of it.
    begun: HashMap<String, (usize, usize)>,
}

impl Sought {
    pub(crate) fn new(ask: Ask, fields: &[Field], synonyms: &Synonyms) -> Sought {
        let keys = |field: &Field| Keys {
            path: field.path.clone(),
            number: field.number,
            written: HashMap::new(),
            stems: HashMap::new(),
            begun: HashMap::new(),
        };
        Sought { ask, fields: fields.iter().map(keys).collect(), synonyms: synonyms.clone() }
    }

    /// A key the query word at `word` found documents by; of several words
    /// that give one key, the first names it.
    pub(crate) fn key(&mut self, (part, term): (Part, String), word: usize) {
        let Some(keys) = self.fields.iter_mut().find(|keys| keys.number == part.field()) else { return };
        let kept = if part.is_stems() { &mut keys.stems } else { &mut keys.written };
        kept.entry(term).or_insert(word);
    }

    /// A word of `part` that the query word at `word`, `letters` long, began
    /// as a prefix, which the search read in its place.
    pub(crate) fn begun(&mut self, (part, term): (Part, String), (word, letters): (usize, usize)) {
        if let Some(keys) = self.fields.iter_mut().find(|keys| keys.number == part.field()) {
            keys.begun.entry(term).or_insert((word, letters));
        }
    }

    /// Where the search's words matched a document kept as `json`; nothing
    /// for bytes that are not JSON, which no index keeps.
    pub(crate) fn marked_in(&self, json: &[u8]) -> Marked {
        serde_json::from_slice(json).map_or_else(|_| Marked::default(), |document| self.marked(&document))
    }

    /// Where the search's words matched `document`, a document or a table's
    /// row as its index reads one: field by field in the index's order, a
    /// list's strings in theirs, and each string's ranges in its text's;
    /// and the snippet of the field it asks one of, cut of the first string
    /// that matched, or of the first string where none did.
    pub(crate) fn marked(&self, document: &Value) -> Marked {
        let mut marked = Marked::default();
        let cut = self.ask.snippet.as_ref();
        for keys in &self.fields {
            let snipped = cut.filter(|cut| cut.field == keys.path);
            if !self.ask.listed && snipped.is_none() {
                continue;
            }
            // A field of something else than text is a document its index refused.
            let Ok(texts) = document::texts(document, &keys.path) else { continue };
            let mut first = None;
            for (item, text) in texts {
                let (ranges, words): (Vec<_>, Vec<_>) = keys.ranges(text, &self.synonyms).into_iter().unzip();
                if let Some(cut) = snipped
                    && marked.snippet.is_none()
                    && (!ranges.is_empty() || first.is_none())
                {
                    let snippet = cut.of(text, &ranges, self.ask.unit, item);
                    match ranges.is_empty() {
                        true => first = Some(snippet),
                        false => marked.snippet = Some(snippet),
                    }
                }
                if self.ask.listed {
                    for (at, word) in counted(text, ranges, self.ask.unit).into_iter().zip(words) {
                        let word = self.ask.words.get(word).cloned().unwrap_or_default();
                        marked.matches.push(Match { field: keys.path.clone(), item, at, word });
                    }
                }
            }
            marked.snippet = marked.snippet.take().or(first);
        }
        marked
    }
}

impl Keys {
    /// The bytes of `text` that the search's words matched, in its order and
    /// apart, each with its query word's place: a word itself, and the text
    /// a synonym stands for where a search found it by the synonym, a
    /// phrase whole for its short form and a short form for its phrase's
    /// words.
    fn ranges(&self, text: &str, synonyms: &Synonyms) -> Vec<(Range<usize>, usize)> {
        let Some(mapped) = analyzer::mapped(text) else { return Vec::new() };
        let words = analyzer::words_of(&mapped.normal);
        let mut found = Vec::new();
        for word in &words {
            if let Some(at) = self.sought(word) {
                found.push((mapped.original(word.at.clone()), at));
            }
            if let Some(&(at, letters)) = self.begun.get(&word.text) {
                found.push((mapped.original(typed(&mapped.normal, word, letters)), at));
            }
        }
        if !synonyms.is_empty() {
            for standing in synonyms.standing(&words) {
                let (from, at) = match standing {
                    Standing::Phrase { words: run, short } => {
                        (words[run.start].at.start..words[run.end - 1].at.end, self.found_by(short))
                    }
                    Standing::Short { word, phrase } => {
                        (words[word].at.clone(), phrase.iter().filter_map(|word| self.found_by(word)).min())
                    }
                };
                found.extend(at.map(|at| (mapped.original(from), at)));
            }
        }
        merged(found)
    }

    /// The place of the query word that found a synonym's `word`, as
    /// written, by its stem or begun by a prefix; of several, the first. A
    /// synonym marks the text that stands for it whole, however few of its
    /// letters were typed.
    fn found_by(&self, word: &Word) -> Option<usize> {
        let begun = [Some(&word.text), word.stem.as_ref()].into_iter().flatten();
        let begun = begun.filter_map(|key| self.begun.get(key)).map(|&(at, _)| at).min();
        [self.sought(word), begun].into_iter().flatten().min()
    }

    /// The place of the query word that found `word`, as written or by its
    /// stem; of two, the first.
    fn sought(&self, word: &Word) -> Option<usize> {
        let by_stem = word.stem.as_ref().and_then(|stem| self.stems.get(stem));
        [self.written.get(&word.text), by_stem].into_iter().flatten().min().copied()
    }
}

/// The start of `word` that holds its first `letters` letters: a word keeps
/// only the letters of its place, so `o'rei` of `o'reilly` holds four.
fn typed(normal: &str, word: &Word, letters: usize) -> Range<usize> {
    let mut counted = 0;
    for (at, c) in normal[word.at.clone()].char_indices() {
        counted += usize::from(analyzer::is_letter(c));
        if counted == letters {
            return word.at.start..word.at.start + at + c.len_utf8();
        }
    }
    word.at.clone()
}

/// Ranges in their text's order, those that overlap joined into one named
/// by the word of the first, the longest of those that start first: the
/// pairs of an unspaced script overlap, and a word typed whole and begun by
/// a prefix matches twice.
fn merged(mut found: Vec<(Range<usize>, usize)>) -> Vec<(Range<usize>, usize)> {
    found.sort_by_key(|(range, word)| (range.start, Reverse(range.end), *word));
    let mut merged: Vec<(Range<usize>, usize)> = Vec::with_capacity(found.len());
    for (range, word) in found {
        match merged.last_mut() {
            Some((last, _)) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push((range, word)),
        }
    }
    merged
}

/// Byte ranges of `text`, in its order and apart, counted in `unit`, the
/// text read once.
pub(crate) fn counted(text: &str, ranges: Vec<Range<usize>>, unit: Unit) -> Vec<Range<usize>> {
    let count: fn(&str) -> usize = match unit {
        Unit::Bytes => return ranges,
        Unit::Utf16 => |piece| piece.encode_utf16().count(),
        Unit::Chars => |piece| piece.chars().count(),
    };
    let (mut byte, mut units) = (0, 0);
    let mut to = |at: usize| {
        units += count(&text[byte..at]);
        byte = at;
        units
    };
    ranges.into_iter().map(|range| to(range.start)..to(range.end)).collect()
}

#[cfg(test)]
#[path = "matches_tests.rs"]
mod tests;
