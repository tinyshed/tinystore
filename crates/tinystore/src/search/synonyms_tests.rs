use std::time::{Duration, Instant};

use super::*;
use crate::search::document;
use crate::search::fixture::{Fixture, Random, fixture};
use crate::search::index::Index;
use crate::search::settings::Field;

fn pairs(given: &[(&str, &str)]) -> Vec<(String, String)> {
    given.iter().map(|(short, phrase)| ((*short).to_owned(), (*phrase).to_owned())).collect()
}

fn texts(words: Vec<&Word>) -> Vec<&str> {
    words.into_iter().map(|word| word.text.as_str()).collect()
}

#[test]
fn a_phrase_by_its_stems_adjacent_and_in_order_holds_its_short_form_and_the_short_form_its_phrase() {
    let synonyms = Synonyms::new(&pairs(&[("кмзи", "криптографические методы защиты информации")])).unwrap();
    let declined = analyzer::words("курс криптографических методов защиты информации");
    assert_eq!(texts(synonyms.extra(&declined)), ["кмзи"]);
    let apart = analyzer::words("криптографические методы и защиты информации");
    assert!(synonyms.extra(&apart).is_empty(), "the phrase's words apart");
    let swapped = analyzer::words("методы криптографические защиты информации");
    assert!(synonyms.extra(&swapped).is_empty(), "out of order");
    let short = analyzer::words("экзамен по КМЗИ");
    assert_eq!(texts(synonyms.extra(&short)), ["криптографические", "методы", "защиты", "информации"]);
}

#[test]
fn a_short_form_of_several_words_an_empty_phrase_and_a_phrase_that_is_its_short_form_are_refused() {
    let refused = |short: &str, phrase: &str| Synonyms::new(&pairs(&[(short, phrase)])).unwrap_err().to_string();
    assert!(refused("machine learning", "ml").contains("one word"));
    assert!(refused("ml", " — ").contains("1 to 8 words"));
    assert!(refused("ml", "a b c d e f g h i").contains("1 to 8 words"));
    assert!(refused("Методы", "методов").contains("its own short form"));
    let itself = refused("learning", "machine learning");
    assert!(itself.contains("holds its own short form") && itself.contains("\"learning\""), "{itself}");
    assert!(refused(&"m".repeat(65), "machine").contains("past 64"));
    let many: Vec<_> = (0..1001).map(|at| (format!("s{at}"), "phrase".to_owned())).collect();
    assert!(Synonyms::new(&many).unwrap_err().to_string().contains("past 1000"));
}

#[test]
fn synonyms_keep_their_pairs_as_given_and_read_them_back() {
    let synonyms = Synonyms::new(&pairs(&[("ML", "Machine Learning"), ("кмзи", "защита информации")])).unwrap();
    let kept = synonyms.to_json().unwrap();
    assert_eq!(kept, json!([["ML", "Machine Learning"], ["кмзи", "защита информации"]]));
    assert_eq!(Synonyms::from_json(&kept), Some(synonyms));
    assert_eq!(Synonyms::default().to_json(), None);
    assert_eq!(Synonyms::from_json(&Value::Null), Some(Synonyms::default()));
}

#[test]
fn a_fields_length_counts_its_written_words_and_none_a_synonym_adds() {
    let field = Field { path: "name".to_owned(), weight: 1.0, number: 0 };
    let document = json!({ "name": "экзамен по КМЗИ" });
    let synonyms = Synonyms::new(&pairs(&[("кмзи", "криптографические методы защиты информации")])).unwrap();
    let with = document::analyze(&document, std::slice::from_ref(&field), &[], &synonyms).unwrap();
    let without = document::analyze(&document, &[field], &[], &Synonyms::default()).unwrap();
    assert_eq!(with.lengths, without.lengths, "three written words, whatever the synonyms");
    assert_eq!(with.terms.len(), without.terms.len() + 8, "four words of the phrase, each with its stem");
}

const KMZI: &str = "криптографические методы защиты информации";

fn catalog(fixture: &Fixture) -> Index<Value> {
    let open = fixture.store.index::<Value>("catalog").field("name", 1.0);
    open.synonyms([("кмзи", KMZI), ("ml", "machine learning")]).open().unwrap()
}

fn found(index: &Index<Value>, text: &str) -> Vec<String> {
    let mut ids: Vec<String> = index.search(text).run().unwrap().hits.into_iter().map(|hit| hit.id).collect();
    ids.sort();
    ids
}

// contract: SRCH-23
#[test]
fn a_short_form_finds_its_phrase_and_the_phrase_its_short_form_adjacent_and_in_order() {
    let fixture = fixture();
    let catalog = catalog(&fixture);
    let names = [
        "Криптографических методов защиты информации курс",
        "Экзамен по КМЗИ",
        "Методы криптографические защиты информации",
        "Machine learning, a first course",
        "A machine for learning",
    ];
    for (id, name) in (1..).zip(names) {
        catalog.set(id, &json!({ "name": name })).unwrap();
    }
    assert_eq!(found(&catalog, "кмзи"), ["1", "2"], "the phrase in another case counts, out of order it does not");
    assert_eq!(found(&catalog, "защиты информации"), ["1", "2", "3"], "the short form holds the phrase's words");
    assert_eq!(found(&catalog, "ML"), ["4"], "machine and learning apart are no phrase");
    let matched = catalog.search("кмзи").matches().snippet("name", 80).run().unwrap();
    let marked: Vec<(String, String)> = matched
        .hits
        .iter()
        .map(|hit| (hit.id.clone(), hit.value["name"].as_str().unwrap()[hit.matches[0].at.clone()].to_owned()))
        .collect();
    let phrase = "Криптографических методов защиты информации".to_owned();
    assert!(marked.contains(&("1".to_owned(), phrase)), "the phrase that stands there: {marked:?}");
    assert!(marked.contains(&("2".to_owned(), "КМЗИ".to_owned())), "{marked:?}");
    let by_phrase = catalog.search("информации").matches().run().unwrap();
    let exam = by_phrase.hits.iter().find(|hit| hit.id == "2").unwrap();
    assert_eq!(&"Экзамен по КМЗИ"[exam.matches[0].at.clone()], "КМЗИ", "the short form that stands there");
}

// contract: SRCH-23
#[test]
fn other_synonyms_read_the_index_again_and_none_given_leaves_it_none() {
    let fixture = fixture();
    let plain = fixture.store.index::<Value>("catalog").field("name", 1.0).open().unwrap();
    plain.set(1, &json!({ "name": "Machine learning" })).unwrap();
    assert!(found(&plain, "ml").is_empty());
    let catalog = catalog(&fixture);
    assert!(found(&catalog, "ml").is_empty(), "the index answers by what it had until its rebuild ends");
    catalog.ready().unwrap();
    assert_eq!(found(&catalog, "ml"), ["1"]);
    drop(fixture.store.index::<Value>("catalog").field("name", 1.0).open().unwrap());
    catalog.ready().unwrap();
    assert!(found(&catalog, "ml").is_empty(), "an open without synonyms leaves none");
}

// contract: SRCH-23
#[test]
fn a_synonym_of_several_words_for_its_short_form_is_refused_naming_the_index_and_the_synonym() {
    let fixture = fixture();
    let refused = fixture.store.index::<Value>("catalog").synonyms([("machine learning", "ml")]).open().unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid);
    let said = refused.to_string();
    assert!(said.contains("index catalog") && said.contains("\"machine learning\""), "{said}");
}

/// A word of a made-up language by its rank: syllables, two at least.
fn word(rank: u64) -> String {
    const SYLLABLES: [&str; 12] = ["ba", "ko", "ri", "mu", "te", "sa", "lo", "ve", "ni", "du", "pe", "zo"];
    let (mut rank, mut out) = (rank + 12, String::new());
    while rank > 0 {
        out.push_str(SYLLABLES[(rank % 12) as usize]);
        rank /= 12;
    }
    out
}

/// An index of 20,000 documents of 50 words, and how long setting them took.
fn timed_index(fixture: &Fixture, name: &str, synonyms: &[(String, String)]) -> (Index<String>, Duration) {
    let open = fixture.store.index::<String>(name).typo_tolerance(false);
    let index = open.synonyms(synonyms.iter().cloned()).open().unwrap();
    let mut random = Random::new(11);
    let started = Instant::now();
    for batch in 0..20 {
        let texts: Vec<String> =
            (0..1000).map(|_| (0..50).map(|_| word(random.skewed(5000))).collect::<Vec<_>>().join(" ")).collect();
        let ids: Vec<String> = (0..1000).map(|n| format!("{batch}-{n}")).collect();
        index.set_many(ids.iter().zip(&texts)).unwrap();
    }
    (index, started.elapsed())
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times[times.len() / 2]
}

/// Indexing 20,000 documents of 50 words with no synonym and with 200, each
/// a phrase of two of the 400 commonest words, and a search of each index.
#[test]
#[ignore = "a timing, run by hand"]
fn indexing_with_synonyms_takes() {
    let synonyms: Vec<(String, String)> =
        (0..200).map(|n| (format!("short{n}"), format!("{} {}", word(2 * n), word(2 * n + 1)))).collect();
    let (mut without, mut with) = (Vec::new(), Vec::new());
    let (mut plain_searches, mut rich_searches) = (Vec::new(), Vec::new());
    for pass in 0..3 {
        let fixture = fixture();
        let (plain, took) = timed_index(&fixture, &format!("plain{pass}"), &[]);
        without.push(took);
        let (rich, took) = timed_index(&fixture, &format!("rich{pass}"), &synonyms);
        with.push(took);
        crate::search::maintain(&fixture.store).unwrap();
        for (index, times) in [(&plain, &mut plain_searches), (&rich, &mut rich_searches)] {
            for n in 0..200 {
                let started = Instant::now();
                index.search(&format!("{} {}", word(n), word(n + 7))).run().unwrap();
                times.push(started.elapsed());
            }
        }
    }
    println!("20,000 documents of 50 words, median of 3 passes:");
    println!("  no synonym      {:?}", median(without));
    println!("  200 synonyms    {:?}", median(with));
    println!("a search of two words, median of 600:");
    println!("  no synonym      {:?}", median(plain_searches));
    println!("  200 synonyms    {:?}", median(rich_searches));
}

#[test]
fn the_same_pairs_in_another_order_are_the_same_synonyms_kept_the_same() {
    let one = Synonyms::new(&pairs(&[("ml", "machine learning"), ("ai", "artificial intelligence")])).unwrap();
    let other = Synonyms::new(&pairs(&[("ai", "artificial intelligence"), ("ml", "machine learning")])).unwrap();
    assert_eq!(one, other);
    assert_eq!(one.to_json(), other.to_json());
    let twice =
        Synonyms::new(&pairs(&[("ml", "machine learning"), ("ai", "intelligence"), ("ml", "machine learning")]));
    assert!(twice.unwrap_err().to_string().contains("given twice"));
}

// contract: SRCH-23
#[test]
fn an_open_of_the_same_pairs_in_another_order_reads_nothing_again() {
    let fixture = fixture();
    let open = |pairs: &[(&str, &str)]| {
        let open = fixture.store.index::<Value>("catalog").field("name", 1.0);
        open.synonyms(pairs.iter().copied()).open().unwrap()
    };
    let catalog = open(&[("ml", "machine learning"), ("ai", "artificial intelligence")]);
    catalog.set(1, &json!({ "name": "Machine learning" })).unwrap();
    let reopened = open(&[("ai", "artificial intelligence"), ("ml", "machine learning")]);
    assert_eq!(reopened.status().unwrap(), crate::search::Status::Ready { error: None }, "no rebuild");
    assert_eq!(found(&reopened, "ml"), ["1"]);
}

#[test]
fn kept_synonyms_are_read_without_an_opens_checks_and_once_a_process() {
    // a pair a later analyzer may read as two words, kept by an earlier one
    let kept = json!([["ai", "artificial intelligence"], ["machine learning", "ml"]]);
    let read = Synonyms::from_json(&kept).expect("the index still opens");
    assert_eq!(read.to_json(), Some(kept.clone()), "the pairs kept as they were");
    assert_eq!(texts(read.extra(&analyzer::words("an ai course"))), ["artificial", "intelligence"]);
    assert!(read.extra(&analyzer::words("ml")).is_empty(), "the pair that reads as none finds nothing");
    let again = Synonyms::from_json(&kept).unwrap();
    assert!(Arc::ptr_eq(&read.0, &again.0), "the same pairs read again are the synonyms read then");
}

// contract: SRCH-23
#[test]
fn a_synonym_found_by_a_prefix_marks_the_text_that_stands_there() {
    let fixture = fixture();
    let catalog = catalog(&fixture);
    catalog.set(1, &json!({ "name": "Курс криптографических методов защиты информации" })).unwrap();
    catalog.set(2, &json!({ "name": "Экзамен по КМЗИ" })).unwrap();
    let marked = |text: &str, id: &str| {
        let found = catalog.search(text).prefix().matches().snippet("name", 80).run().unwrap();
        let hit = found.hits.into_iter().find(|hit| hit.id == id).expect("found by its synonym");
        let name = hit.value["name"].as_str().unwrap().to_owned();
        let matched: Vec<String> = hit.matches.iter().map(|at| name[at.at.clone()].to_owned()).collect();
        (matched, hit.snippet.expect("a snippet").marks.len())
    };
    let (matched, marks) = marked("кмз", "1");
    assert_eq!(matched, ["криптографических методов защиты информации"], "the phrase, by its short form begun");
    assert_eq!(marks, 1);
    let (matched, marks) = marked("информац", "2");
    assert_eq!(matched, ["КМЗИ"], "the short form, by its phrase's word begun");
    assert_eq!(marks, 1);
}

// contract: SRCH-28
#[test]
fn an_index_rebuilt_for_new_synonyms_lists_the_open_the_rebuild_followed() {
    let dir = tempfile::tempdir().unwrap();
    let start = std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let clock = Arc::new(crate::TestClock::new(start));
    let options = crate::Options { clock: Some(clock.clone()), background: false, ..crate::Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let plain = store.index::<Value>("catalog").field("name", 1.0).open().unwrap();
    for n in 0..300 {
        plain.set(n, &json!({ "name": format!("Machine learning, part {n}") })).unwrap();
    }
    clock.set(start + Duration::from_secs(60)).unwrap();
    let synonyms = store.index::<Value>("catalog").field("name", 1.0).synonyms([("ml", "machine learning")]);
    let rich = synonyms.open().unwrap();
    assert!(matches!(rich.status().unwrap(), crate::search::Status::Rebuilding { .. }));
    clock.set(start + Duration::from_secs(120)).unwrap();
    while matches!(rich.status().unwrap(), crate::search::Status::Rebuilding { .. }) {
        crate::search::maintain(&store).unwrap();
    }
    let listed = crate::search::list(&store).unwrap();
    assert_eq!(listed.len(), 1, "the old rows dropped, the rebuilt ones under the index's name");
    let opened = Some(start + Duration::from_secs(60));
    assert_eq!(listed[0].opened, opened, "the open with synonyms, which the switch kept");
    assert_eq!(found(&rich, "ml").len(), 10, "the program's handle answers from the rebuilt rows");
    drop(store);
}
