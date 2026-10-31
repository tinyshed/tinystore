//! Keyboard layouts an index's readers type with, and a search's text read
//! as the same keys type in another of them: a word typed with the wrong
//! layout on finds nothing, and no typo is that far from it.
//!
//! ```text
//! rhbgnj   on QWERTY's keys  →  крипто on ЙЦУКЕН's
//! by;      →  инж                ЙЦУКЕН's `ж` is QWERTY's `;`
//! знерщт   →  python
//! ```
//!
//! A piece of the text between spaces is read so when none of its words
//! finds anything as typed, and the reading is kept only when every word it
//! gives finds something, so a hit as typed always ranks as it did.

use std::ops::Range;

use rusqlite::Connection;

use super::analyzer::{self, Word};
use super::read::{Asked, begins_a_word, halfway, held, prefix_of};
use crate::{Error, Result};

/// A layout an index may name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// QWERTY, as US English types.
    En,
    /// ЙЦУКЕН, as Russian types.
    Ru,
}

/// Each layout's characters key by key, the top row's left first, as its
/// keys type them alone and with shift: the same place is the same key.
const EN: [&str; 2] =
    ["`1234567890-=qwertyuiop[]\\asdfghjkl;'zxcvbnm,./", "~!@#$%^&*()_+QWERTYUIOP{}|ASDFGHJKL:\"ZXCVBNM<>?"];
const RU: [&str; 2] =
    ["ё1234567890-=йцукенгшщзхъ\\фывапролджэячсмитьбю.", "Ё!\"№;%:?*()_+ЙЦУКЕНГШЩЗХЪ/ФЫВАПРОЛДЖЭЯЧСМИТЬБЮ,"];

impl Layout {
    /// The layout a name gives, refusing one no release knows yet.
    pub(crate) fn named(name: &str) -> Result<Layout> {
        match name {
            "en" => Ok(Layout::En),
            "ru" => Ok(Layout::Ru),
            name => Err(Error::invalid(format!("a layout {name:?}: the layouts known are ru and en"))),
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Layout::En => "en",
            Layout::Ru => "ru",
        }
    }

    fn keys(self) -> [&'static str; 2] {
        match self {
            Layout::En => EN,
            Layout::Ru => RU,
        }
    }

    /// Where `c` lies on the layout: its level, alone or with shift, and its key.
    fn place(self, c: char) -> Option<(usize, usize)> {
        let keys = self.keys();
        (0..keys.len()).find_map(|level| keys[level].chars().position(|key| key == c).map(|key| (level, key)))
    }

    /// Whether the layout types every character of `piece`, a letter among them.
    fn types(self, piece: &str) -> bool {
        piece.chars().all(|c| self.place(c).is_some()) && piece.chars().any(char::is_alphabetic)
    }

    /// `piece`, which this layout types, as the same keys type in `other`.
    fn retyped(self, piece: &str, other: Layout) -> String {
        let at = |(level, key): (usize, usize)| other.keys()[level].chars().nth(key);
        piece.chars().map(|c| self.place(c).and_then(at).unwrap_or(c)).collect()
    }
}

/// The layouts `names` give, each once.
pub(crate) fn layouts<S: AsRef<str>>(names: &[S]) -> Result<Vec<Layout>> {
    let mut layouts = Vec::with_capacity(names.len());
    for name in names {
        let layout = Layout::named(name.as_ref())?;
        if layouts.contains(&layout) {
            return Err(Error::invalid(format!("layout {} is given twice", layout.name())));
        }
        layouts.push(layout);
    }
    if layouts.len() == 1 {
        return Err(Error::invalid("a single layout: a word typed in one is read in another, so name two"));
    }
    Ok(layouts)
}

/// `piece` as the same keys type in each other layout of `layouts`, from
/// the first that types it; none when none does.
fn readings(piece: &str, layouts: &[Layout]) -> Vec<String> {
    let Some(typed) = layouts.iter().copied().find(|layout| layout.types(piece)) else { return Vec::new() };
    layouts.iter().copied().filter(|other| *other != typed).map(|other| typed.retyped(piece, other)).collect()
}

/// Where each run of `text` between spaces lies.
fn pieces(text: &str) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(from)) => {
                pieces.push(from..at);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        pieces.push(from..text.len());
    }
    pieces
}

/// Reads each piece of the search's text whose words find nothing as typed
/// in another of its index's layouts, where every word that gives finds
/// something: held, or begun when it is the word a prefix search reads as
/// the start of others. The query's text and words become those read; says
/// whether any piece was read so.
///
/// A word read so finds something, so the typo correction after it leaves
/// the word as it is; and since only what found nothing is read so, a hit
/// as typed ranks as it did.
pub(super) fn swap(connection: &Connection, asked: &mut Asked<'_>) -> Result<bool> {
    if asked.settings.layouts.is_empty() {
        return Ok(false);
    }
    let query = &asked.query;
    let mut swaps: Vec<(Range<usize>, String)> = Vec::new();
    for piece in pieces(&query.normal) {
        let inside = |word: &&Word| piece.start <= word.at.start && word.at.end <= piece.end;
        let typed: Vec<&Word> = query.words.iter().filter(inside).collect();
        if typed.is_empty() || finds_typed(connection, asked, &typed)? {
            continue;
        }
        let last = piece.end == query.normal.len();
        for reading in readings(&query.normal[piece.clone()], &asked.settings.layouts) {
            let reading = analyzer::normalize(&reading);
            if finds_read(connection, asked, &reading, last)? {
                swaps.push((piece.clone(), reading));
                break;
            }
        }
    }
    if swaps.is_empty() {
        return Ok(false);
    }
    let normal = rewritten(&asked.query.normal, swaps);
    asked.query.words = super::index::words_read(&normal);
    asked.query.normal = normal;
    Ok(true)
}

/// Whether any of a piece's words finds something as typed.
fn finds_typed(connection: &Connection, asked: &Asked<'_>, typed: &[&Word]) -> Result<bool> {
    let prefix = prefix_of(&asked.query).map(|word| word.at.clone());
    for word in typed {
        if held(connection, asked, word)? > 0 {
            return Ok(true);
        }
        if prefix.as_ref() == Some(&word.at) && begins_a_word(connection, asked, &word.text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether every word of a piece's reading finds something, its last read
/// as a prefix where it ends the search's text, `last`, as typed text does.
fn finds_read(connection: &Connection, asked: &Asked<'_>, reading: &str, last: bool) -> Result<bool> {
    let words = analyzer::words_of(reading);
    if words.is_empty() {
        return Ok(false);
    }
    for (at, word) in words.iter().enumerate() {
        if held(connection, asked, word)? > 0 {
            continue;
        }
        let prefix = asked.query.prefix && last && at == words.len() - 1 && halfway(word, reading);
        if !(prefix && begins_a_word(connection, asked, &word.text)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `text` with each piece written over by its reading, in order.
fn rewritten(text: &str, swaps: Vec<(Range<usize>, String)>) -> String {
    let mut written = String::with_capacity(text.len() * 2);
    let mut from = 0;
    for (piece, reading) in swaps {
        written.push_str(&text[from..piece.start]);
        written.push_str(&reading);
        from = piece.end;
    }
    written.push_str(&text[from..]);
    written
}

#[cfg(test)]
#[path = "layouts_tests.rs"]
mod tests;
