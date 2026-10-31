use super::*;
use crate::search::fixture::fixture;
use crate::search::read::checked;
use crate::search::{Found, Index};

#[test]
fn each_layout_has_a_character_a_key_and_each_once() {
    for layout in [Layout::En, Layout::Ru] {
        let keys = layout.keys();
        assert!(keys.iter().all(|level| level.chars().count() == 47), "{layout:?}");
        let every: Vec<char> = keys.iter().flat_map(|level| level.chars()).collect();
        let mut once = every.clone();
        once.sort_unstable();
        once.dedup();
        assert_eq!(once.len(), every.len(), "{layout:?} types a character on two keys");
    }
}

#[test]
fn a_piece_reads_as_the_same_keys_type_in_the_other_layout() {
    let both = [Layout::Ru, Layout::En];
    assert_eq!(readings("rhbgnj", &both), ["крипто"]);
    assert_eq!(readings("by;", &both), ["инж"]);
    assert_eq!(readings("знерщт", &both), ["python"]);
    assert_eq!(readings("[kt,", &both), ["хлеб"]);
    // shifted keys give capitals, which normalizing makes small
    assert_eq!(readings("<fhcer", &both), ["Барсук"]);
    assert_eq!(readings("'nj", &both), ["это"]);
    assert_eq!(readings("эюж", &both), ["'.;"]);
    assert_eq!(readings("v2", &both), ["м2"], "a digit is the same key in both");
}

#[test]
fn a_piece_no_layout_types_whole_or_without_a_letter_reads_as_nothing_else() {
    let both = [Layout::Ru, Layout::En];
    assert!(readings("pythonдля", &both).is_empty(), "two scripts");
    assert!(readings("café", &both).is_empty());
    assert!(readings("1.5", &both).is_empty(), "no letter");
    assert!(readings(";", &both).is_empty());
    assert!(readings("rhbgnj", &[]).is_empty());
}

#[test]
fn layouts_are_known_names_each_once_and_two_at_least() {
    assert_eq!(layouts(&["ru", "en"]).unwrap(), [Layout::Ru, Layout::En]);
    assert!(layouts::<&str>(&[]).unwrap().is_empty());
    for refused in [&["ru", "de"][..], &["ru", "ru"], &["ru"], &["RU", "en"]] {
        assert_eq!(layouts(refused).unwrap_err().kind(), crate::ErrorKind::Invalid, "{refused:?}");
    }
}

#[test]
fn pieces_are_runs_between_spaces() {
    let text = " by;  uhfa\tx ";
    let found: Vec<&str> = pieces(text).into_iter().map(|piece| &text[piece]).collect();
    assert_eq!(found, ["by;", "uhfa", "x"]);
    assert!(pieces("").is_empty());
}

/// The catalogue of issue 44, whose students switch layouts all day.
const COURSES: [&str; 5] = [
    "Криптографические методы защиты информации",
    "Линейная алгебра",
    "Python для анализа данных",
    "Инженерная графика",
    "Information security",
];

fn courses(index: &Index<String>) {
    for (id, name) in (1..).zip(COURSES) {
        index.set(id, &name.to_owned()).unwrap();
    }
}

fn read(found: Found<String>) -> (Vec<String>, Option<String>) {
    (found.hits.into_iter().map(|hit| hit.id).collect(), found.corrected)
}

fn hits(ids: &[&str], corrected: Option<&str>) -> (Vec<String>, Option<String>) {
    (ids.iter().map(|id| (*id).to_owned()).collect(), corrected.map(str::to_owned))
}

// contract: SRCH-12
#[test]
fn a_word_typed_with_the_other_layout_on_is_read_as_its_keys_type_there_and_corrected_says_so() {
    let fixture = fixture();
    let index = fixture.store.index::<String>("courses").layouts(["ru", "en"]).open().unwrap();
    courses(&index);
    let typed = |text: &str| read(checked(|| index.search(text).prefix().run()).unwrap());
    assert_eq!(typed("rhbgnj"), hits(&["1"], Some("крипто")));
    assert_eq!(typed("знерщт"), hits(&["3"], Some("python")));
    // `лин` and `инж` are no word of the index, and only the last word is read as a prefix
    assert_eq!(typed("kby fku"), hits(&["2"], Some("kby алг")));
    assert_eq!(typed("by; uhfa"), hits(&["4"], Some("by; граф")));
    // the course names mix both, and so does a search box
    assert_eq!(typed("Python lkz"), hits(&["3"], Some("python для")));
    // a swap and a typo in one text
    assert_eq!(typed("informatoin rhbgnj"), hits(&["5", "1"], Some("information крипто")));
    let whole = |text: &str| read(checked(|| index.search(text).run()).unwrap());
    assert_eq!(whole("rhbgnj"), hits(&[], None), "without prefix `крипто` is a whole word, which none holds");
}

// contract: SRCH-12
#[test]
fn a_word_that_finds_something_as_typed_is_never_read_in_another_layout() {
    let fixture = fixture();
    let index = fixture.store.index::<String>("greetings").layouts(["en", "ru"]).open().unwrap();
    for (id, text) in [(1, "ghbdtn"), (2, "привет"), (3, "ghost")] {
        index.set(id, &text.to_owned()).unwrap();
    }
    let found = |text: &str, prefix: bool| {
        let call = index.search(text);
        read(checked(|| if prefix { call.prefix() } else { call }.run()).unwrap())
    };
    assert_eq!(found("ghbdtn", false), hits(&["1"], None), "held as typed, though `привет` is held too");
    assert_eq!(found("привет", false), hits(&["2"], None));
    assert_eq!(found("ghb", true), hits(&["1"], None), "a prefix that begins a word as typed");
    assert_eq!(found("пр", true), hits(&["2"], None));
    assert_eq!(found("ghbdtn ghost", false), hits(&["1", "3"], None), "each piece as typed");
    // `прщые` finds nothing, and `ghost` does: the piece read is the one that finds nothing
    assert_eq!(found("привет прщые", false), hits(&["2", "3"], Some("привет ghost")));
}

// contract: SRCH-12
#[test]
fn layouts_read_without_typo_tolerance_and_an_index_without_them_reads_as_typed() {
    let fixture = fixture();
    let strict = fixture.store.index::<String>("strict").layouts(["ru", "en"]).typo_tolerance(false).open().unwrap();
    let plain = fixture.store.index::<String>("plain").open().unwrap();
    courses(&strict);
    courses(&plain);
    let typed = |index: &Index<String>, text: &str| read(checked(|| index.search(text).prefix().run()).unwrap());
    assert_eq!(typed(&strict, "rhbgnj"), hits(&["1"], Some("крипто")));
    assert_eq!(typed(&strict, "informatoin rhbgnj"), hits(&["1"], Some("informatoin крипто")));
    assert_eq!(typed(&plain, "rhbgnj"), hits(&[], None));
    let reopened = fixture.store.index::<String>("plain").layouts(["ru", "en"]).open().unwrap();
    assert_eq!(typed(&reopened, "rhbgnj"), hits(&["1"], Some("крипто")), "layouts apply at once");
    let refused = fixture.store.index::<String>("plain").layouts(["ru", "de"]).open().unwrap_err();
    assert_eq!(refused.to_string(), "index plain: a layout \"de\": the layouts known are ru and en: invalid");
}
