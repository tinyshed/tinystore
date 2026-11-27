//! An index's synonyms: a short form, one word, paired with the phrase it
//! stands for, read both ways when a document is read, so that a search
//! pays nothing for them.
//!
//! ```text
//! кмзи ↔ криптографические методы защиты информации
//!
//! "Курс «Криптографических методов защиты информации»"
//!   its words, by stem: курс | криптографическ | метод | защит | информац
//!   the phrase stands there, adjacent and in order, by the same stems
//!   → кмзи is kept as a word of the field too
//! "Экзамен по КМЗИ"
//!   кмзи stands there → криптографические, методы, защиты, информации kept too
//! ```
//!
//! A word read for a synonym is kept in the postings as any other, and
//! counts in no field's length: a field's length is the words written in
//! it, so that a synonym never dilutes what its own words score. A word of
//! a phrase meets a word of a text by its stem where it has one, and by
//! itself where not, as a search meets a word.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use serde_json::{Value, json};

use super::analyzer::{self, Word};
use crate::{Error, Result};

/// The synonyms an index holds at most, the bytes of a short form, and the
/// words of a phrase.
const MOST_SYNONYMS: usize = 1000;
const MOST_SHORT: usize = 64;
const MOST_PHRASE: usize = 8;

/// A short form and its phrase as the index's open gave them, and their
/// words as the index reads them.
#[derive(Clone, Debug)]
struct Synonym {
    given: (String, String),
    short: Word,
    phrase: Vec<Word>,
}

/// An index's synonyms, each found by the key of its short form and of its
/// phrase's first word, so that reading a text is one pass over its words;
/// shared, since a search that says where it matched reads them after its
/// read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Synonyms(Arc<Held>);

#[derive(Debug, Default)]
struct Held {
    held: Vec<Synonym>,
    by_short: HashMap<String, Vec<usize>>,
    by_first: HashMap<String, Vec<usize>>,
}

impl PartialEq for Synonyms {
    fn eq(&self, other: &Synonyms) -> bool {
        self.given().eq(other.given())
    }
}

/// Where a synonym stands in a text's words: its phrase over a run of
/// them, or its short form at one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Standing<'s> {
    /// The phrase over `words`, whose short form the text holds too.
    Phrase { words: Range<usize>, short: &'s Word },
    /// The short form at `word`, whose phrase's words the text holds too.
    Short { word: usize, phrase: &'s [Word] },
}

/// The key a word of a phrase and a word of a text meet by.
fn key(word: &Word) -> &str {
    word.stem.as_deref().unwrap_or(&word.text)
}

impl Held {
    fn hold(&mut self, synonym: Synonym) {
        let at = self.held.len();
        self.by_short.entry(key(&synonym.short).to_owned()).or_default().push(at);
        self.by_first.entry(key(&synonym.phrase[0]).to_owned()).or_default().push(at);
        self.held.push(synonym);
    }
}

impl Synonyms {
    /// The synonyms `given`, checked: at most 1,000, each short form one
    /// word of 64 bytes at most, each phrase of 1 to 8 words, never its own
    /// short form, and no pair twice.
    pub(crate) fn new(given: &[(String, String)]) -> Result<Synonyms> {
        if given.len() > MOST_SYNONYMS {
            return Err(Error::invalid(format!("{} synonyms, past {MOST_SYNONYMS}", given.len())));
        }
        let mut synonyms = Held::default();
        for (at, (short, phrase)) in given.iter().enumerate() {
            let refused = |why: &str| Error::invalid(format!("synonym {short:?} for {phrase:?}: {why}"));
            if short.len() > MOST_SHORT {
                return Err(refused(&format!("a short form of {} bytes, past {MOST_SHORT}", short.len())));
            }
            let (mut shorts, words) = (analyzer::words(short), analyzer::words(phrase));
            if shorts.len() != 1 {
                return Err(refused("a short form is one word"));
            }
            if !(1..=MOST_PHRASE).contains(&words.len()) {
                return Err(refused("a phrase is of 1 to 8 words"));
            }
            let short = shorts.remove(0);
            if words.len() == 1 && key(&words[0]) == key(&short) {
                return Err(refused("a phrase that is its own short form"));
            }
            if given[..at].iter().any(|earlier| earlier == &given[at]) {
                return Err(refused("given twice"));
            }
            synonyms.hold(Synonym { given: given[at].clone(), short, phrase: words });
        }
        Ok(Synonyms(Arc::new(synonyms)))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.held.is_empty()
    }

    /// Each pair as the index's open gave it.
    pub(crate) fn given(&self) -> impl Iterator<Item = &(String, String)> {
        self.0.held.iter().map(|synonym| &synonym.given)
    }

    /// Where each synonym stands in `words`, a text's words in order: a
    /// phrase where its words follow one another, by their keys, and a short
    /// form where its word stands.
    pub(crate) fn standing<'s>(&'s self, words: &[Word]) -> Vec<Standing<'s>> {
        let Held { held: synonyms, by_short, by_first } = &*self.0;
        let mut found = Vec::new();
        for (at, word) in words.iter().enumerate() {
            for &held in by_first.get(key(word)).into_iter().flatten() {
                let Synonym { short, phrase, .. } = &synonyms[held];
                let run = at..at + phrase.len();
                let here = words.get(run.clone()).is_some_and(|run| run.iter().map(key).eq(phrase.iter().map(key)));
                if here {
                    found.push(Standing::Phrase { words: run, short });
                }
            }
            for &held in by_short.get(key(word)).into_iter().flatten() {
                found.push(Standing::Short { word: at, phrase: &synonyms[held].phrase });
            }
        }
        found
    }

    /// The words `words` holds besides its own for its synonyms: a phrase's
    /// short form where the phrase stands, and a short form's phrase where
    /// the short form does.
    pub(crate) fn extra(&self, words: &[Word]) -> Vec<&Word> {
        let mut extra = Vec::new();
        for standing in self.standing(words) {
            match standing {
                Standing::Phrase { short, .. } => extra.push(short),
                Standing::Short { phrase, .. } => extra.extend(phrase),
            }
        }
        extra
    }

    /// The pairs as the index's settings keep them; none for an index of no
    /// synonym, which keeps no key.
    pub(crate) fn to_json(&self) -> Option<Value> {
        (!self.is_empty()).then(|| json!(self.given().collect::<Vec<_>>()))
    }

    /// The synonyms an index's settings keep, none where they keep no key;
    /// none outer when they do not read.
    pub(crate) fn from_json(kept: &Value) -> Option<Synonyms> {
        if kept.is_null() {
            return Some(Synonyms::default());
        }
        let pair = |pair: &Value| Some((pair[0].as_str()?.to_owned(), pair[1].as_str()?.to_owned()));
        let given: Option<Vec<_>> = kept.as_array()?.iter().map(pair).collect();
        Synonyms::new(&given?).ok()
    }
}

#[cfg(test)]
#[path = "synonyms_tests.rs"]
mod tests;
