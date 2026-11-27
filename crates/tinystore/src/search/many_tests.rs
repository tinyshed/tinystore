//! One question of several indexes: the store's indexes of documents read in
//! one snapshot, a failed index its group's error, `more` exact, and `best`
//! by reciprocal rank fusion, never by score.

use serde_json::{Value, json};

use super::*;
use crate::ErrorKind;
use crate::search::eq;
use crate::search::fixture::{Fixture, fixture};

impl Fixture {
    /// An index of `name` holding a document `{name}-{n}` titled `title` for each title.
    fn holding(&self, name: &str, titles: &[&str]) -> Index<Value> {
        let index = self.store.index::<Value>(name).field("title", 1.0).filters(["space"]).open().unwrap();
        for (n, title) in titles.iter().enumerate() {
            index.set(format!("{name}-{n}"), &json!({ "title": title, "space": n % 2 })).unwrap();
        }
        index
    }
}

fn ids<V>(group: &Group<V>) -> Vec<&str> {
    group.hits.iter().map(|hit| hit.id.as_str()).collect()
}

fn best(found: &ManyFound) -> Vec<&str> {
    found.best.iter().map(|best| best.hit.id.as_str()).collect()
}

#[test]
fn every_index_answers_in_its_own_group_in_the_order_asked() {
    let f = fixture();
    let courses = f.holding("courses", &["linear algebra", "algebra of logic", "history"]);
    let pages = f.holding("pages", &["algebra notes", "about"]);
    let found = f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a).run().unwrap();
    let names: Vec<&str> = found.groups.iter().map(|group| group.index.as_str()).collect();
    assert_eq!(names, ["courses", "pages"]);
    assert_eq!(ids(&found.groups[0]).len(), 2);
    assert_eq!(ids(&found.groups[1]), ["pages-0"]);
    assert!(found.groups.iter().all(|group| group.error.is_none() && !group.more));
    assert_eq!(found.best.len(), 3, "every group's hits once");
    assert_eq!(found.best[0].hit, found.groups[found.best[0].group].hits[0], "the same hit as its group's");
}

#[test]
fn the_stores_indexes_of_documents_are_searched_in_one_snapshot() {
    // contract: SRCH-24
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let pages = f.holding("pages", &["algebra"]);
    let writer = pages.clone();
    set_before_group(Some(Box::new(move || {
        // another thread's write, committed between the two groups' searches
        let writer = writer.clone();
        std::thread::spawn(move || writer.set("late", &json!({ "title": "algebra", "space": 0 })).unwrap())
            .join()
            .unwrap();
    })));
    let found = f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a).run();
    set_before_group(None);
    let found = found.unwrap();
    assert_eq!(ids(&found.groups[1]), ["pages-0"], "the write after the snapshot began is not seen");
    let after = pages.search("algebra").run().unwrap();
    assert_eq!(after.hits.len(), 2, "the write landed, and the next search sees it");
}

#[test]
fn an_index_that_fails_is_its_groups_error_and_the_others_answer() {
    // contract: SRCH-25
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let gone = f.holding("gone", &["algebra"]);
    let people = f.holding("people", &["algebra"]);
    assert!(f.store.drop_index("gone").unwrap());
    let found = f.store.search("algebra").among(&courses, |a| a).among(&gone, |a| a).among(&people, |a| a).run();
    let found = found.unwrap();
    let failed = found.groups[1].error.as_ref().expect("the dropped index's group says why");
    assert!(failed.to_string().contains("index gone"), "{failed}");
    assert!(found.groups[1].hits.is_empty() && !found.groups[1].more);
    assert_eq!(ids(&found.groups[0]), ["courses-0"]);
    assert_eq!(ids(&found.groups[2]), ["people-0"]);
    assert_eq!(best(&found), ["courses-0", "people-0"]);
    let filtered = f.store.search("algebra").among(&courses, |a| a.filter(eq("nowhere", &1))).among(&people, |a| a);
    let filtered = filtered.run().unwrap();
    assert_eq!(
        filtered.groups[0].error.as_ref().map(Error::kind),
        Some(ErrorKind::Invalid),
        "a filter it keeps none of"
    );
}

#[test]
fn a_call_whose_every_group_fails_fails_with_the_first_error() {
    // contract: SRCH-25
    let f = fixture();
    let first = f.holding("first", &["algebra"]);
    let second = f.holding("second", &["algebra"]);
    f.store.drop_index("first").unwrap();
    f.store.drop_index("second").unwrap();
    let failed = f.store.search("algebra").among(&first, |a| a).among(&second, |a| a).run().unwrap_err();
    assert!(failed.to_string().contains("index first"), "{failed}");
}

#[test]
fn a_cancel_or_a_timeout_between_groups_fails_the_whole_call() {
    // contract: SRCH-25
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let pages = f.holding("pages", &["algebra"]);
    let token = crate::CancellationToken::new();
    let cancelling = token.clone();
    set_before_group(Some(Box::new(move || cancelling.cancel())));
    let cancelled = token.run(|| f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a).run());
    set_before_group(None);
    let cancelled = cancelled.expect_err("a cancel is no group's answer");
    assert_eq!(cancelled.kind(), ErrorKind::Cancelled);
    assert!(!cancelled.to_string().contains("interrupted"), "said as the token's: {cancelled}");

    let timed = crate::CancellationToken::timeout(std::time::Duration::from_millis(20));
    set_before_group(Some(Box::new(|| std::thread::sleep(std::time::Duration::from_millis(40)))));
    let timed_out = timed.run(|| f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a).run());
    set_before_group(None);
    let timed_out = timed_out.expect_err("a timeout is no group's answer");
    assert_eq!(timed_out.kind(), ErrorKind::Cancelled);
    assert!(timed_out.to_string().contains("timeout"), "{timed_out}");
}

#[test]
fn more_says_exactly_whether_the_index_has_hits_past_the_limit() {
    // contract: SRCH-27
    let f = fixture();
    let three = f.holding("three", &["algebra one", "algebra two", "algebra three"]);
    let more = |limit: usize| {
        let found = f.store.search("algebra").among(&three, |a| a.limit(limit)).run().unwrap();
        (found.groups[0].hits.len(), found.groups[0].more)
    };
    assert_eq!(more(3), (3, false), "at the limit, nothing past it");
    assert_eq!(more(2), (2, true), "one past the limit");
    assert_eq!(more(4), (3, false));
}

#[test]
fn best_merges_the_groups_by_rank_and_boost_never_by_score() {
    // contract: SRCH-26
    let f = fixture();
    // courses scores its hits far lower than pages, whose title is the word alone
    let courses =
        f.holding("courses", &["algebra and a long title of many words", "algebra, logic and many more words"]);
    let pages = f.holding("pages", &["algebra", "algebra algebra"]);
    let even = f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a).run().unwrap();
    assert!(even.groups[1].hits[0].score > even.groups[0].hits[0].score, "scores that would put pages first");
    let (c, p) = (ids(&even.groups[0]), ids(&even.groups[1]));
    assert_eq!(best(&even), [c[0], p[0], c[1], p[1]], "equal weights: the group asked first, then the rank");
    let boosted = f.store.search("algebra").among(&courses, |a| a).among(&pages, |a| a.boost(2.0)).run().unwrap();
    let boosted: Vec<&str> = best(&boosted);
    // pages at 2/61 and 2/62 before courses at 1/61
    assert_eq!(boosted[..2].iter().filter(|id| id.starts_with("pages")).count(), 2, "{boosted:?}");
    let groups: Vec<usize> = even.best.iter().map(|best| best.group).collect();
    assert_eq!(groups, [0, 1, 0, 1]);
}

#[test]
fn the_same_index_may_be_asked_twice_with_another_filter() {
    let f = fixture();
    let pages = f.holding("pages", &["algebra even", "algebra odd", "algebra even again"]);
    let found = f
        .store
        .search("algebra")
        .among(&pages, |a| a.filter(eq("space", &0)))
        .among(&pages, |a| a.filter(eq("space", &1)).snippet("title", 20).matches());
    let found = found.run().unwrap();
    assert_eq!(ids(&found.groups[0]).len(), 2);
    assert_eq!(ids(&found.groups[1]), ["pages-1"]);
    let hit = &found.groups[1].hits[0];
    assert!(hit.snippet.is_some() && !hit.matches.is_empty(), "each group with its own options");
}

#[test]
fn a_call_of_no_index_too_many_or_options_no_group_takes_is_refused() {
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let invalid = |found: Result<ManyFound>| {
        let error = found.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        error.to_string()
    };
    assert!(invalid(f.store.search("algebra").run()).contains("no index"));
    let mut many = f.store.search("algebra");
    for _ in 0..=MOST_AMONG {
        many = many.among(&courses, |a| a);
    }
    assert!(invalid(many.run()).contains("33 indexes"));
    assert!(invalid(f.store.search("a").among(&courses, |a| a.limit(0)).run()).contains("1 to 100"));
    assert!(invalid(f.store.search("a").among(&courses, |a| a.limit(101)).run()).contains("1 to 100"));
    assert!(invalid(f.store.search("a").among(&courses, |a| a.boost(0.0)).run()).contains("positive"));
    assert!(invalid(f.store.search("a").among(&courses, |a| a.boost(f64::NAN)).run()).contains("positive"));
    let other = fixture();
    let theirs = other.holding("theirs", &["algebra"]);
    let refused = invalid(f.store.search("algebra").among(&courses, |a| a).among(&theirs, |a| a).run());
    assert!(refused.contains("another store"), "{refused}");
}

#[test]
fn a_misspelt_word_is_corrected_in_each_group_as_its_index_reads_it() {
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let found = f.store.search("algebar").among(&courses, |a| a).run().unwrap();
    assert_eq!(found.groups[0].corrected.as_deref(), Some("algebra"));
}

#[cfg(feature = "sql")]
#[test]
fn a_tables_index_answers_beside_the_stores_indexes_its_rows_as_json() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Person {
        id: i64,
        name: String,
    }
    let f = fixture();
    let courses = f.holding("courses", &["algebra"]);
    let db = f
        .store
        .database("app")
        .migrations([("0001_people.sql", "create table people (id integer primary key, name text)")]);
    let db = db.open().unwrap();
    db.exec(crate::sql!("insert into people (id, name) values (1, 'Ada of algebra'), (2, 'Bob')")).unwrap();
    let people = db.search_index::<Person>("people").table("people").field("name", 1.0).open().unwrap();
    crate::search::maintain(&f.store).unwrap();
    people.ready().unwrap();
    let found = f.store.search("algebra").among(&courses, |a| a).among(&people, |a| a.limit(5)).run().unwrap();
    assert_eq!(ids(&found.groups[1]), ["1"]);
    assert_eq!(found.groups[1].hits[0].value, json!({ "id": 1, "name": "Ada of algebra" }));
    assert_eq!(best(&found), ["courses-0", "1"]);
}

/// One search of 13 small indexes against 13 searches of one each, in the
/// same run, in turns, the median of passes:
/// `cargo test --release -p tinystore --lib --features testing many_against_single -- --ignored --nocapture`.
#[test]
#[ignore = "a timing, run by hand"]
fn many_against_single() {
    use std::time::Instant;
    let f = fixture();
    let titles: Vec<String> = (0..200).map(|n| format!("algebra course {n} of the term {}", n % 7)).collect();
    let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
    let indexes: Vec<Index<Value>> = (0..13).map(|n| f.holding(&format!("index-{n}"), &titles)).collect();
    crate::search::maintain(&f.store).unwrap();
    let (mut many, mut single) = (Vec::new(), Vec::new());
    for _ in 0..30 {
        let started = Instant::now();
        for _ in 0..20 {
            let asked =
                indexes.iter().fold(f.store.search("algebra term"), |many, index| many.among(index, |a| a.limit(5)));
            asked.run().unwrap();
        }
        many.push(started.elapsed() / 20);
        let started = Instant::now();
        for _ in 0..20 {
            for index in &indexes {
                index.search("algebra term").limit(6).run().unwrap();
            }
        }
        single.push(started.elapsed() / 20);
    }
    many.sort();
    single.sort();
    println!(
        "13 indexes of 200 documents, limit 5: one search among them {:?}, 13 searches {:?} (medians of 30)",
        many[15], single[15]
    );
}
