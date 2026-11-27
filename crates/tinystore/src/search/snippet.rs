//! A hit's snippet: the piece of a long field around the first place a
//! search's words matched it, cut where a word starts or ends, so that a
//! search box shows the sentence that matched rather than the field's start.
//!
//! ```text
//! around 12, the match "algebra":
//! "In the first term we read sets. Then linear algebra took three weeks of …"
//!                               ↑ 12 characters back      12 on ↑
//!                         cut at the word that starts after, and the one that ends before:
//! "…Then linear algebra took three…", the mark 13..20 in UTF-16 units
//! ```
//!
//! A character is a grapheme, what a reader takes for one, so that a cut
//! never splits an accent from its letter or an emoji in two. The words are
//! the analyzer's, read back into the text given, so a cut falls where the
//! index itself sees a word begin or end. The store adds no markup: a
//! field's text is the program's users' text, which only the program knows
//! how to escape.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use super::analyzer;
use super::matches::{Unit, counted};

/// The characters a snippet of the wire keeps on each side of its match
/// unless the query says; Rust's call always says.
#[cfg(feature = "wire")]
pub(crate) const AROUND: usize = 80;
/// The most characters a search may ask a snippet to keep on each side.
pub(crate) const MOST_AROUND: usize = 1000;

/// What marks where a snippet was cut.
const CUT: &str = "…";

/// The piece of a field around the first place a search's words matched a
/// hit, with every match inside it.
///
/// ```text
/// text  "…Then linear algebra took three weeks of…"
/// marks [13..20], counted as the search's matches are: bytes in Rust
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snippet {
    /// The field's own text, `…` where it was cut.
    pub text: String,
    /// Where the search's words matched inside `text`, in its order and
    /// apart.
    pub marks: Vec<Range<usize>>,
    /// The string's place in the list, for a field that holds a list.
    pub item: Option<usize>,
}

/// A snippet a search asks for: of which field, and how many characters on
/// each side of its first match.
#[derive(Clone, Debug)]
pub(crate) struct Cut {
    pub(crate) field: String,
    pub(crate) around: usize,
}

impl Cut {
    /// The snippet of `text`, whose matches are `ranges`, bytes of it in its
    /// order and apart: around the first, or the text's start where there is
    /// none, its marks counted in `unit`.
    pub(crate) fn of(&self, text: &str, ranges: &[Range<usize>], unit: Unit, item: Option<usize>) -> Snippet {
        let graphemes = Graphemes::of(text);
        let words = word_bounds(text);
        let kept = match ranges.first() {
            Some(first) => around(first, &graphemes, &words, self.around),
            None => graphemes.start_to(self.around.saturating_mul(2), &words),
        };
        let (before, after) = (if kept.start > 0 { CUT } else { "" }, if kept.end < text.len() { CUT } else { "" });
        let piece = format!("{before}{}{after}", &text[kept.clone()]);
        let inside = ranges.iter().filter(|range| kept.start <= range.start && range.end <= kept.end);
        let moved = inside.map(|range| range.start - kept.start + before.len()..range.end - kept.start + before.len());
        Snippet { marks: counted(&piece, moved.collect(), unit), text: piece, item }
    }
}

/// The bytes kept around `first`: from the first word that starts within
/// `around` characters before it, to the last that ends within `around`
/// characters after it, or the characters themselves where no word does.
fn around(first: &Range<usize>, graphemes: &Graphemes, words: &[Range<usize>], around: usize) -> Range<usize> {
    let (from, to) = (graphemes.back(first.start, around), graphemes.on(first.end, around));
    let start = match from {
        0 => 0,
        from => words.iter().map(|word| word.start).find(|start| (from..=first.start).contains(start)).unwrap_or(from),
    };
    let end = match to {
        to if to == graphemes.len => to,
        to => words.iter().rev().map(|word| word.end).find(|end| (first.end..=to).contains(end)).unwrap_or(to),
    };
    start..end
}

/// The words of `text` as the index reads them, as bytes of it; none for a
/// text whose letters cannot be placed, which is then cut by its graphemes.
fn word_bounds(text: &str) -> Vec<Range<usize>> {
    let Some(mapped) = analyzer::mapped(text) else { return Vec::new() };
    analyzer::words_of(&mapped.normal).into_iter().map(|word| mapped.original(word.at)).collect()
}

/// Where each grapheme of a text starts, and the text's length.
struct Graphemes {
    starts: Vec<usize>,
    len: usize,
}

impl Graphemes {
    fn of(text: &str) -> Graphemes {
        Graphemes { starts: text.grapheme_indices(true).map(|(at, _)| at).collect(), len: text.len() }
    }

    /// The place `count` graphemes before `at`, a grapheme's start.
    fn back(&self, at: usize, count: usize) -> usize {
        let index = self.starts.partition_point(|start| *start < at);
        self.starts.get(index.saturating_sub(count)).copied().unwrap_or(0)
    }

    /// The place `count` graphemes after `at`, a grapheme's start or the end.
    fn on(&self, at: usize, count: usize) -> usize {
        let index = self.starts.partition_point(|start| *start < at);
        self.starts.get(index.saturating_add(count)).copied().unwrap_or(self.len)
    }

    /// The text's first `count` graphemes, to the end of the last word that
    /// ends within them.
    fn start_to(&self, count: usize, words: &[Range<usize>]) -> Range<usize> {
        let to = self.on(0, count);
        if to == self.len {
            return 0..to;
        }
        let end = words.iter().rev().map(|word| word.end).find(|end| (1..=to).contains(end));
        0..end.unwrap_or(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str, first: Option<Range<usize>>, around: usize) -> Snippet {
        Cut { field: String::new(), around }.of(text, first.as_slice(), Unit::Utf16, None)
    }

    #[test]
    fn a_snippet_is_cut_where_a_word_starts_and_ends_around_its_first_match() {
        let text = "In the first term we read sets. Then linear algebra took three weeks of the term.";
        let at = text.find("algebra").unwrap();
        let snippet = cut(text, Some(at..at + 7), 12);
        assert_eq!(snippet.text, "…Then linear algebra took three…");
        assert_eq!((snippet.marks.len(), snippet.marks[0].clone()), (1, 13..20));
        assert_eq!(&snippet.text[snippet.marks[0].start + 2..snippet.marks[0].end + 2], "algebra");
    }

    #[test]
    fn a_snippet_without_a_match_is_the_texts_start_and_one_that_fits_is_whole() {
        let text = "Linear algebra and analytic geometry";
        assert_eq!(cut(text, None, 8).text, "Linear algebra…", "16 characters, to the last word they hold");
        assert_eq!(cut(text, None, 80).text, text);
        assert_eq!(cut(text, Some(7..14), 80).text, text, "nothing cut, no mark of a cut");
    }

    #[test]
    fn a_snippet_is_never_cut_inside_a_grapheme_and_counts_them_as_characters() {
        // a family emoji is seven code points and one grapheme
        let text = "👨‍👩‍👧 alpha beta gamma";
        let at = text.find("gamma").unwrap();
        let snippet = cut(text, Some(at..at + 5), 7);
        assert_eq!(snippet.text, "…beta gamma");
        let unspaced = "東京タワーへ行きました";
        let snippet = cut(unspaced, Some(0..6), 1);
        assert!(unspaced.starts_with(snippet.text.trim_end_matches('…')), "{snippet:?}");
    }
}
