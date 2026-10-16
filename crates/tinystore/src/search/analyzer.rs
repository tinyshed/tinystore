//! What a document's text and a search's both become before the index sees
//! them. The same text always gives the same words, and which stemmer a word
//! meets is chosen by its script, never by guessing its language, so that a
//! search of two words and a long document agree.
//!
//! ```text
//! "Кари́бский кризис, café 世界和平"
//!   NFKC, lower case, stress marks off, ё → е, Latin accents folded
//!   words by Unicode's rules            → карибский | кризис | cafe
//!   a run of Han, kana or Thai          → 世界 | 界和 | 和平   (overlapping pairs)
//!   a stem where Snowball helps         → карибск
//! ```
//!
//! search-plan measured Russian's stems at +7–12 % and English's at a loss,
//! so only a Cyrillic word is stemmed.

use std::ops::Range;

use rust_stemmers::{Algorithm, Stemmer};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;
use unicode_segmentation::UnicodeSegmentation;

/// The analysis an index's words were read with. A release that reads text
/// otherwise raises it, and an index of an older one is read again.
pub(crate) const VERSION: u32 = 1;

/// A word of normalized text: as written, its stem where that differs, and
/// where it lies in the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Word {
    pub(crate) text: String,
    pub(crate) stem: Option<String>,
    pub(crate) at: Range<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    /// Written without spaces between words: Han, kana, Thai, Lao, Khmer, Myanmar.
    Unspaced,
    Other,
}

fn script(c: char) -> Script {
    match c as u32 {
        0x0041..=0x024F | 0x1E00..=0x1EFF => Script::Latin,
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Script::Cyrillic,
        0x0E00..=0x0EFF // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x3040..=0x30FF // kana
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF // Han
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0x20000..=0x3134F => Script::Unspaced,
        _ => Script::Other,
    }
}

/// The words of `text`, in order.
pub(crate) fn words(text: &str) -> Vec<Word> {
    words_of(&normalize(text))
}

/// The words of text [`normalize`] gave, each at its place in it.
pub(crate) fn words_of(normal: &str) -> Vec<Word> {
    let letters = |c: &char| c.is_alphanumeric() || is_combining_mark(*c);
    let mut words = Vec::new();
    for at in cut(normal) {
        let text: String = normal[at.clone()].chars().filter(letters).collect();
        words.push(Word { stem: stem(&text), text, at });
    }
    words
}

/// The stem of a word, where Snowball helps its script and the stem differs.
pub(crate) fn stem(word: &str) -> Option<String> {
    let first = word.chars().next()?;
    if script(first) != Script::Cyrillic {
        return None;
    }
    let stem = Stemmer::create(Algorithm::Russian).stem(word).into_owned();
    (stem != word).then_some(stem)
}

/// NFKC, lower case, the marks a Latin, Cyrillic or Greek letter carries
/// apart taken off (Russian Wikipedia's stress marks), ё as е, and Latin
/// accents folded.
pub(crate) fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut base = Script::Other;
    let mut greek = false;
    for c in text.nfkc().flat_map(char::to_lowercase) {
        if is_combining_mark(c) {
            if !(matches!(base, Script::Latin | Script::Cyrillic) || greek) {
                out.push(c);
            }
            continue;
        }
        base = script(c);
        greek = (0x0370..=0x03FF).contains(&(c as u32));
        match c {
            'ё' => out.push('е'),
            c if base == Script::Latin && !c.is_ascii() => {
                out.extend(std::iter::once(c).nfd().filter(|c| !is_combining_mark(*c)));
            }
            c => out.push(c),
        }
    }
    out
}

/// Where each word lies: words of letters, marks and digits by Unicode's
/// rules, and a run of an unspaced script cut out first and read as
/// overlapping pairs of its graphemes, since Unicode's rules split Han and
/// Thai a character at a time.
fn cut(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let (mut spaced_from, mut run_from) = (0, None);
    for (at, c) in text.char_indices() {
        let unspaced = script(c) == Script::Unspaced && (c.is_alphanumeric() || is_combining_mark(c));
        match (unspaced, run_from) {
            (true, None) => {
                spaced(text, spaced_from..at, &mut found);
                run_from = Some(at);
            }
            (false, Some(from)) => {
                pairs(text, from..at, &mut found);
                (spaced_from, run_from) = (at, None);
            }
            _ => {}
        }
    }
    match run_from {
        Some(from) => pairs(text, from..text.len(), &mut found),
        None => spaced(text, spaced_from..text.len(), &mut found),
    }
    found
}

/// Words by Unicode's rules that hold a letter or a digit. A word that holds
/// what is neither a letter, a mark nor a digit, `can't`, is read without it
/// by [`words_of`], so that no word holds what the index splits on.
fn spaced(text: &str, within: Range<usize>, found: &mut Vec<Range<usize>>) {
    for (at, word) in text[within.clone()].unicode_word_indices() {
        if word.chars().any(|c| c.is_alphanumeric()) {
            found.push(within.start + at..within.start + at + word.len());
        }
    }
}

/// Overlapping pairs of a run's graphemes, so that a Thai vowel stays with
/// its consonant: `世界和平` → `世界 界和 和平`; one grapheme alone.
fn pairs(text: &str, within: Range<usize>, found: &mut Vec<Range<usize>>) {
    let starts: Vec<usize> = text[within.clone()].grapheme_indices(true).map(|(at, _)| within.start + at).collect();
    if starts.len() == 1 {
        found.push(within);
        return;
    }
    for (pair, start) in starts.iter().enumerate().take(starts.len().saturating_sub(1)) {
        let end = starts.get(pair + 2).copied().unwrap_or(within.end);
        found.push(*start..end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        words(text).into_iter().map(|word| word.text).collect()
    }

    #[test]
    fn stress_marks_and_yo_go_and_short_i_stays() {
        assert_eq!(texts("Кари́бский ЁЖ йод"), ["карибский", "еж", "йод"]);
    }

    #[test]
    fn unspaced_scripts_become_pairs_beside_spaced_words() {
        assert_eq!(texts("café 世界和平 abc"), ["cafe", "世界", "界和", "和平", "abc"]);
        assert_eq!(texts("東京タワー"), ["東京", "京タ", "タワ", "ワー"]);
        assert_eq!(texts("สวัสดี"), ["สวั", "วัส", "สดี"]);
        assert_eq!(texts("中"), ["中"]);
    }

    #[test]
    fn only_cyrillic_words_are_stemmed_and_only_where_the_stem_differs() {
        let stems: Vec<_> = words("кошки running кот").into_iter().map(|word| word.stem).collect();
        assert_eq!(stems, [Some("кошк".to_owned()), None, None]);
    }

    #[test]
    fn latin_accents_fold_and_a_word_keeps_only_its_letters_and_digits() {
        assert_eq!(texts("Schön, Ångström! can't 4K"), ["schon", "angstrom", "cant", "4k"]);
    }

    #[test]
    fn a_word_lies_where_its_place_in_the_normalized_text_says() {
        let normal = normalize("Helo, Wörld 世界");
        let places: Vec<&str> = words_of(&normal).iter().map(|word| &normal[word.at.clone()]).collect();
        assert_eq!(places, ["helo", "world", "世界"]);
    }
}
