//! The words nearest a misspelt one: a dictionary of words in an FST, read
//! through a Levenshtein automaton that counts two letters swapped as one
//! edit. It applies no rule of its own: each caller says how many edits it
//! looks across, since search reads a word of five letters with one and the
//! command line a command of three. Config, the command line and search use
//! it, none of them importing another, and a suggestion it gives changes
//! nothing by itself.
//!
//! ```
//! use tinystore::typos::{Dictionary, Near};
//!
//! let commands = Dictionary::new(["serve", "backup", "restore"]);
//! assert_eq!(commands.nearest("bakup", 1), Some(Near { word: "backup".to_owned(), edits: 1 }));
//! assert_eq!(commands.nearest("bkp", 1), None);
//! ```

use std::sync::OnceLock;

use fst::{Automaton, IntoStreamer, Set, Streamer};
use levenshtein_automata::{DFA, Distance, LevenshteinAutomatonBuilder, SINK_STATE};

/// The most edits a dictionary looks across: an automaton of three edits
/// takes far longer to build than every search it would serve.
pub const MOST_EDITS: u8 = 2;

pub struct Dictionary {
    words: Set<Vec<u8>>,
    /// The automatons' builders for one and two edits, made at their first use.
    builders: [OnceLock<LevenshteinAutomatonBuilder>; 2],
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary").field("words", &self.words.len()).finish()
    }
}

/// A word of the dictionary near the one asked about, and how many edits
/// apart: a letter added, dropped, changed, or two neighbours swapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Near {
    pub word: String,
    pub edits: u8,
}

impl Dictionary {
    pub fn new<S: AsRef<str>>(words: impl IntoIterator<Item = S>) -> Dictionary {
        let mut words: Vec<String> = words.into_iter().map(|word| word.as_ref().to_owned()).collect();
        words.sort_unstable();
        words.dedup();
        Dictionary {
            words: Set::from_iter(words).expect("sorted and deduplicated words build a set"),
            builders: [OnceLock::new(), OnceLock::new()],
        }
    }

    pub fn contains(&self, word: &str) -> bool {
        self.words.contains(word)
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The words within `edits` of `word`, at most [`MOST_EDITS`], the nearest
    /// first and words as near in their order; `word` itself is not near.
    pub fn within(&self, word: &str, edits: u8) -> Vec<Near> {
        let edits = edits.min(MOST_EDITS);
        if edits == 0 {
            return Vec::new();
        }
        let builder =
            self.builders[usize::from(edits) - 1].get_or_init(|| LevenshteinAutomatonBuilder::new(edits, true));
        let dfa = builder.build_dfa(word);
        let mut found = Vec::new();
        let mut stream = self.words.search(Walk(&dfa)).into_stream();
        while let Some(bytes) = stream.next() {
            if bytes != word.as_bytes() {
                // The set holds only what `new` was given, which was text.
                let near = String::from_utf8_lossy(bytes).into_owned();
                found.push(Near { edits: distance(&dfa, bytes), word: near });
            }
        }
        found.sort_by(|a, b| a.edits.cmp(&b.edits).then_with(|| a.word.cmp(&b.word)));
        found
    }

    /// The word nearest `word` within `edits`; of two as near, the first in order.
    pub fn nearest(&self, word: &str, edits: u8) -> Option<Near> {
        self.within(word, edits).into_iter().next()
    }
}

fn distance(dfa: &DFA, bytes: &[u8]) -> u8 {
    let state = bytes.iter().fold(dfa.initial_state(), |state, byte| dfa.transition(state, *byte));
    match dfa.distance(state) {
        Distance::Exact(edits) | Distance::AtLeast(edits) => edits,
    }
}

/// The automaton as the FST walks one.
struct Walk<'a>(&'a DFA);

impl Automaton for Walk<'_> {
    type State = u32;

    fn start(&self) -> u32 {
        self.0.initial_state()
    }

    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }

    fn can_match(&self, state: &u32) -> bool {
        *state != SINK_STATE
    }

    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(word: &str, edits: u8) -> Near {
        Near { word: word.to_owned(), edits }
    }

    #[test]
    fn two_letters_swapped_are_one_edit() {
        let flags = Dictionary::new(["--dir", "--log", "--idle"]);
        assert_eq!(flags.nearest("--dri", 1), Some(near("--dir", 1)));
    }

    #[test]
    fn the_nearest_comes_first_and_words_as_near_in_their_order() {
        let words = Dictionary::new(["port", "post", "sport", "ports"]);
        assert_eq!(words.within("prot", 2), [near("port", 1), near("ports", 2), near("post", 2), near("sport", 2)]);
    }

    #[test]
    fn a_word_is_not_near_itself_and_none_is_past_the_edits_asked() {
        let words = Dictionary::new(["backup", "restore"]);
        assert_eq!(words.within("backup", 2), []);
        assert_eq!(words.nearest("bakcup", 0), None);
        assert_eq!(words.nearest("bkp", 2), None);
    }

    #[test]
    fn edits_past_two_look_across_two() {
        let words = Dictionary::new(["restore"]);
        assert_eq!(words.nearest("rstr", 9), None);
        assert_eq!(words.nearest("restr", 9), Some(near("restore", 2)));
    }

    #[test]
    fn letters_count_as_letters_in_any_script() {
        let words = Dictionary::new(["репликация", "резервирование"]);
        assert_eq!(words.nearest("реплиакция", 1), Some(near("репликация", 1)));
    }

    #[test]
    fn an_empty_dictionary_has_nothing_near() {
        let words = Dictionary::new(Vec::<String>::new());
        assert!(words.is_empty());
        assert_eq!(words.nearest("anything", 2), None);
    }
}
