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
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};
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
    let mut words = Vec::new();
    for at in cut(normal) {
        let text: String = normal[at.clone()].chars().filter(|c| is_letter(*c)).collect();
        words.push(Word { stem: stem(&text), text, at });
    }
    words
}

/// Whether a word keeps `c` of its place in the text: a letter, a digit or
/// a mark, and not the `'` of `can't`.
pub(crate) fn is_letter(c: char) -> bool {
    c.is_alphanumeric() || is_combining_mark(c)
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
    Folding::new().fold(text, &mut out);
    out
}

/// Where [`normalize`] stands between two pieces of one text: the script of
/// the last letter it folded, which says whether a mark after it goes.
struct Folding {
    base: Script,
    greek: bool,
}

impl Folding {
    fn new() -> Folding {
        Folding { base: Script::Other, greek: false }
    }

    /// Appends `text` to `out` as [`normalize`] reads it, a mark at its start
    /// taken off or kept by the letter that ended the piece folded before.
    /// Text Unicode's quick check finds in NFKC already, as nearly all text
    /// is in any script, is not composed again: composing took a fifth of
    /// the analysis of MS MARCO's passages.
    fn fold(&mut self, text: &str, out: &mut String) {
        if is_nfkc_quick(text.chars()) == IsNormalized::Yes {
            self.fold_chars(text.chars(), out);
        } else {
            self.fold_chars(text.nfkc(), out);
        }
    }

    fn fold_chars(&mut self, text: impl Iterator<Item = char>, out: &mut String) {
        for c in text.flat_map(char::to_lowercase) {
            if is_combining_mark(c) {
                if !(matches!(self.base, Script::Latin | Script::Cyrillic) || self.greek) {
                    out.push(c);
                }
                continue;
            }
            self.base = script(c);
            self.greek = (0x0370..=0x03FF).contains(&(c as u32));
            match c {
                'ё' => out.push('е'),
                c if self.base == Script::Latin && !c.is_ascii() => {
                    out.extend(std::iter::once(c).nfd().filter(|c| !is_combining_mark(*c)));
                }
                c => out.push(c),
            }
        }
    }
}

/// Text [`normalize`] gave, and where each grapheme of the text it was given
/// went in it, so that a place in the one reads back as a place in the
/// other: normalizing changes lengths, `ﬁ` → `fi`, `Ａ` → `a`, `é` → `e`,
/// `İ` → `i̇`, and only `ё` → `е` keeps its bytes.
///
/// ```text
/// given   ﬁ    n  é      "ine", 1..4 of normal, lies in ﬁ, n and é,
/// normal  f i  n  e      so it reads back as "ﬁné", 0..6 of the text given
/// ```
pub(crate) struct Mapped {
    pub(crate) normal: String,
    /// Where each grapheme starts in the text given and in `normal`, and
    /// both their ends last.
    starts: Vec<(usize, usize)>,
}

/// The text `text` normalizes to, folded a grapheme at a time, with where
/// each grapheme went; none where that reads otherwise than [`normalize`]
/// reading it whole, since a place it gave would then be wrong. NFKC
/// composes and reorders letters only within a grapheme, so no text known
/// reads otherwise, and a test holds random text to it.
pub(crate) fn mapped(text: &str) -> Option<Mapped> {
    let mut folding = Folding::new();
    let mut normal = String::with_capacity(text.len());
    let mut starts = Vec::new();
    for (at, grapheme) in text.grapheme_indices(true) {
        starts.push((at, normal.len()));
        folding.fold(grapheme, &mut normal);
    }
    starts.push((text.len(), normal.len()));
    (normal == normalize(text)).then_some(Mapped { normal, starts })
}

impl Mapped {
    /// The bytes of the text given that `normal`, a range of the normalized
    /// text, came from: every grapheme it lies in, whole.
    pub(crate) fn original(&self, normal: Range<usize>) -> Range<usize> {
        // The last grapheme that starts at or before a byte holds it; one that
        // gave nothing starts where the next does, and is passed over.
        let holding = |at: usize| self.starts.partition_point(|(_, start)| *start <= at).saturating_sub(1);
        let first = holding(normal.start);
        let last = holding(normal.end.saturating_sub(1).max(normal.start));
        self.starts[first].0..self.starts[last + 1].0
    }
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
///
/// The rules break at every space unless what follows joins it, as a mark
/// does, so the text is read a piece between spaces at a time: a piece of
/// letters and digits the rules join, as most words are in any script
/// written with spaces, is one word, and only another piece, `can't` or
/// `3.14`, is read by the rules. Reading every letter by them took a third
/// of the analysis of MS MARCO's passages. A text where a piece begins with
/// what joins the space before it is read whole by the rules.
fn spaced(text: &str, within: Range<usize>, found: &mut Vec<Range<usize>>) {
    let piece = &text[within.clone()];
    if piece.split(' ').any(|part| part.chars().next().is_some_and(joins_a_space)) {
        return by_the_rules(text, within, found);
    }
    let mut at = within.start;
    for part in piece.split(' ') {
        if !part.is_empty() && part.chars().all(in_a_word) {
            found.push(at..at + part.len());
        } else if !part.is_empty() {
            by_the_rules(text, at..at + part.len(), found);
        }
        at += part.len() + 1;
    }
}

/// [`spaced`] by Unicode's rules alone.
fn by_the_rules(text: &str, within: Range<usize>, found: &mut Vec<Range<usize>>) {
    for (at, word) in text[within.clone()].unicode_word_indices() {
        if word.chars().any(|c| c.is_alphanumeric()) {
            found.push(within.start + at..within.start + at + word.len());
        }
    }
}

/// Code points below this the rules are asked about once each, at the
/// first text read; others are left to the rules every time.
const ASKED_BELOW: u32 = 0x3000;
const IN_A_WORD: u8 = 1;
const JOINS_A_SPACE: u8 = 2;

/// What the rules say of each code point below [`ASKED_BELOW`].
static ASKED: std::sync::LazyLock<Vec<u8>> =
    std::sync::LazyLock::new(|| (0..ASKED_BELOW).map(|code| char::from_u32(code).map_or(0, ask)).collect());

fn ask(c: char) -> u8 {
    let alone = |text: &str| {
        let mut words = text.unicode_word_indices();
        matches!((words.next(), words.next()), (Some((0, word)), None) if word == text)
    };
    let joined = c.is_alphanumeric()
        && !is_combining_mark(c)
        && script(c) != Script::Unspaced
        && alone(&c.to_string())
        && ["a", "1", "я"].iter().all(|side| alone(&format!("{side}{c}{side}")) && alone(&format!("{c}{side}")));
    let spaced = format!(" {c}");
    let joins = spaced.split_word_bound_indices().next().is_some_and(|(_, first)| first != " ");
    (u8::from(joined) * IN_A_WORD) | (u8::from(joins) * JOINS_A_SPACE)
}

/// Whether the rules join `c` into one word with the letters and digits on
/// either side of it.
fn in_a_word(c: char) -> bool {
    match c as u32 {
        _ if c.is_ascii() => c.is_ascii_alphanumeric(),
        code if code < ASKED_BELOW => ASKED[code as usize] & IN_A_WORD != 0,
        _ => false,
    }
}

/// Whether the rules join `c` to a space before it, or may: a mark does.
fn joins_a_space(c: char) -> bool {
    match c as u32 {
        _ if c.is_ascii() => false,
        code if code < ASKED_BELOW => ASKED[code as usize] & JOINS_A_SPACE != 0,
        _ => !c.is_alphanumeric(),
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

    /// Each word of `text` as the text given wrote it.
    fn written(text: &str) -> Vec<&str> {
        let mapped = mapped(text).unwrap();
        words_of(&mapped.normal).into_iter().map(|word| &text[mapped.original(word.at)]).collect()
    }

    #[test]
    fn a_word_reads_back_as_the_text_given_wrote_it_where_normalizing_changed_its_length() {
        let mapped = mapped("ﬁné").unwrap();
        assert_eq!((mapped.normal.as_str(), mapped.original(1..4)), ("fine", 0..6), "the doc comment's example");
        // the stress mark goes, İ lowers to i and a dot that goes, Ё keeps its bytes as Е
        assert_eq!(written("Кари́бский İstanbul ЁЖ"), ["Кари́бский", "İstanbul", "ЁЖ"]);
        // a decomposed é composes, then folds; full width narrows; ﬃ and ½ widen
        assert_eq!(written("cafe\u{301}, ＴＯＫＹＯ ﬃx"), ["cafe\u{301}", "ＴＯＫＹＯ", "ﬃx"]);
        // и with a breve apart composes into й, which stays
        assert_eq!(written("и\u{306}од"), ["и\u{306}од"]);
        assert_eq!(written("can't 世界和平"), ["can't", "世界", "界和", "和平"]);
    }

    /// Letters whose normalizing changes their length or composes them with
    /// the next, marks, unspaced scripts, emoji and what ends a word.
    const TRICKY: &[&str] = &[
        "a", "Z", "e", "и", "к", "ω", " ", ",", "'", "-", "\n", "\r", "4", "\u{301}", "\u{306}", "\u{307}", "\u{308}",
        "\u{200d}", "ﬁ", "ﬃ", "ﬆ", "İ", "ё", "Ё", "Ａ", "ｂ", "é", "Å", "\u{212b}", "ǅ", "ẞ", "ß", "ά", "\u{2126}",
        "ŉ", "ǰ", "½", "㎏", "Ⅻ", "\u{1100}", "\u{1161}", "\u{11a8}", "가", "世", "界", "タ", "ส", "\u{e31}", "क",
        "\u{94d}", "ष", "\u{b47}", "\u{b3e}", "👍", "🏽", "🇷", "🇺", "Я", "й", "ї", "Ґ", "ά", "ŉ", "ة", "ـ", "\u{5d0}",
        "\u{5b8}", "\u{651}", "x", "Q", "7", ".", ":", "_",
    ];

    #[test]
    fn a_piece_between_spaces_is_cut_as_the_rules_cut_the_text_whole() {
        let mut random = crate::search::fixture::Random::new(47);
        let alphabet: Vec<&str> = TRICKY.iter().copied().chain([" ", " ", " ", "«", "»", "—", "'", "3", "q"]).collect();
        for round in 0..40_000 {
            let length = random.below(24);
            let text: String = (0..length).map(|_| alphabet[random.below(alphabet.len() as u64) as usize]).collect();
            let (mut fast, mut whole) = (Vec::new(), Vec::new());
            spaced(&text, 0..text.len(), &mut fast);
            by_the_rules(&text, 0..text.len(), &mut whole);
            assert_eq!(fast, whole, "round {round}: {text:?}");
        }
        for code in 0..ASKED_BELOW {
            let Some(c) = char::from_u32(code) else { continue };
            for text in [format!("x{c}x y"), format!("{c}я {c}"), format!("1{c} {c}1"), format!(" {c}a b{c}")] {
                let (mut fast, mut whole) = (Vec::new(), Vec::new());
                spaced(&text, 0..text.len(), &mut fast);
                by_the_rules(&text, 0..text.len(), &mut whole);
                assert_eq!(fast, whole, "{text:?}");
            }
        }
    }

    #[test]
    fn text_the_quick_check_passes_folds_as_it_does_composed_again() {
        let mut random = crate::search::fixture::Random::new(46);
        for round in 0..20_000 {
            let length = random.below(16);
            let text: String = (0..length).map(|_| TRICKY[random.below(TRICKY.len() as u64) as usize]).collect();
            let mut composed = String::new();
            Folding::new().fold_chars(text.nfkc(), &mut composed);
            assert_eq!(normalize(&text), composed, "round {round}: {text:?}");
        }
    }

    #[test]
    fn random_text_folds_a_grapheme_at_a_time_as_it_folds_whole_and_each_word_reads_back() {
        let mut random = crate::search::fixture::Random::new(45);
        for round in 0..20_000 {
            let length = random.below(16);
            let text: String = (0..length).map(|_| TRICKY[random.below(TRICKY.len() as u64) as usize]).collect();
            let mapped = mapped(&text).unwrap_or_else(|| panic!("round {round}: {text:?} folds otherwise apart"));
            let mut start = 0;
            for word in words_of(&mapped.normal) {
                let given = mapped.original(word.at.clone());
                assert!(given.start >= start, "round {round}: {text:?}: a word starts before the one before it");
                let read = &text[given.clone()];
                assert!(normalize(read).contains(&mapped.normal[word.at.clone()]), "round {round}: {text:?} {read:?}");
                start = given.start;
            }
        }
    }
}
