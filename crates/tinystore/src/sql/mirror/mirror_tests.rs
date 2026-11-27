//! A mirror against what its table holds: what each call answers, what it
//! writes and what it leaves, its guard, its refusals, and its cost against
//! the two loops a program writes without it.

use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::sql::{Changed, Database, Diff};
use crate::{ErrorKind, Options, Store, TestClock, sql, unix_millis};

const WORKS: &str = "create table works (
    course_id  integer not null,
    id         text not null,
    title      text not null,
    grade      real,
    fetched_at integer not null default 0,
    gone_at    integer,
    primary key (course_id, id)
) strict;
create table writes (id text not null);
create trigger written after update on works begin insert into writes values (new.id); end;
create trigger inserted after insert on works begin insert into writes values (new.id); end;";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Work {
    course_id: i64,
    id: String,
    title: String,
    grade: Option<f64>,
}

fn work(course_id: i64, id: &str, title: &str, grade: Option<f64>) -> Work {
    Work { course_id, id: id.to_owned(), title: title.to_owned(), grade }
}

struct Fixture {
    db: Database,
    clock: Arc<TestClock>,
    _store: Store,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let db = store.database("lms").migrations([("0001_works.sql", WORKS)]).open().unwrap();
    Fixture { db, clock, _store: store, _dir: dir }
}

impl Fixture {
    /// The ids written since the last look, and none after.
    fn written(&self) -> Vec<String> {
        let ids: Vec<String> = self.db.all("select id from writes order by rowid").unwrap();
        self.db.exec("delete from writes").unwrap();
        ids
    }

    fn held(&self) -> Vec<(i64, String, Option<f64>, Option<i64>)> {
        self.db.all("select course_id, id, grade, gone_at from works order by course_id, id").unwrap()
    }
}

fn three() -> Vec<Work> {
    vec![work(1, "a", "Essay", Some(4.0)), work(1, "b", "Quiz", None), work(1, "c", "Lab", Some(3.0))]
}

// contract: SQL-17
#[test]
fn replace_answers_exactly_what_was_added_changed_gone_and_the_same() {
    let f = fixture();
    let works =
        f.db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").change_on(["grade"]).open().unwrap();
    let first = works.replace(&[("course_id", 1)], &three()).unwrap();
    assert_eq!(first, Diff { added: three(), changed: vec![], gone: vec![], same: 0, suspect: false });

    // b graded, c dropped by the source, d new, a's title alone rewritten
    f.clock.advance(Duration::from_secs(60));
    let fetched =
        vec![work(1, "a", "Essay (v2)", Some(4.0)), work(1, "b", "Quiz", Some(5.0)), work(1, "d", "Exam", None)];
    let diff = works.replace(&[("course_id", 1)], &fetched).unwrap();
    assert_eq!(diff.added, [work(1, "d", "Exam", None)]);
    assert_eq!(diff.changed, [Changed { before: work(1, "b", "Quiz", None), after: work(1, "b", "Quiz", Some(5.0)) }]);
    assert_eq!(diff.gone, [work(1, "c", "Lab", Some(3.0))]);
    assert_eq!((diff.same, diff.suspect), (1, false));
    let now = unix_millis(f.clock.now());
    assert_eq!(
        f.held(),
        [
            (1, "a".to_owned(), Some(4.0), None),
            (1, "b".to_owned(), Some(5.0), None),
            (1, "c".to_owned(), Some(3.0), Some(now)),
            (1, "d".to_owned(), None, None),
        ]
    );
    let title: String = f.db.scalar("select title from works where id = 'a'").unwrap();
    assert_eq!(title, "Essay", "a's title alone differs, and grade is what is compared: a is not written");

    // c marked is neither marked nor answered again; back, it is added and loses its mark
    let again = works.replace(&[("course_id", 1)], &fetched).unwrap();
    assert_eq!((again.added.len(), again.changed.len(), again.gone.len(), again.same), (0, 0, 0, 3));
    let back = [fetched.clone(), vec![work(1, "c", "Lab", Some(3.0))]].concat();
    let returned = works.replace(&[("course_id", 1)], &back).unwrap();
    assert_eq!((returned.added, returned.same), (vec![work(1, "c", "Lab", Some(3.0))], 3));
    assert_eq!(f.held()[2], (1, "c".to_owned(), Some(3.0), None));
}

// contract: SQL-19
#[test]
fn a_row_the_list_gives_as_it_is_held_is_not_written() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    assert_eq!(f.written(), ["a", "b", "c"]);
    let diff = works.replace(&[("course_id", 1)], &three()).unwrap();
    assert_eq!(diff.same, 3);
    assert!(f.written().is_empty(), "no row of the same list is written");

    let mut graded = three();
    graded[1].grade = Some(2.0);
    // 4 beside 4.0 is one number, as SQLite's `is` says
    let diff = works.replace(&[("course_id", 1.0)], &graded).unwrap();
    assert_eq!((diff.changed.len(), diff.same), (1, 2));
    assert_eq!(f.written(), ["b"], "only the row that changed is written");
}

#[test]
fn without_a_gone_column_what_the_list_leaves_out_is_deleted_and_other_scopes_stay() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    works.replace(&[("course_id", 2)], &[work(2, "a", "Other", None)]).unwrap();
    let diff = works.replace(&[("course_id", 1)], &three()[..1]).unwrap();
    assert_eq!(diff.gone, three()[1..]);
    let ids: Vec<(i64, String)> = f.db.all("select course_id, id from works order by course_id, id").unwrap();
    assert_eq!(ids, [(1, "a".to_owned()), (2, "a".to_owned())]);
    let none = works.replace::<&str, i64>(&[], &[]).unwrap();
    assert_eq!(none.gone.len(), 2, "an empty scope is the whole table");
}

/// The temporary tables on the database's writer, where a run's parts wait.
fn runs_left(db: &Database) -> Vec<String> {
    db.tx(|tx| tx.all::<String>("select name from sqlite_temp_master where type = 'table'")).unwrap()
}

// contract: SQL-26
#[test]
fn a_replace_in_parts_writes_nothing_until_its_commit_and_answers_as_one_replace() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    f.written();
    let run = works.begin(&[("course_id", 1)]).unwrap();
    run.add(&[work(1, "a", "Essay", Some(5.0))]).unwrap();
    run.add(&[]).unwrap();
    run.add(&[work(1, "y", "New", None), work(1, "z", "Newer", None)]).unwrap();
    assert!(f.written().is_empty(), "a part writes nothing to the table");
    assert_eq!(runs_left(&f.db).len(), 1, "the parts wait on the writer");
    let diff = run.commit().unwrap();
    assert_eq!(diff.added, [work(1, "y", "New", None), work(1, "z", "Newer", None)]);
    assert_eq!(diff.changed.len(), 1);
    let gone: Vec<&str> = diff.gone.iter().map(|work| work.id.as_str()).collect();
    assert_eq!((gone, diff.same), (vec!["b", "c"], 0), "gone: what no part named");
    assert_eq!(f.held().iter().filter(|(_, _, _, gone)| gone.is_some()).count(), 2);
    assert!(runs_left(&f.db).is_empty(), "the commit dropped the run's table");

    let guarded = works.min_kept(0.9).begin(&[("course_id", 1)]).unwrap();
    guarded.add(&[work(1, "a", "Essay", Some(5.0))]).unwrap();
    assert!(guarded.commit().unwrap().suspect, "the guard holds the commit back");
}

// contract: SQL-26
#[test]
fn a_run_let_go_of_idle_or_refused_writes_nothing_and_leaves_no_table() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    let before = f.held();
    {
        let run = works.begin(&[("course_id", 1)]).unwrap();
        run.add(&three()[..1]).unwrap();
    }
    f.db.exec("delete from writes").unwrap(); // a write after the drop's
    assert!(runs_left(&f.db).is_empty(), "a run dropped leaves no table");
    assert_eq!(f.held(), before, "and wrote nothing");

    let run = works.begin(&[("course_id", 1)]).unwrap();
    run.add(&three()[..1]).unwrap();
    run.add(&three()[..1]).unwrap(); // the same key again, which the commit finds
    let committed = run.commit().unwrap_err();
    assert_eq!(committed.kind(), ErrorKind::Invalid, "{committed}");
    assert!(committed.to_string().contains("given twice"), "a key once in the whole run: {committed}");
    assert_eq!(f.held(), before, "a commit refused writes nothing");

    let run = works.begin(&[("course_id", 1)]).unwrap();
    let outside = run.add(&[work(2, "a", "Essay", None)]).unwrap_err();
    assert!(outside.to_string().contains("outside the scope"), "{outside}");
    assert!(run.add(&three()).unwrap_err().to_string().contains("a part failed"), "a part that fails ends the run");

    let run = works.begin(&[("course_id", 1)]).unwrap();
    f.clock.advance(Duration::from_secs(5 * 60 + 1));
    let idle = run.add(&three()).unwrap_err();
    assert_eq!(idle.kind(), ErrorKind::Limit, "{idle}");
    assert_eq!(f.held(), before);
}

// contract: SQL-25
#[test]
fn gone_marks_with_the_stores_clock_in_the_unit_its_mark_names() {
    let f = fixture();
    let works =
        f.db.mirror::<Work>("works").key(["course_id", "id"]).gone(sql::Mark::seconds("gone_at")).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    f.clock.advance(Duration::from_millis(2_500));
    works.replace(&[("course_id", 1)], &three()[..2]).unwrap();
    let seconds = unix_millis(f.clock.now()).div_euclid(1000);
    assert_eq!(f.held()[2].3, Some(seconds), "the test clock's whole second");
    let by_millis = f.db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").open().unwrap();
    by_millis.replace(&[("course_id", 1)], &three()[..1]).unwrap();
    assert_eq!(f.held()[1].3, Some(unix_millis(f.clock.now())), "a column alone marks in milliseconds");
}

#[test]
fn upsert_writes_what_it_adds_and_changes_and_marks_nothing_gone() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    let diff = works.upsert(&[work(1, "b", "Quiz", Some(1.0)), work(3, "z", "New", None)]).unwrap();
    assert_eq!(diff.added, [work(3, "z", "New", None)]);
    assert_eq!(diff.changed.len(), 1);
    assert!(diff.gone.is_empty());
    assert_eq!(f.held().len(), 4);
    assert!(f.held().iter().all(|(_, _, _, gone)| gone.is_none()));
}

// contract: SQL-18
#[test]
fn a_replace_that_fails_on_its_last_write_writes_none_of_them() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    f.written();
    // a mirror inserts, then updates, then deletes: the delete of c fails last
    f.db.exec("create trigger refused before delete on works when old.id = 'c' begin select raise(abort, 'no c'); end")
        .unwrap();
    let mut list = three()[..2].to_vec();
    list[0].grade = Some(1.0);
    list.push(work(1, "y", "Fine", None));
    let refused = works.replace(&[("course_id", 1)], &list).unwrap_err();
    assert!(refused.to_string().contains("no c"), "{refused}");
    let ids: Vec<String> = f.held().into_iter().map(|(_, id, _, _)| id).collect();
    assert_eq!(ids, ["a", "b", "c"], "y not added, c not deleted");
    assert_eq!(f.held()[0].2, Some(4.0), "a's grade not changed");
    assert!(f.written().is_empty(), "the insert and the update ran, and went with the delete");
}

/// A row of a non-strict table whose columns convert what they are given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Loose {
    id: serde_json::Value,
    amount: serde_json::Value,
}

// contract: SQL-19
#[test]
fn a_list_compares_as_its_columns_keep_what_it_gives() {
    let f = fixture();
    f.db.exec("create table loose (id text not null primary key, amount integer, price real)").unwrap();
    let loose = f.db.mirror::<Loose>("loose").key(["id"]).open().unwrap();
    // a text key given 4 keeps "4", an integer column given "12" keeps 12
    let given = [Loose { id: serde_json::json!(4), amount: serde_json::json!("12") }];
    assert_eq!(loose.replace::<&str, i64>(&[], &given).unwrap().added.len(), 1);
    let again = loose.replace::<&str, i64>(&[], &given).unwrap();
    assert_eq!((again.added.len(), again.changed.len(), again.same), (0, 0, 1), "{again:?}");
    let held: (String, i64) = f.db.one("select id, amount from loose").unwrap().unwrap();
    assert_eq!(held, ("4".to_owned(), 12));
    // "4" and 4 are one key of the text column: the list gives it twice
    let twice = [given[0].clone(), Loose { id: serde_json::json!("4"), amount: serde_json::json!(12) }];
    let refused = loose.replace::<&str, i64>(&[], &twice).unwrap_err();
    assert!(refused.to_string().contains("given twice, as the table compares its key"), "{refused}");
    // text that is not a number whole stays text in an integer column
    let apples = [Loose { id: serde_json::json!(4), amount: serde_json::json!("12 apples") }];
    assert_eq!(loose.replace::<&str, i64>(&[], &apples).unwrap().changed.len(), 1);
    assert_eq!(loose.replace::<&str, i64>(&[], &apples).unwrap().same, 1);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Person {
    email: String,
    name: String,
}

// contract: SQL-19
#[test]
fn a_key_compares_by_its_index_collation() {
    let f = fixture();
    f.db.exec("create table people (email text not null primary key collate nocase, name text not null)").unwrap();
    let people = f.db.mirror::<Person>("people").key(["email"]).open().unwrap();
    let person = |email: &str, name: &str| Person { email: email.to_owned(), name: name.to_owned() };
    people.replace::<&str, i64>(&[], &[person("Ann@example.org", "Ann")]).unwrap();
    let again = people.replace::<&str, i64>(&[], &[person("ann@example.org", "Ann")]).unwrap();
    assert_eq!((again.added.len(), again.gone.len(), again.same), (0, 0, 1), "{again:?}");
    let renamed = people.replace::<&str, i64>(&[], &[person("ANN@example.org", "Anna")]).unwrap();
    assert_eq!(renamed.changed.len(), 1);
    let names: Vec<String> = f.db.all("select name from people").unwrap();
    assert_eq!(names, ["Anna"]);
    let twice = [person("a@example.org", "A"), person("A@example.org", "A")];
    let refused = people.replace::<&str, i64>(&[], &twice).unwrap_err();
    assert!(refused.to_string().contains("given twice, as the table compares its key"), "{refused}");
}

/// A work as a program reads it back, the mark among its fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Marked {
    course_id: i64,
    id: String,
    title: String,
    gone_at: Option<i64>,
}

#[test]
fn a_row_type_that_carries_the_mark_null_is_mirrored_and_one_that_gives_it_is_refused() {
    let f = fixture();
    let works = f.db.mirror::<Marked>("works").key(["course_id", "id"]).gone("gone_at").open().unwrap();
    let marked = |id: &str, gone_at| Marked { course_id: 1, id: id.to_owned(), title: "T".to_owned(), gone_at };
    let first = works.replace(&[("course_id", 1)], &[marked("a", None), marked("b", None)]).unwrap();
    assert_eq!(first.added, [marked("a", None), marked("b", None)]);
    let dropped = works.replace(&[("course_id", 1)], &[marked("a", None)]).unwrap();
    assert_eq!((dropped.same, dropped.gone.clone()), (1, vec![marked("b", None)]), "the answer carries no mark");
    assert!(f.held()[1].3.is_some(), "b is marked");
    let refused = works.replace(&[("course_id", 1)], &[marked("a", Some(1))]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert!(refused.to_string().contains("row 1: it marks \"gone_at\""), "{refused}");
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Dotted {
    #[serde(rename = "item.id")]
    id: i64,
    #[serde(rename = "item.name")]
    name: String,
}

#[test]
fn a_column_whose_name_holds_a_dot_is_one_name() {
    let f = fixture();
    f.db.exec(r#"create table "shop.items" ("item.id" integer primary key, "item.name" text not null)"#).unwrap();
    let items = f.db.mirror::<Dotted>("shop.items").key(["item.id"]).open().unwrap();
    let item = |name: &str| Dotted { id: 1, name: name.to_owned() };
    assert_eq!(items.replace::<&str, i64>(&[], &[item("cup")]).unwrap().added.len(), 1);
    assert_eq!(items.replace::<&str, i64>(&[], &[item("mug")]).unwrap().changed.len(), 1);
    assert_eq!(items.replace::<&str, i64>(&[], &[]).unwrap().gone, [item("mug")]);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Code {
    code: Option<String>,
    label: String,
}

#[test]
fn a_row_whose_key_holds_a_null_is_refused() {
    let f = fixture();
    f.db.exec("create table codes (code text unique, label text not null)").unwrap();
    let codes = f.db.mirror::<Code>("codes").key(["code"]).open().unwrap();
    let rows =
        [Code { code: Some("a".to_owned()), label: "A".to_owned() }, Code { code: None, label: "none".to_owned() }];
    let refused = codes.replace::<&str, i64>(&[], &rows).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(refused.to_string(), "sql lms: mirror codes: row (code null): its key's code is null: invalid");
    assert_eq!(codes.upsert(&rows).unwrap_err().kind(), ErrorKind::Invalid);
    let count: i64 = f.db.scalar("select count(*) from codes").unwrap();
    assert_eq!(count, 0);
}

#[cfg(feature = "replication")]
#[test]
fn a_mirror_on_a_copy_is_refused_unavailable_naming_it() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    f.db.file().become_copy().unwrap();
    for refused in [works.replace(&[("course_id", 1)], &three()), works.upsert(&three()[..1])] {
        let refused = refused.unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Unavailable, "{refused}");
        assert!(refused.to_string().starts_with("sql lms: mirror works: a mirror writes"), "{refused}");
    }
}

// contract: SQL-20
#[test]
fn the_min_kept_guard_answers_what_it_would_do_and_writes_nothing() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).gone("gone_at").open().unwrap();
    works.replace(&[("course_id", 1)], &three()).unwrap();
    f.written();
    let held = f.held();
    let empty = works.min_kept(0.5).replace(&[("course_id", 1)], &[]).unwrap();
    assert_eq!((empty.gone.len(), empty.suspect), (3, true));
    let one = works.min_kept(0.5).replace(&[("course_id", 1)], &[work(1, "a", "Essay", Some(9.0))]).unwrap();
    assert_eq!((one.changed.len(), one.gone.len(), one.suspect), (1, 2, true));
    assert_eq!(f.held(), held, "a suspect answer wrote nothing");
    assert!(f.written().is_empty());
    let two = works.min_kept(0.5).replace(&[("course_id", 1)], &three()[..2]).unwrap();
    assert_eq!((two.gone.len(), two.suspect), (1, false), "two of three live is past a half");
    // a scope that holds nothing live is never held back
    let fresh = works.min_kept(1.0).replace(&[("course_id", 9)], &[]).unwrap();
    assert!(!fresh.suspect);
    for fraction in [0.0, 1.5, f64::NAN] {
        let refused = works.min_kept(fraction).replace(&[("course_id", 1)], &[]).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{fraction}");
    }
}

#[test]
fn a_list_that_cannot_be_right_is_refused_naming_the_mirror_and_the_row() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    let outside = works.replace(&[("course_id", 1)], &[work(2, "a", "Elsewhere", None)]).unwrap_err();
    assert_eq!(outside.kind(), ErrorKind::Invalid);
    assert_eq!(
        outside.to_string(),
        "sql lms: mirror works: row (course_id 2, id \"a\"): course_id 2, outside the scope's course_id 1: invalid"
    );
    let twice =
        works.replace(&[("course_id", 1)], &[work(1, "a", "One", None), work(1, "a", "Two", None)]).unwrap_err();
    assert!(twice.to_string().contains("row (course_id 1, id \"a\"): given twice"), "{twice}");
    let scope = works.replace(&[("term", "fall")], &three()).unwrap_err();
    assert!(scope.to_string().contains("no scope column \"term\""), "{scope}");
}

#[test]
fn an_open_refuses_what_its_table_lacks_and_a_key_that_names_several_rows() {
    let f = fixture();
    let refused = |builder: crate::sql::MirrorBuilder<Work>| builder.open().unwrap_err();
    let no_table = refused(f.db.mirror("workz").key(["id"]));
    assert_eq!(no_table.to_string(), "sql lms: mirror workz: the database has no table \"workz\": invalid");
    let no_column = refused(f.db.mirror("works").key(["course_id", "id"]).gone("deleted_at"));
    assert!(no_column.to_string().contains("no column \"deleted_at\""), "{no_column}");
    let several = refused(f.db.mirror("works").key(["id"]));
    assert_eq!(several.kind(), ErrorKind::Invalid);
    assert!(several.to_string().starts_with("sql lms: mirror works: the key (id) names no one row"), "{several}");
    f.db.exec("create unique index works_by_id on works (id)").unwrap();
    f.db.mirror::<Work>("works").key(["id"]).open().unwrap();
    f.db.mirror::<Work>("works").key(["id", "title"]).open().unwrap();
    let no_key = refused(f.db.mirror("works"));
    assert!(no_key.to_string().contains("at least one column"), "{no_key}");
}

#[test]
fn a_mirror_joins_its_databases_transaction_and_alone_is_refused_inside_it() {
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    let rolled = f.db.tx(|tx| -> crate::Result<()> {
        let diff = tx.with(&works).replace(&[("course_id", 1)], &three())?;
        assert_eq!(diff.added.len(), 3);
        assert_eq!(tx.scalar::<i64>("select count(*) from works")?, 3, "the transaction sees what it wrote");
        let alone = works.replace(&[("course_id", 1)], &three()).unwrap_err();
        assert_eq!(alone.kind(), ErrorKind::Invalid);
        Err(crate::Error::invalid("rolled back"))
    });
    assert!(rolled.is_err());
    assert!(f.held().is_empty(), "the mirror's rows went with the transaction");
}

/// A course of `rows` works, `changed` of them graded anew.
fn course(rows: usize, changed: usize, pass: usize) -> Vec<Work> {
    (0..rows)
        .map(|at| {
            let grade = if at < changed { (pass * 10 + at) as f64 } else { at as f64 };
            work(1, &format!("w{at:05}"), &format!("Work {at} of a course whose title runs on a while"), Some(grade))
        })
        .collect()
}

/// The rows the triggers saw written since the last look.
fn rows_written(db: &Database) -> i64 {
    let written: i64 = db.scalar("select count(*) from writes").unwrap();
    db.exec("delete from writes").unwrap();
    written
}

/// `cargo test --release -p tinystore --lib --features async,testing
/// mirror_against_the_loops -- --ignored --nocapture`: a replace of 5,000
/// rows of which 1% changed, against a delete of every row and an insert of
/// all, and an upsert of all, on the same list in the same run, the median of
/// nine passes each. `writes` counts the rows each one wrote, by triggers.
#[test]
#[ignore = "a timing, run by hand"]
fn mirror_against_the_loops() {
    const ROWS: usize = 5_000;
    const CHANGED: usize = ROWS / 100;
    let f = fixture();
    let works = f.db.mirror::<Work>("works").key(["course_id", "id"]).open().unwrap();
    let table = f.db.table::<Work>("works");
    works.replace(&[("course_id", 1)], &course(ROWS, 0, 0)).unwrap();
    rows_written(&f.db);
    let mut taken: [Vec<Duration>; 3] = Default::default();
    let mut written = [0; 3];
    for pass in 1..=9 {
        let list = course(ROWS, CHANGED, pass);
        let started = Instant::now();
        works.replace(&[("course_id", 1)], &list).unwrap();
        taken[0].push(started.elapsed());
        written[0] = rows_written(&f.db);

        let list = course(ROWS, CHANGED, pass + 100);
        let started = Instant::now();
        f.db.tx(|tx| -> crate::Result<()> {
            tx.exec(sql!("delete from works where course_id = ?", 1))?;
            for chunk in list.chunks(1000) {
                tx.table::<Work>("works").insert_all(chunk)?;
            }
            Ok(())
        })
        .unwrap();
        taken[1].push(started.elapsed());
        written[1] = rows_written(&f.db);

        let list = course(ROWS, CHANGED, pass + 200);
        let upsert = || sql::Upsert::on(["course_id", "id"]).update(["title", "grade"]);
        let started = Instant::now();
        for chunk in list.chunks(1000) {
            table.upsert_all(chunk, upsert()).unwrap();
        }
        taken[2].push(started.elapsed());
        written[2] = rows_written(&f.db);
    }
    for (name, (mut passes, rows)) in
        ["mirror replace", "delete and insert_all", "upsert_all"].iter().zip(taken.into_iter().zip(written))
    {
        passes.sort();
        println!("{name:>22}: median {:>8.2?} of 9 passes, {rows} rows written", passes[4]);
    }
}
