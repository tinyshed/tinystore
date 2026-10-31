use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::*;
use crate::search::Found;
use crate::search::fixture::{Random, fixture};
use crate::search::read::checked;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Course {
    name: String,
    short: String,
}

/// Each match of each hit as its id, its field, the text it slices and the
/// query's word.
fn shown(found: &Found<Course>) -> Vec<(&str, &str, &str, &str)> {
    let mut shown = Vec::new();
    for hit in &found.hits {
        for found in &hit.matches {
            let text = if found.field == "name" { &hit.value.name } else { &hit.value.short };
            shown.push((hit.id.as_str(), found.field.as_str(), &text[found.at.clone()], found.word.as_str()));
        }
    }
    shown
}

/// The text a match slices of `document`, by its field's path and its place
/// in a list.
fn sliced<'v>(document: &'v Value, found: &Match) -> &'v str {
    let mut at = document;
    for name in found.field.split('.').filter(|name| !name.is_empty()) {
        at = &at[name];
    }
    if let Some(item) = found.item {
        at = &at[item];
    }
    &at.as_str().unwrap()[found.at.clone()]
}

// contract: SRCH-14
#[test]
fn a_hit_says_where_it_matched_by_a_stem_by_a_prefix_and_after_a_typo() {
    // the courses issue 45 searched
    let fixture = fixture();
    let courses = fixture.store.index::<Course>("courses").field("name", 2.0).field("short", 1.0).open().unwrap();
    let course = |name: &str, short: &str| Course { name: name.to_owned(), short: short.to_owned() };
    let (algebra, crypto) = (
        course("Линейная алгебра и аналитическая геометрия", "ЛААГ"),
        course("Криптографические методы защиты информации", "КМЗИ"),
    );
    courses.set_many([(1, &algebra), (2, &crypto)]).unwrap();
    let found = |text: &str| checked(|| courses.search(text).prefix().matches().run()).unwrap();

    let by_stem = found("алгебре");
    assert_eq!(shown(&by_stem), [("1", "name", "алгебра", "алгебре")]);
    let at = by_stem.hits[0].matches[0].at.clone();
    assert_eq!((at.clone(), algebra.name[..at.start].chars().count()), (17..31, 9), "bytes, 9..16 in characters");
    assert_eq!(shown(&found("крипто")), [("2", "name", "Крипто", "крипто")], "the letters typed");
    let misspelt = found("Криптографичесике методы");
    assert_eq!(misspelt.corrected.as_deref(), Some("криптографические методы"));
    let words = [("2", "name", "Криптографические", "Криптографичесике"), ("2", "name", "методы", "методы")];
    assert_eq!(shown(&misspelt), words, "the word as the search's text wrote it, before its fix");
    let plain = checked(|| courses.search("алгебре").run()).unwrap();
    assert!(plain.hits[0].matches.is_empty(), "nothing unless asked");
}

// contract: SRCH-14
#[test]
fn a_hit_says_where_each_word_read_as_a_prefix_matched_and_where_a_word_typed_with_the_other_layout_did() {
    let fixture = fixture();
    let open = fixture.store.index::<Course>("courses").field("name", 2.0).field("short", 1.0).layouts(["ru", "en"]);
    let courses = open.open().unwrap();
    let algebra = Course { name: "Линейная алгебра".to_owned(), short: "ЛА".to_owned() };
    courses.set(1, &algebra).unwrap();

    let shortened = checked(|| courses.search("лин алг").prefix_every().matches().run()).unwrap();
    assert_eq!(shown(&shortened), [("1", "name", "Лин", "лин"), ("1", "name", "алг", "алг")], "each word's letters");
    let swapped = checked(|| courses.search("kby fku").prefix_every().matches().run()).unwrap();
    assert_eq!(swapped.corrected.as_deref(), Some("лин алг"));
    assert_eq!(shown(&swapped), [("1", "name", "Лин", "kby"), ("1", "name", "алг", "fku")], "as the text wrote them");
}

// contract: SRCH-14
#[test]
fn a_list_says_which_string_matched_and_a_word_matched_twice_gives_two_ranges() {
    let fixture = fixture();
    let books = fixture.store.index::<Value>("books").field("tags", 1.0).field("author.name", 1.0).open().unwrap();
    let book = json!({
        "tags": ["Алгебра", null, "линейная алгебра и алгебры"],
        "author": { "name": "Анна Алгебраистова" },
    });
    books.set("b-1", &book).unwrap();
    let found = checked(|| books.search("алгебра").matches().run()).unwrap();
    let matches = &found.hits[0].matches;
    let places: Vec<(&str, Option<usize>, &str)> =
        matches.iter().map(|found| (found.field.as_str(), found.item, sliced(&book, found))).collect();
    assert_eq!(places, [("tags", Some(0), "Алгебра"), ("tags", Some(2), "алгебра"), ("tags", Some(2), "алгебры")]);
    let by_author = checked(|| books.search("анна").matches().run()).unwrap();
    let found = &by_author.hits[0].matches[0];
    assert_eq!((found.field.as_str(), found.item, sliced(&book, found)), ("author.name", None, "Анна"));
}

// contract: SRCH-14
#[test]
fn a_document_that_is_its_own_text_matches_in_the_field_of_no_name() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").open().unwrap();
    let note = "Хлеб на закваске. Хлеба много".to_owned();
    notes.set(1, &note).unwrap();
    let found = checked(|| notes.search("хлеб").matches().run()).unwrap();
    let places: Vec<(&str, Option<usize>, &str)> =
        found.hits[0].matches.iter().map(|found| (found.field.as_str(), found.item, &note[found.at.clone()])).collect();
    assert_eq!(places, [("", None, "Хлеб"), ("", None, "Хлеба")]);
}

// contract: SRCH-14
#[test]
fn overlapping_matches_join_and_a_word_typed_whole_and_begun_matches_both() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").open().unwrap();
    let (tower, crypto) = ("東京タワーの夜景".to_owned(), "Крипто и криптография".to_owned());
    notes.set_many([(1, &tower), (2, &crypto)]).unwrap();
    let found = |text: &str, id: &str| {
        let found = checked(|| notes.search(text).prefix().matches().run()).unwrap();
        let hit = found.hits.into_iter().find(|hit| hit.id == id).unwrap();
        let slice = |found: &Match| (hit.value[found.at.clone()].to_owned(), found.word.clone());
        hit.matches.iter().map(slice).collect::<Vec<_>>()
    };
    // 東京 京タ タワ ワー overlap pair by pair, and the first names the range they join into
    assert_eq!(found("東京タワー", "1"), [("東京タワー".to_owned(), "東京".to_owned())]);
    let begun = [("Крипто".to_owned(), "крипто".to_owned()), ("крипто".to_owned(), "крипто".to_owned())];
    assert_eq!(found("крипто", "2"), begun);
}

#[test]
fn a_search_by_its_vector_alone_matched_no_word() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").vectors(2).open().unwrap();
    notes.id(1).vector(&[1.0, 0.0]).set(&"bread".to_owned()).unwrap();
    let by_vector = notes.search_vector(&[1.0, 0.0]).matches().run().unwrap();
    assert!(by_vector.hits[0].matches.is_empty());
    let by_both = notes.search("bread").vector(&[1.0, 0.0]).matches().run().unwrap();
    assert_eq!(by_both.hits[0].matches.len(), 1);
}

/// `word` as a person might store it: in capitals, full width, its accents
/// composed or apart, `ё` for `е`, a stress mark, a ligature; each folds
/// back to `word`, mostly to fewer or more bytes.
fn decorated(random: &mut Random, word: &str) -> String {
    let mut out = String::new();
    let mut letters = word.chars().peekable();
    while let Some(letter) = letters.next() {
        if letter == 'f' && letters.peek() == Some(&'i') && random.below(2) == 0 {
            letters.next();
            out.push('ﬁ');
            continue;
        }
        let vowel = "aeiouаеиоуыэюя".contains(letter);
        let letter = if letter == 'е' && random.below(2) == 0 { 'ё' } else { letter };
        let letter = if random.below(2) == 0 { letter.to_uppercase().next().unwrap() } else { letter };
        match random.below(4) {
            // the full-width forms of ASCII lie 0xFEE0 past it: A → Ａ, a → ａ
            0 if letter.is_ascii_alphabetic() => out.push(char::from_u32(u32::from(letter) + 0xFEE0).unwrap()),
            1 if vowel => out.extend(format!("{letter}\u{301}").nfc()),
            2 if vowel => out.extend([letter, '\u{301}']),
            _ => out.push(letter),
        }
    }
    out
}

// contract: SRCH-14
#[test]
fn a_match_slices_the_word_as_stored_however_folding_changed_its_length() {
    let fixture = fixture();
    let notes = fixture.store.index::<String>("notes").typo_tolerance(false).open().unwrap();
    let words = ["cafe", "finance", "tokyo", "елка", "кризис", "карибский"];
    let gaps = [" ", ", ", " — ", "\n", "; "];
    let mut random = Random::new(45);
    let mut held: Vec<(String, Vec<&str>)> = Vec::new();
    for id in 0..60 {
        let chosen: Vec<&str> = (0..2 + random.below(6)).map(|_| words[random.below(6) as usize]).collect();
        let mut text = String::new();
        for (at, word) in chosen.iter().enumerate() {
            if at > 0 {
                text.push_str(gaps[random.below(gaps.len() as u64) as usize]);
            }
            text.push_str(&decorated(&mut random, word));
        }
        notes.set(id, &text).unwrap();
        held.push((text, chosen));
    }
    for word in words {
        let found = checked(|| notes.search(word).matches().limit(1000).run()).unwrap();
        let holding = held.iter().filter(|(_, chosen)| chosen.contains(&word)).count();
        assert_eq!(found.hits.len(), holding, "{word}");
        for hit in &found.hits {
            let (text, chosen) = &held[hit.id.parse::<usize>().unwrap()];
            let times = chosen.iter().filter(|each| **each == word).count();
            assert_eq!(hit.matches.len(), times, "{word} in {text:?}");
            for found in &hit.matches {
                let sliced = &text[found.at.clone()];
                assert_eq!(analyzer::normalize(sliced), word, "{word} in {text:?} sliced {sliced:?}");
            }
        }
    }
}

#[test]
fn ranges_count_in_bytes_utf16_units_or_code_points() {
    // an emoji outside the BMP is 4 bytes, 2 units of UTF-16 and 1 code point
    let text = "🦀 Ёж";
    let at = || vec![5..7, 7..9];
    assert_eq!(counted(text, at(), Unit::Bytes), [5..7, 7..9]);
    assert_eq!(counted(text, at(), Unit::Utf16), [3..4, 4..5]);
    assert_eq!(counted(text, at(), Unit::Chars), [2..3, 3..4]);
}

#[test]
fn overlapping_ranges_join_named_by_the_first_and_apart_ones_stay() {
    let found = vec![(6..12, 2), (0..6, 0), (3..9, 1), (14..16, 3), (14..20, 4), (20..22, 5)];
    assert_eq!(merged(found), [(0..12, 0), (14..20, 4), (20..22, 5)]);
}

#[test]
fn a_prefix_covers_the_letters_typed_and_what_lies_between_them() {
    let normal = "o'reilly";
    let word = analyzer::words_of(normal).remove(0);
    assert_eq!(&normal[typed(normal, &word, 3)], "o're");
    assert_eq!(&normal[typed(normal, &word, 7)], "o'reilly");
}
