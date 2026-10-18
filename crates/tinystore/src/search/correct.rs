//! A misspelt word read as the index's nearest: the index keeps one
//! dictionary of its words for each script they are written in, Latin,
//! Cyrillic and every other, since a word is near only words of its own
//! script, and one dictionary of every script made each search walk all of
//! them.
//!
//! A dictionary is built from one snapshot of the index and knows how far
//! its words had changed then. A search uses the one kept even when the
//! index has changed since, and reads a word as a candidate only when the
//! search's own snapshot holds it, so a word gone since is never read; a
//! word new since is missed until maintenance builds the dictionary again.

use std::time::Instant;

use rusqlite::Connection;

use super::score::Part;
use crate::Result;
use crate::sqlite::sql_error;
use crate::typos::Dictionary;

const READ_WORDS: &str = "select part, term from _tinystore_search_keys where index_id = ?1 and documents > 0";
/// Candidates a word takes at most, of its nearest.
const MOST: usize = 8;

pub(crate) struct Corrector {
    /// How far the index's words had changed when it was built.
    pub(crate) version: i64,
    pub(crate) built: Instant,
    scripts: [Dictionary; 3],
}

impl std::fmt::Debug for Corrector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Corrector").field("version", &self.version).finish_non_exhaustive()
    }
}

impl Corrector {
    /// The dictionaries of the words of `parts` an index holds now.
    pub(crate) fn build(connection: &Connection, index: i64, version: i64, parts: &[Part]) -> Result<Corrector> {
        let failed = |error| sql_error("read an index's words", error);
        let mut select = connection.prepare_cached(READ_WORDS).map_err(failed)?;
        let mut rows = select.query([index]).map_err(failed)?;
        let mut words: [Vec<String>; 3] = Default::default();
        while let Some(row) = rows.next().map_err(failed)? {
            let (part, term): (u32, String) = (row.get(0).map_err(failed)?, row.get(1).map_err(failed)?);
            if parts.contains(&Part(part)) {
                words[script(&term)].push(term);
            }
        }
        Ok(Corrector { version, built: Instant::now(), scripts: words.map(Dictionary::new) })
    }

    /// The index's words nearest `word` that a search may read it as: none
    /// under five letters, one edit away up to eight, two from nine, as
    /// Meilisearch reads them.
    pub(crate) fn candidates(&self, word: &str) -> Vec<String> {
        let edits = match word.chars().count() {
            0..5 => return Vec::new(),
            5..9 => 1,
            _ => 2,
        };
        let near = self.scripts[script(word)].within(word, edits);
        let nearest = near.first().map_or(0, |near| near.edits);
        near.into_iter().take_while(|near| near.edits == nearest).take(MOST).map(|near| near.word).collect()
    }
}

/// A word's script by its first letter: Latin, Cyrillic, or any other.
fn script(word: &str) -> usize {
    match word.chars().next().map_or(0, |c| c as u32) {
        0x0000..=0x024F | 0x1E00..=0x1EFF => 0,
        0x0400..=0x052F => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corrector(words: &[&str]) -> Corrector {
        let mut scripts: [Vec<&str>; 3] = Default::default();
        words.iter().for_each(|word| scripts[script(word)].push(word));
        Corrector { version: 0, built: Instant::now(), scripts: scripts.map(Dictionary::new) }
    }

    #[test]
    fn a_word_is_read_with_one_edit_from_five_letters_and_two_from_nine() {
        let words = corrector(&["hello", "help", "replication", "репликация"]);
        assert_eq!(words.candidates("hell"), Vec::<String>::new());
        assert_eq!(words.candidates("helo"), Vec::<String>::new());
        assert_eq!(words.candidates("hellp"), ["hello", "help"]);
        assert_eq!(words.candidates("replicaiton"), ["replication"]);
        assert_eq!(words.candidates("rplicatin"), ["replication"]);
        assert_eq!(words.candidates("реплиакция"), ["репликация"]);
    }

    #[test]
    fn only_the_nearest_are_candidates() {
        let words = corrector(&["kitten", "kittens", "mitten"]);
        assert_eq!(words.candidates("kitten"), ["kittens", "mitten"]);
    }
}
