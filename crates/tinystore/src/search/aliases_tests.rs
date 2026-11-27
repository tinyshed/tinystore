use serde_json::{Value, json};

use super::*;
use crate::search::document::{self, Besides};
use crate::search::fixture::{Fixture, fixture};
use crate::search::index::{Doc, Index};
use crate::search::synonyms::Synonyms;
use crate::search::{Status, maintain};
use crate::sqlite::{Config, File};

const KMZI: &str = "Криптографические методы защиты информации";

fn pairs(given: &[(&str, &str)]) -> Vec<(String, String)> {
    given.iter().map(|(alias, field)| ((*alias).to_owned(), (*field).to_owned())).collect()
}

fn field(path: &str, number: u32) -> Field {
    Field { path: path.to_owned(), weight: 1.0, number }
}

/// Courses by their name, weighing 4, and their summary.
fn courses(fixture: &Fixture) -> Index<Value> {
    fixture.store.index::<Value>("courses").field("name", 4.0).field("summary", 1.0).open().unwrap()
}

fn found(index: &Index<Value>, text: &str) -> Vec<String> {
    index.search(text).run().unwrap().hits.into_iter().map(|hit| hit.id).collect()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// The rows of aliases search.db holds, of every index.
fn rows(fixture: &Fixture) -> i64 {
    let connection = rusqlite::Connection::open(fixture.dir.path().join("search.db")).unwrap();
    connection.query_row("select count(*) from _tinystore_search_aliases", [], |row| row.get(0)).unwrap()
}

#[test]
fn an_alias_is_one_word_for_a_field_the_index_reads_and_none_twice() {
    let fields = [field("name", 0), field("summary", 1)];
    let refused = |given: &[(&str, &str)]| Aliases::new(pairs(given), &fields).unwrap_err().to_string();
    let unread = refused(&[("кмзи", "title")]);
    assert!(unread.contains("alias \"кмзи\" for title") && unread.contains("[\"name\", \"summary\"]"), "{unread}");
    assert!(refused(&[("к м", "name")]).contains("an alias is one word"));
    assert!(refused(&[(" — ", "name")]).contains("an alias is one word"));
    assert!(refused(&[(&*"к".repeat(33), "name")]).contains("past 64"));
    assert!(refused(&[("кмзи", "name"), ("кмзи", "summary")]).contains("given twice"));
    let many: Vec<(String, String)> = (0..65).map(|at| (format!("a{at}"), "name".to_owned())).collect();
    assert!(Aliases::new(many, &fields).unwrap_err().to_string().contains("65 aliases, past 64"));
    let aliases = Aliases::new(pairs(&[("тп", "summary"), ("КМЗИ", "name")]), &fields).unwrap();
    let words: Vec<&str> = aliases.of_field("name").map(|word| word.text.as_str()).collect();
    assert_eq!(words, ["кмзи"], "the word as the index reads one");
}

#[test]
fn kept_aliases_of_a_field_the_index_no_longer_reads_stay_and_find_nothing() {
    let kept = Aliases::kept(pairs(&[("кмзи", "name"), ("тп", "title"), ("two words", "name")]), &[field("name", 0)]);
    let words: Vec<&str> = kept.of_field("name").map(|word| word.text.as_str()).collect();
    assert_eq!(words, ["кмзи"]);
    assert_eq!(kept.given.len(), 3, "every pair as the set gave it");
}

#[test]
fn an_alias_counts_in_no_fields_length_and_stands_for_a_field_with_text_alone() {
    let fields = [field("name", 0), field("summary", 1)];
    let document = json!({ "name": KMZI });
    let aliases = Aliases::new(pairs(&[("кмзи", "name"), ("тп", "summary")]), &fields).unwrap();
    let besides = Besides { synonyms: &Synonyms::default(), aliases: &aliases };
    let with = document::analyze(&document, &fields, &[], besides).unwrap();
    let without = document::analyze_plain(&document, &fields, &[]).unwrap();
    assert_eq!(with.lengths, without.lengths, "four written words, whatever the aliases");
    assert_eq!(with.terms.len(), without.terms.len() + 1, "кмзи in the name; the summary holds no text for тп");
}

// contract: SRCH-30
#[test]
fn an_alias_finds_its_own_document_alone_and_the_same_alias_means_each_documents_own_text() {
    let fixture = fixture();
    let courses = courses(&fixture);
    let set = |id: &str, name: &str, aliases: &[(&str, &str)]| {
        courses.id(id).aliases(aliases.iter().copied()).set(&json!({ "name": name })).unwrap();
    };
    set("1", "Теория принятия решений", &[("тп", "name")]);
    set("2", "Технологии программирования", &[("тп", "name")]);
    set("3", KMZI, &[("кмзи", "name")]);
    set("4", "Теория вероятностей", &[]);
    assert_eq!(sorted(found(&courses, "тп")), ["1", "2"], "each by its own name, the fourth by none");
    assert_eq!(found(&courses, "КМЗИ"), ["3"], "in another case");
    let found = courses.search("тп").matches().snippet("name", 80).run().unwrap();
    for hit in &found.hits {
        let name = hit.value["name"].as_str().unwrap();
        assert_eq!(hit.matches.len(), 1, "{hit:?}");
        let matched = &hit.matches[0];
        assert_eq!((matched.field.as_str(), &name[matched.at.clone()], matched.word.as_str()), ("name", name, "тп"));
        let snippet = hit.snippet.as_ref().expect("a snippet of the name");
        let (text, marks) = (snippet.text.as_str(), snippet.marks.len());
        assert_eq!((text, marks, snippet.marks[0].clone()), (name, 1, 0..name.len()), "the name marked whole");
    }
}

// contract: SRCH-30
#[test]
fn a_document_found_by_its_alias_ranks_as_a_match_of_the_field_it_stands_for() {
    let fixture = fixture();
    let courses = courses(&fixture);
    let by_summary = json!({ "name": "Курс по выбору", "summary": "Теория принятия решений" });
    courses.id("summary").aliases([("тпр", "summary")]).set(&by_summary).unwrap();
    let by_name = json!({ "name": "Теория принятия решений", "summary": "Курс по выбору" });
    courses.id("name").aliases([("тпр", "name")]).set(&by_name).unwrap();
    assert_eq!(found(&courses, "тпр"), ["name", "summary"], "a name weighs four times a summary");
    let hits = courses.search("тпр").matches().run().unwrap().hits;
    let fields: Vec<&str> = hits.iter().map(|hit| hit.matches[0].field.as_str()).collect();
    assert_eq!(fields, ["name", "summary"], "each marked on the field its alias stands for");
}

// contract: SRCH-30
#[test]
fn an_alias_is_found_as_the_index_reads_a_word_typed_in_another_layout_or_begun() {
    let fixture = fixture();
    let courses = fixture.store.index::<Value>("courses").field("name", 1.0).layouts(["ru", "en"]).open().unwrap();
    courses.id("3").aliases([("кмзи", "name")]).set(&json!({ "name": KMZI })).unwrap();
    let typed = courses.search("rvpb").matches().run().unwrap();
    assert_eq!(typed.hits.len(), 1, "кмзи typed in the English layout");
    assert_eq!(typed.hits[0].matches[0].at, 0..KMZI.len());
    let begun = courses.search("кмз").prefix().matches().run().unwrap();
    assert_eq!(begun.hits[0].matches[0].at, 0..KMZI.len(), "the name whole, however few letters were typed");
}

// contract: SRCH-31
#[test]
fn a_set_again_changes_a_documents_aliases_alone_and_reads_nothing_again() {
    let fixture = fixture();
    let courses = courses(&fixture);
    courses.id("1").aliases([("тп", "name")]).set(&json!({ "name": "Теория принятия решений" })).unwrap();
    courses.id("2").aliases([("тп", "name")]).set(&json!({ "name": "Технологии программирования" })).unwrap();
    courses.id("1").aliases([("тпр", "name")]).set(&json!({ "name": "Теория принятия решений" })).unwrap();
    assert_eq!(courses.status().unwrap(), Status::Ready { error: None }, "no rebuild");
    assert_eq!(found(&courses, "тп"), ["2"], "the other document keeps its own");
    assert_eq!(found(&courses, "тпр"), ["1"]);
    courses.set("1", &json!({ "name": "Теория принятия решений" })).unwrap();
    assert!(found(&courses, "тпр").is_empty(), "a set without aliases leaves the document none");
    assert_eq!(rows(&fixture), 1, "the second document's alone");
    assert!(courses.delete("2").unwrap());
    assert!(found(&courses, "тп").is_empty());
    assert_eq!(rows(&fixture), 0, "a delete takes a document's aliases");
}

// contract: SRCH-31
#[test]
fn aliases_survive_a_merge_a_purge_and_a_rebuild() {
    let fixture = fixture();
    let courses = courses(&fixture);
    // an alias a course's own, of letters alone: 7 → "xh"
    let alias =
        |at: u32| format!("x{}", at.to_string().chars().map(|digit| (digit as u8 + 49) as char).collect::<String>());
    let documents: Vec<(String, Value)> =
        (0..300).map(|at| (format!("c{at}"), json!({ "name": format!("Курс номер {at}") }))).collect();
    let set =
        (0..).zip(&documents).map(|(at, (id, value))| Doc::new(id.as_str(), value).aliases([(alias(at), "name")]));
    courses.set_many(set).unwrap();
    courses.id("kmzi").aliases([("кмзи", "name")]).set(&json!({ "name": KMZI })).unwrap();
    for at in (0..300).step_by(2) {
        courses.delete(format!("c{at}")).unwrap();
    }
    maintain(&fixture.store).unwrap();
    assert_eq!(found(&courses, "кмзи"), ["kmzi"], "after a merge and a purge");
    assert_eq!(found(&courses, &alias(7)), ["c7"]);
    assert!(found(&courses, &alias(8)).is_empty(), "a document deleted takes its alias");
    let reindexed = |fields: &[&str]| {
        let open = fields.iter().fold(fixture.store.index::<Value>("courses"), |open, path| open.field(path, 1.0));
        let index = open.reindex().unwrap();
        index.ready().unwrap();
        index
    };
    let rebuilt = reindexed(&["name", "summary"]);
    let hit = rebuilt.search("кмзи").matches().run().unwrap().hits.remove(0);
    assert_eq!((hit.id.as_str(), hit.matches[0].at.clone()), ("kmzi", 0..KMZI.len()), "read again with its aliases");
    assert_eq!(found(&rebuilt, &alias(7)), ["c7"]);
    let without_name = reindexed(&["summary"]);
    assert!(found(&without_name, "кмзи").is_empty(), "an alias of a field the index reads no more finds nothing");
    let with_name = reindexed(&["name"]);
    assert_eq!(found(&with_name, "кмзи"), ["kmzi"], "and finds again once it reads the field again");
}

#[test]
fn a_refused_alias_names_the_index_the_document_and_the_alias() {
    let fixture = fixture();
    let courses = courses(&fixture);
    let refused = courses.id("1").aliases([("кмзи", "title")]).set(&json!({ "name": KMZI })).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid);
    let said = refused.to_string();
    assert!(said.contains("index courses") && said.contains("\"1\"") && said.contains("alias \"кмзи\""), "{said}");
    let value = json!({ "name": KMZI });
    let many = courses.set_many([Doc::new("1", &value), Doc::new("2", &value).aliases([("к м", "name")])]);
    let said = many.unwrap_err().to_string();
    assert!(said.contains("document 1, id \"2\""), "{said}");
    assert!(found(&courses, "криптографические").is_empty(), "none of them set");
}

// contract: STORE-11
#[test]
fn a_file_v0_13_wrote_opens_and_reads_as_before_and_v0_13_refuses_one_this_release_wrote() {
    let fixture = fixture();
    courses(&fixture).set("1", &json!({ "name": KMZI })).unwrap();
    fixture.store.close().unwrap();
    // v0.13.0 knows four steps, and wrote no table of aliases
    let path = fixture.dir.path().join("search.db");
    let file = File::open(&path, Config::default()).unwrap();
    let refused = file.migrate("search", &crate::search::engine::MIGRATIONS[..4]).unwrap_err();
    assert!(refused.to_string().contains("a newer program migrated it"), "{refused}");
    file.close().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "drop table _tinystore_search_aliases;
            delete from _tinystore_migrations where history = 'search' and version = 5;",
        )
        .unwrap();
    drop(connection);
    let store = crate::search::fixture::open(fixture.dir.path());
    let courses = store.index::<Value>("courses").field("name", 4.0).field("summary", 1.0).open().unwrap();
    assert_eq!(found(&courses, "криптографические"), ["1"], "read as before");
    courses.id("1").aliases([("кмзи", "name")]).set(&json!({ "name": KMZI })).unwrap();
    assert_eq!(found(&courses, "кмзи"), ["1"]);
    store.close().unwrap();
}
