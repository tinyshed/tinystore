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
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::analyzer::{self, Word};
use crate::sync::lock;
use crate::{Error, Result};

/// The synonyms an index holds at most, the bytes of a short form, and the
/// words of a phrase.
const MOST_SYNONYMS: usize = 1000;
const MOST_SHORT: usize = 64;
const MOST_PHRASE: usize = 8;
/// The sets of synonyms read from settings that stay read, the latest last.
const MOST_READ: usize = 16;

/// The synonyms read from indexes' settings, so that the same pairs read
/// again are the same synonyms: a commit's settle and a search in a
/// transaction read an index's settings each time, and a set's words are
/// read once a process rather than at each of them.
static READ: Mutex<Vec<Synonyms>> = Mutex::new(Vec::new());

/// A short form and its phrase as the index reads them.
#[derive(Clone, Debug)]
struct Synonym {
    short: Word,
    phrase: Vec<Word>,
}

/// An index's synonyms, each found by the key of its short form and of its
/// phrase's first word, so that reading a text is one pass over its words;
/// shared, since a search that says where it matched reads them after its
/// read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Synonyms(Arc<Held>);

/// The pairs, sorted, so that the same pairs in another order are the same
/// synonyms and change nothing of an index, and the words of each pair that
/// reads as a synonym.
#[derive(Debug, Default)]
struct Held {
    given: Vec<(String, String)>,
    held: Vec<Synonym>,
    by_short: HashMap<String, Vec<usize>>,
    by_first: HashMap<String, Vec<usize>>,
}

impl PartialEq for Synonyms {
    fn eq(&self, other: &Synonyms) -> bool {
        self.0.given == other.0.given
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
    /// The pairs `given`, sorted, each read as the analyzer reads a text: an
    /// open's `Checked`, refusing a pair that reads as no synonym, and the
    /// settings' `Kept`, where such a pair stays and finds nothing, as a
    /// later analyzer may read a kept one, so that the index still opens and
    /// its rebuild reads it as its new open says.
    fn of(mut given: Vec<(String, String)>, given_by: Given) -> Result<Held> {
        given.sort_unstable();
        let mut held = Held { given, ..Held::default() };
        for pair in &held.given {
            let synonym = match synonym(pair) {
                Ok(synonym) => synonym,
                Err(why) if given_by == Given::Checked => {
                    let (short, phrase) = pair;
                    return Err(Error::invalid(format!("synonym {short:?} for {phrase:?}: {why}")));
                }
                Err(_) => continue,
            };
            let at = held.held.len();
            held.by_short.entry(key(&synonym.short).to_owned()).or_default().push(at);
            held.by_first.entry(key(&synonym.phrase[0]).to_owned()).or_default().push(at);
            held.held.push(synonym);
        }
        Ok(held)
    }
}

/// Who gives a set of synonyms: an open, whose pairs are checked, or an
/// index's settings, which keep what an open checked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Given {
    Checked,
    Kept,
}

impl Synonyms {
    /// The synonyms `given`, checked: at most 1,000, each short form one
    /// word of 64 bytes at most, each phrase of 1 to 8 words, none of them its
    /// short form, and no pair twice. Their order is not kept.
    pub(crate) fn new(given: &[(String, String)]) -> Result<Synonyms> {
        if given.len() > MOST_SYNONYMS {
            return Err(Error::invalid(format!("{} synonyms, past {MOST_SYNONYMS}", given.len())));
        }
        let held = Held::of(given.to_vec(), Given::Checked)?;
        if let Some(twice) = held.given.windows(2).find(|pairs| pairs[0] == pairs[1]) {
            let (short, phrase) = &twice[0];
            return Err(Error::invalid(format!("synonym {short:?} for {phrase:?}: given twice")));
        }
        Ok(Synonyms(Arc::new(held)))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.given.is_empty()
    }

    /// Each pair as the index's open gave it, sorted.
    pub(crate) fn given(&self) -> impl Iterator<Item = &(String, String)> {
        self.0.given.iter()
    }

    /// Where each synonym stands in `words`, a text's words in order: a
    /// phrase where its words follow one another, by their keys, and a short
    /// form where its word stands.
    pub(crate) fn standing<'s>(&'s self, words: &[Word]) -> Vec<Standing<'s>> {
        let Held { held: synonyms, by_short, by_first, .. } = &*self.0;
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
    /// none outer when they do not read. What its open checked is not
    /// checked again, and pairs read before are the synonyms read then.
    pub(crate) fn from_json(kept: &Value) -> Option<Synonyms> {
        if kept.is_null() {
            return Some(Synonyms::default());
        }
        let pairs = kept.as_array()?;
        let same = |read: &&Synonyms| {
            read.0.given.len() == pairs.len()
                && read.0.given.iter().zip(pairs).all(|((short, phrase), pair)| {
                    text_of(pair, 0) == Some(short.as_str()) && text_of(pair, 1) == Some(phrase.as_str())
                })
        };
        if let Some(synonyms) = lock(&READ).iter().find(same) {
            return Some(synonyms.clone());
        }
        let pair = |pair: &Value| Some((text_of(pair, 0)?.to_owned(), text_of(pair, 1)?.to_owned()));
        let given: Vec<_> = pairs.iter().map(pair).collect::<Option<_>>()?;
        let synonyms = Synonyms(Arc::new(Held::of(given, Given::Kept).ok()?));
        let mut read = lock(&READ);
        if read.len() == MOST_READ {
            read.remove(0);
        }
        read.push(synonyms.clone());
        Some(synonyms)
    }
}

/// The short form, `at` 0, or the phrase, 1, of a pair the settings keep.
fn text_of(pair: &Value, at: usize) -> Option<&str> {
    pair.get(at).and_then(Value::as_str)
}

/// A pair's words, or why an open cannot give it: a short form of more than
/// one word or past 64 bytes, a phrase of no word or more than 8, or one
/// that holds its short form among its words.
fn synonym((short, phrase): &(String, String)) -> std::result::Result<Synonym, String> {
    if short.len() > MOST_SHORT {
        return Err(format!("a short form of {} bytes, past {MOST_SHORT}", short.len()));
    }
    let (mut shorts, phrase) = (analyzer::words(short), analyzer::words(phrase));
    if shorts.len() != 1 {
        return Err("a short form is one word".to_owned());
    }
    if !(1..=MOST_PHRASE).contains(&phrase.len()) {
        return Err("a phrase is of 1 to 8 words".to_owned());
    }
    let short = shorts.remove(0);
    // a short form among its phrase's words would stand for itself, and its
    // phrase would find the short form wherever its word stands alone
    if let Some(itself) = phrase.iter().find(|word| key(word) == key(&short)) {
        return Err(format!("a phrase that holds its own short form, {:?}", itself.text));
    }
    Ok(Synonym { short, phrase })
}

#[cfg(test)]
#[path = "synonyms_tests.rs"]
mod tests;
