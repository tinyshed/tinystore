//! A stream's `where` kept to the values a database's statement gives: read
//! once as the query starts, compared as a list's values are, and bounded.

use std::time::Duration;

use serde_json::json;

use super::super::fixture::{DAY, Fixture, SECOND, fixture, released, timed_in_turns};
use super::super::maintain::maintain;
use crate::records::{Aggregate, Events, NewEvent, Not};
use crate::sql::Database;
use crate::{ErrorKind, sql};

const HOUR: Duration = Duration::from_secs(60 * 60);
const PEOPLE: &str = "create table teachers (id integer primary key) strict;
    create table students (id integer primary key, first_seen_at integer not null) strict;
    create table kept (k text primary key, v any) strict";

fn app(fixture: &Fixture) -> Database {
    fixture.store.database("app").migrations([("0001_people.sql", PEOPLE)]).open().unwrap()
}

/// An event of each `userId`, in their order, a second apart.
fn used(fixture: &Fixture, events: &Events, users: &[serde_json::Value]) {
    for (n, user) in users.iter().enumerate() {
        events.append("used", &json!({ "userId": user, "n": n })).unwrap();
        fixture.pass(SECOND);
    }
}

// contract: REC-22
#[test]
fn a_where_keeps_to_the_values_a_statement_of_a_database_gives() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    used(&fixture, &events, &[json!(1), json!(2), json!(1)]);
    db.exec("insert into teachers (id) values (2)").unwrap();
    let teachers = db.statement("select id from teachers");
    let list = events.list().since(DAY);
    assert_eq!(list.clone().filter("userId", &teachers).count().unwrap(), 1);
    assert_eq!(list.clone().filter("userId", Not(&teachers)).count().unwrap(), 2);
    let page = list.clone().filter("userId", &teachers).page().unwrap();
    assert_eq!(page.events.len(), 1);
    let by = Aggregate::new().by(["name"]).distinct(["userId"]);
    let rows = list.clone().filter("userId", Not(&teachers)).aggregate(&by).unwrap();
    assert_eq!((rows.len(), rows[0].count, rows[0].distinct["userId"]), (1, 2, 1));
    let top = list.clone().filter("userId", &teachers).top("userId").unwrap();
    assert_eq!((top[0].value.clone(), top[0].count), (json!(2), 1));
    assert_eq!(list.clone().filter("userId", &teachers).largest("n").unwrap()[0].fields.json("n"), Some("1"));
    // a statement of no rows keeps nothing, and none of it keeps everything
    let none = db.statement(sql!("select id from teachers where id > ?", 9));
    assert_eq!(list.clone().filter("userId", &none).count().unwrap(), 0);
    assert_eq!(list.filter("userId", Not(none)).count().unwrap(), 3);
}

// contract: REC-22
#[test]
fn a_statements_values_compare_as_the_json_an_events_field_is_written_as() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    used(&fixture, &events, &[json!(7), json!("7"), json!(7.0), json!(0.5), json!(null), json!([7])]);
    let rows = [("integer", "7"), ("text", "'7'"), ("real", "7.0"), ("half", "0.5"), ("null", "null")];
    for (k, v) in rows {
        db.exec(format!("insert into kept (k, v) values ('{k}', {v})").as_str()).unwrap();
    }
    db.exec("insert into kept (k, v) values ('blob', x'07')").unwrap();
    let found = |k: &str| {
        let statement = db.statement(sql!("select v from kept where k = ?", k));
        let page = events.list().since(DAY).filter("userId", statement).page().unwrap();
        page.events.iter().map(|event| event.fields.json("userId").unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(found("integer"), ["7"]);
    assert_eq!(found("text"), ["\"7\""]);
    assert_eq!(found("real"), ["7.0"]);
    assert_eq!(found("half"), ["0.5"]);
    assert!(found("null").is_empty(), "a null matches nothing, a JSON null among them");
    let blob = db.statement(sql!("select v from kept where k = ?", "blob"));
    let refused = events.list().since(DAY).filter("userId", blob).count().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("where \"userId\": sql app: statement"), "{refused}");
    assert!(refused.to_string().contains("a blob"), "{refused}");
}

// contract: REC-22
#[test]
fn a_statement_is_read_as_a_query_starts_and_again_for_a_page_asked_after() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    used(&fixture, &events, &[json!(1), json!(2), json!(3)]);
    db.exec("insert into teachers (id) values (1)").unwrap();
    let teachers = db.statement("select id from teachers");
    let list = events.list().since(DAY).filter("userId", &teachers).limit(1);
    let first = list.page().unwrap();
    assert_eq!(first.events[0].fields.json("userId"), Some("1"));
    db.exec("insert into teachers (id) values (3)").unwrap();
    let next = list.clone().after(&first.next.unwrap()).page().unwrap();
    assert_eq!(next.events[0].fields.json("userId"), Some("3"), "the page after read the statement again");
    // a walk reads it once, as it starts: what changes meanwhile changes nothing it reads
    let mut all = events.list().since(DAY).limit(1).filter("userId", &teachers).all();
    assert_eq!(all.next().unwrap().unwrap().fields.json("userId"), Some("1"));
    db.exec("insert into teachers (id) values (2)").unwrap();
    let rest: Vec<String> = all.map(|event| event.unwrap().fields.json("userId").unwrap().to_owned()).collect();
    assert_eq!(rest, ["3"]);
}

// contract: REC-22
#[test]
fn a_statement_past_its_rows_that_writes_or_gives_two_columns_is_refused_and_a_tail_takes_none() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    used(&fixture, &events, &[json!(1)]);
    let count = |statement: &str| events.list().since(DAY).filter("userId", db.statement(statement)).count();
    let many = "with recursive n(x) as (select 1 union all select x + 1 from n where x < 100001) select x from n";
    let past = count(many).unwrap_err();
    assert_eq!(past.kind(), ErrorKind::Limit, "{past}");
    assert!(past.to_string().contains("more than 100000 rows"), "{past}");
    let bound = many.replace("100001", "100000");
    assert_eq!(count(&bound).unwrap(), 1);
    for (statement, why) in [
        ("insert into teachers (id) values (1) returning id", "this one writes"),
        ("select id, id from teachers", "gives 2"),
    ] {
        let refused = count(statement).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        assert!(refused.to_string().contains(why), "{refused}");
    }
    let tail = events.list().filter("userId", db.statement("select id from teachers")).tail().unwrap_err();
    assert_eq!(tail.kind(), ErrorKind::Invalid, "{tail}");
    assert!(tail.to_string().contains("a tail's where takes no statement"), "{tail}");
}

// contract: REC-22
#[test]
fn a_delete_reads_its_statement_once_as_it_starts() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    used(&fixture, &events, &[json!(1), json!(2), json!(1), json!(3)]);
    db.exec("insert into students (id, first_seen_at) values (1, 0), (3, 0)").unwrap();
    let students = db.statement("select id from students");
    assert_eq!(events.list().filter("userId", &students).delete().unwrap(), 3);
    let left = events.list().page().unwrap();
    assert_eq!(left.events.iter().map(|event| event.fields.json("userId").unwrap()).collect::<Vec<_>>(), ["2"]);
}

/// Users `u-0` to `u-1999` a few events each, packed into blocks whose
/// filters hold their ids, and the set of every third.
fn packed(fixture: &Fixture, db: &Database) -> Events {
    let events = fixture.events("activity");
    let people: Vec<NewEvent> =
        (0..6000).map(|n| NewEvent::new("used").fields(&json!({ "userId": format!("u-{}", n % 2000) }))).collect();
    events.append_all(people).unwrap();
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    assert_eq!(fixture.rows("heads"), 0, "every event packed");
    for n in (0..2000).step_by(3) {
        db.exec(sql!("insert into kept (k, v) values (?, ?)", format!("u-{n}"), format!("u-{n}"))).unwrap();
    }
    events
}

// contract: REC-22
#[test]
fn a_statement_of_thousands_of_values_keeps_to_them_in_packed_blocks_as_a_short_list_does() {
    let fixture = fixture();
    let db = app(&fixture);
    let events = packed(&fixture, &db);
    let kept = db.statement("select v from kept");
    let list = events.list().since(DAY);
    assert_eq!(list.clone().filter("userId", &kept).count().unwrap(), 667 * 3);
    assert_eq!(list.clone().filter("userId", Not(&kept)).count().unwrap(), 6000 - 667 * 3);
    // a short one asks the blocks' filters, and finds what a list finds
    let few = db.statement("select v from kept where k in ('u-3', 'u-999', 'u-nobody')");
    assert_eq!(list.clone().filter("userId", few).count().unwrap(), 6);
    assert_eq!(list.filter("userId", &["u-3", "u-999", "u-nobody"]).count().unwrap(), 6);
}

// contract: REC-22, NOT-12
#[test]
fn a_statement_keeps_to_its_values_what_v0_1_0_packed() {
    let fixture = released();
    let db = app(&fixture);
    let all = fixture.store.all_events().unwrap();
    let packed = all.list().count().unwrap();
    // v0.1.0's events carry no field: its blocks are read and hold no teacher
    used(&fixture, &fixture.events("api"), &[json!(1), json!(2), json!(3)]);
    db.exec("insert into teachers (id) values (2), (4)").unwrap();
    let teachers = db.statement("select id from teachers");
    assert_eq!(all.list().filter("userId", &teachers).count().unwrap(), 1);
    assert_eq!(all.list().filter("userId", Not(&teachers)).count().unwrap(), packed + 2);
    let page = all.list().filter("userId", Not(&teachers)).limit(10_000).page().unwrap();
    assert_eq!(page.events.len() as u64, packed + 2);
}

#[test]
#[ignore = "a timing, run by hand"]
fn a_count_kept_to_a_statement_of_fifty_thousand_beside_no_where_and_the_programs_own_filter() {
    const EVENTS: usize = 1_000_000;
    const USERS: usize = 100_000;
    let fixture = fixture();
    let db = app(&fixture);
    let events = fixture.events("activity");
    for batch in 0..EVENTS / 10_000 {
        let made = (0..10_000).map(|n| {
            let user = (batch * 10_000 + n) % USERS;
            NewEvent::new("used").fields(&json!({ "userId": user, "route": "/notes" }))
        });
        events.append_all(made).unwrap();
    }
    fixture.pass(HOUR);
    maintain(&fixture.store).unwrap();
    let every_other = "with recursive n(x) as (select 0 union all select x + 2 from n where x < 99998) select x from n";
    db.exec(format!("insert into teachers (id) {every_other}").as_str()).unwrap();
    let teachers = db.statement("select id from teachers");
    let list = events.list().since(DAY);
    let kept = list.clone().filter("userId", &teachers);
    // the program's old way: an aggregate by person, kept to the ids after,
    // which past 10,000 people is every event walked and counted by hand
    let refused = list.aggregate(&Aggregate::new().by(["userId"])).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit, "{refused}");
    let ids: std::collections::HashSet<i64> = db.all::<i64>("select id from teachers").unwrap().into_iter().collect();
    let by_hand = || {
        let user = |event: crate::records::Event| event.fields.get::<i64>("userId").unwrap();
        let users = list.clone().all().filter_map(|event| user(event.unwrap()));
        users.filter(|id| ids.contains(id)).count() as u64
    };
    let none = || list.count().unwrap();
    let statement = || kept.count().unwrap();
    println!("{EVENTS} events of {USERS} people, a statement of {} ids, the median of 7 passes in turns", ids.len());
    timed_in_turns(
        7,
        &[
            ("a count with no where", &none),
            ("a count kept to the statement", &statement),
            ("every event walked, kept to the ids by the program", &by_hand),
        ],
    );
    assert_eq!(kept.count().unwrap(), EVENTS as u64 / 2);
}
