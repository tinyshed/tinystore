//! The query builder: every query of testdata/sql/queries.json, written as
//! the Bun SDK writes it, and tables over a real database.

use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use super::values::Value;
use super::{Direction, Sql, Table, Upsert};
use crate::{ErrorKind, Options, Store, TestClock, sql};

/// A row as the vectors give one: its columns in their order.
#[derive(Clone, Debug, Default, PartialEq)]
struct Ordered(Vec<(String, Json)>);

impl Serialize for Ordered {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, value) in &self.0 {
            map.serialize_entry(name, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Pairs;
        impl<'de> Visitor<'de> for Pairs {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a row")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, Json>()? {
                    pairs.push(pair);
                }
                Ok(Ordered(pairs))
            }
        }
        deserializer.deserialize_map(Pairs)
    }
}

fn ordered(pairs: &Json) -> Ordered {
    Ordered(
        pairs.as_array().unwrap().iter().map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].clone())).collect(),
    )
}

fn condition(given: &Json) -> Sql {
    let (kind, argument) = given.as_object().unwrap().iter().next().unwrap();
    match kind.as_str() {
        "equal" => {
            let mut names: Vec<&String> = argument.as_object().unwrap().keys().collect();
            names.sort();
            let pieces: Vec<Sql> = names.iter().map(|name| equal(name, &argument[name.as_str()])).collect();
            if pieces.len() == 1 { pieces.into_iter().next().unwrap() } else { sql::and(pieces) }
        }
        "sql" => {
            let parts = argument.as_array().unwrap();
            parts[1..].iter().fold(Sql::new(parts[0].as_str().unwrap()), |piece, value| piece.bind(value))
        }
        "or" => sql::or(argument.as_array().unwrap().iter().map(condition)),
        "and" => sql::and(argument.as_array().unwrap().iter().map(condition)),
        "has" => sql::has(argument[0].as_str().unwrap(), &argument[1]),
        other => panic!("no condition {other}"),
    }
}

fn equal(name: &str, value: &Json) -> Sql {
    match value.as_array() {
        Some(list) => sql::is_in(name, list),
        None => sql::eq(name, value),
    }
}

/// Equal values at the top of a query stand apart, as an object's do in the Bun SDK.
fn conditions(given: &Json) -> Vec<Sql> {
    match given.get("equal").and_then(Json::as_object) {
        Some(object) => {
            let mut names: Vec<&String> = object.keys().collect();
            names.sort();
            names.iter().map(|name| equal(name, &object[name.as_str()])).collect()
        }
        None => vec![condition(given)],
    }
}

fn on(given: &Json) -> Sql {
    given.as_str().map_or_else(|| condition(given), Sql::new)
}

type Rows<'r> = super::Query<'r, Ordered>;

fn apply<'r>(query: Rows<'r>, step: &Json) -> Rows<'r> {
    let (verb, argument) = step.as_object().unwrap().iter().next().unwrap();
    match verb.as_str() {
        "where" => conditions(argument).into_iter().fold(query, |query, piece| query.filter(piece)),
        "having" => conditions(argument).into_iter().fold(query, |query, piece| query.having(piece)),
        "select" => query.select(argument.as_str().unwrap()),
        "join" => query.join(argument[0].as_str().unwrap(), on(&argument[1])),
        "leftJoin" => query.left_join(argument[0].as_str().unwrap(), on(&argument[1])),
        "groupBy" => query.group_by(argument.as_array().unwrap().iter().map(|name| name.as_str().unwrap())),
        "orderBy" => {
            let direction = if argument[1] == "desc" { Direction::Desc } else { Direction::Asc };
            match argument[0].as_str() {
                Some(column) => query.order_by(column, direction),
                None => query.order_by_sql(condition(&argument[0]), direction),
            }
        }
        "withDeleted" => query.with_deleted(),
        "deletedOnly" => query.deleted_only(),
        "limit" => query.limit(argument.as_u64().unwrap()),
        "offset" => query.offset(argument.as_u64().unwrap()),
        "include" => {
            let table = Table::<Ordered>::recorded(argument[1]["table"].as_str().unwrap(), Arc::default());
            let included = argument[1]["steps"].as_array().unwrap().iter().fold((*table).clone(), apply);
            query.include(argument[0].as_str().unwrap(), &included)
        }
        other => panic!("no step {other}"),
    }
}

fn kept(value: &Value) -> Json {
    match value {
        Value::Null => Json::Null,
        Value::Integer(n) => Json::from(*n),
        Value::Real(x) => Json::from(*x),
        Value::Text(text) => Json::from(text.as_str()),
        Value::Blob(bytes) => Json::from(bytes.clone()),
    }
}

/// An upsert as a vector's options give it: an absent `update` names none.
fn upsert_of(options: &Json) -> Upsert {
    let update = options.get("update").and_then(Json::as_array).cloned().unwrap_or_default();
    let mut asked = Upsert::on([options["conflict"].as_str().unwrap()])
        .update(update.iter().map(|name| name.as_str().unwrap().to_owned()));
    if let Some(only_if) = options.get("where") {
        asked = asked.only_if(condition(only_if));
    }
    asked
}

/// Runs the call a vector ends in, which leaves its statement with `last`.
fn ran(
    vector: &Json,
    table: &Table<'_, Ordered>,
    query: &Rows<'_>,
    last: &Arc<Mutex<Option<Sql>>>,
) -> crate::Result<()> {
    if let Some(sets) = vector.get("update") {
        let sets = sets.as_array().unwrap().iter().map(|pair| {
            let value = &pair[1];
            let piece = if value.get("sql").is_some() { condition(value) } else { sql::value(value) };
            (pair[0].as_str().unwrap().to_owned(), piece)
        });
        query.update(sets.collect::<Vec<_>>()).map(drop)
    } else if vector.get("delete").is_some() {
        query.delete().map(drop)
    } else if let Some(rows) = vector.get("insert") {
        let rows: Vec<Ordered> = rows.as_array().unwrap().iter().map(ordered).collect();
        table.insert_all(&rows).map(drop)
    } else if let Some(upsert) = vector.get("upsert") {
        table.upsert(&ordered(&upsert[0]), upsert_of(&upsert[1])).map(drop)
    } else if let Some(upserts) = vector.get("upserts") {
        let rows: Vec<Ordered> = upserts[0].as_array().unwrap().iter().map(ordered).collect();
        table.upsert_all(&rows, upsert_of(&upserts[1])).map(drop)
    } else if vector.get("count").is_some() {
        query.count().map(drop)
    } else if vector.get("restore").is_some() {
        query.restore().map(drop)
    } else if vector.get("purge").is_some() {
        query.purge().map(drop)
    } else if let Some(page) = vector.get("list") {
        query.list(page["limit"].as_u64().unwrap(), None).map(drop)
    } else {
        query.to_sql().map(|statement| *last.lock().unwrap() = Some(statement))
    }
}

// contract: SQL-10
#[test]
fn every_query_of_the_vectors_is_the_statement_every_sdk_writes() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/sql/queries.json");
    let vectors: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for vector in vectors["queries"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let last = Arc::new(Mutex::new(None));
        let mut table = Table::<Ordered>::recorded(vector["table"].as_str().unwrap(), Arc::clone(&last));
        if let Some(column) = vector.pointer("/options/deleted").and_then(Json::as_str) {
            table = table.deleted(column);
        }
        if let Some(column) = vector.pointer("/options/version").and_then(Json::as_str) {
            table = table.version(column);
        }
        let steps = vector.get("steps").and_then(Json::as_array).cloned().unwrap_or_default();
        let query = steps.iter().fold((*table).clone(), apply);
        let statement = ran(vector, &table, &query, &last).map(|()| last.lock().unwrap().take().unwrap());
        if let Some(why) = vector.get("refused").and_then(Json::as_str) {
            let error = statement.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Invalid, "{name}: {error}");
            assert!(error.to_string().contains(why), "{name}: {error}");
            continue;
        }
        let statement = statement.unwrap();
        assert_eq!(statement.text(), vector["text"].as_str().unwrap(), "{name}");
        let values: Vec<Json> = statement.values().iter().map(kept).collect();
        assert_eq!(Json::from(values), vector["values"], "{name}");
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Note {
    id: String,
    author_id: i64,
    title: String,
    done: bool,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    views: Option<i64>,
}

fn note(id: &str, author_id: i64, title: &str) -> Note {
    Note { id: id.to_owned(), author_id, title: title.to_owned(), done: false, tags: Vec::new(), views: None }
}

struct Fixture {
    store: Store,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let options = Options { clock: Some(clock), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { _dir: dir, store }
}

const NOTES: &str = "create table notes (
    id        text primary key,
    author_id integer not null,
    title     text not null,
    done      integer not null default 0 check (done in (0, 1)),
    tags      text not null default '[]' check (json_valid(tags)),
    views     integer not null default 0
) strict";

#[test]
fn a_table_inserts_rows_and_reads_them_back_as_its_type() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    let first = Note { tags: vec!["home".to_owned()], ..note("n1", 5, "Buy milk") };
    assert_eq!(notes.insert(&first).unwrap(), Note { views: Some(0), ..first.clone() }, "the default filled");
    let more = notes.insert_all(&[note("n2", 5, "Call mom"), note("n3", 6, "Read")]).unwrap();
    assert_eq!(more.len(), 2);
    assert_eq!(notes.insert(&first).unwrap_err().kind(), ErrorKind::Conflict);

    assert_eq!(notes.filter(sql::eq("author_id", &5)).count().unwrap(), 2);
    let tagged = notes.filter(sql::has("tags", "home")).all().unwrap();
    assert_eq!(tagged.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["n1"]);
    let mine = notes.filter(sql::is_in("id", &["n1", "n3"])).order_by("id", Direction::Desc).all().unwrap();
    assert_eq!(mine.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["n3", "n1"]);
    let titles: Vec<String> = notes.select("title").order_by("title", Direction::Asc).limit(2).all().unwrap();
    assert_eq!(titles, ["Buy milk", "Call mom"]);
}

// contract: SQL-09
#[test]
fn an_update_or_a_delete_changes_what_its_condition_says_and_refuses_every_row() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    notes.insert_all(&[note("n1", 5, "a"), note("n2", 5, "b")]).unwrap();

    let one = notes.filter(sql::eq("id", "n1"));
    let changed = one.update([("done", sql::value(&true)), ("views", sql!("views + 1"))]).unwrap();
    assert_eq!(changed.changes, 1);
    let read = one.one().unwrap().unwrap();
    assert_eq!((read.done, read.views), (true, Some(1)));

    for refused in [notes.delete(), notes.filter(sql::and(Vec::<Sql>::new())).delete()] {
        assert_eq!(refused.unwrap_err().kind(), ErrorKind::Invalid);
    }
    let limited = notes.filter(sql::eq("author_id", &5)).limit(1).delete().unwrap_err();
    assert_eq!(limited.kind(), ErrorKind::Invalid, "a limit SQLite would not keep: {limited}");
    assert_eq!(notes.count().unwrap(), 2);
    assert_eq!(notes.filter(sql::eq("done", &true)).delete().unwrap().changes, 1);
    assert_eq!(notes.count().unwrap(), 1);
    let short = notes.filter(sql!("id = ? and author_id = ?", "n2")).all().unwrap_err();
    assert!(short.to_string().contains("2 placeholders and 1 values"), "{short}");
}

// contract: SQL-10
#[test]
fn a_comment_at_the_end_of_a_piece_hides_nothing_the_builder_writes_after_it() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    notes.insert_all(&[note("n1", 5, "a"), Note { done: true, ..note("n2", 5, "b") }, note("n3", 6, "c")]).unwrap();

    let ids = |rows: Vec<Note>| rows.into_iter().map(|row| row.id).collect::<Vec<_>>();
    let newest = notes.filter("author_id = 5 -- the first author").order_by("id", Direction::Desc).all();
    assert_eq!(ids(newest.unwrap()), ["n2", "n1"], "the order after the comment holds");
    let one = notes.filter("id = 'n1' -- the first note").update([("views", sql!("views + 1 -- one more"))]);
    assert_eq!(one.unwrap().changes, 1, "an update sets the rows its condition says, not every row");
    let done = notes.filter("author_id = 5 -- the first author").filter("done = 1").delete();
    assert_eq!(done.unwrap().changes, 1, "a delete keeps the condition after the comment");
    assert_eq!(ids(notes.order_by("id", Direction::Asc).all().unwrap()), ["n1", "n3"]);

    let open = notes.filter("done = 1 /* the done ones").all().unwrap_err();
    assert_eq!(open.kind(), ErrorKind::Invalid, "{open}");
    assert!(open.to_string().contains("ends inside a /* comment"), "{open}");
}

#[test]
fn an_upsert_sets_only_what_it_names_on_the_row_its_owner_holds() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    let owners = |author: i64| Upsert::on(["id"]).update(["title"]).only_if(sql::eq("author_id", &author));
    assert_eq!(notes.upsert(&note("u1", 1, "mine"), owners(1)).unwrap().unwrap().title, "mine");
    let changed = notes.upsert(&Note { done: true, ..note("u1", 1, "still mine") }, owners(1)).unwrap().unwrap();
    assert_eq!((changed.title.as_str(), changed.done), ("still mine", false), "only the title it named");
    assert!(notes.upsert(&note("u1", 2, "stolen"), owners(2)).unwrap().is_none(), "another's row stays as it is");
    assert_eq!(notes.filter(sql::eq("id", "u1")).one().unwrap().unwrap().title, "still mine");
}

// contract: SQL-15
#[test]
fn an_upsert_of_rows_writes_every_row_or_none_in_one_statement() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    let owners = |author: i64| Upsert::on(["id"]).update(["title"]).only_if(sql::eq("author_id", &author));
    notes.insert(&note("u1", 1, "mine")).unwrap();
    notes.insert(&note("u2", 2, "theirs")).unwrap();

    let rows = [note("u1", 1, "still mine"), note("u2", 1, "stolen"), note("u3", 1, "new")];
    let written = notes.upsert_all(&rows, owners(1)).unwrap();
    let titles: Vec<&str> = written.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(titles, ["still mine", "new"], "every row it wrote, not the one whose owner kept it");
    assert_eq!(notes.filter(sql::eq("id", "u2")).one().unwrap().unwrap().title, "theirs");

    let broken = [note("u4", 1, "a"), Note { done: true, ..note("u5", 1, "b") }];
    let refuse_u5 =
        "create trigger no_u5 before insert on notes when new.id = 'u5' begin select raise(abort, 'no'); end";
    db.exec(refuse_u5).unwrap();
    assert_eq!(notes.upsert_all(&broken, owners(1)).unwrap_err().kind(), ErrorKind::Invalid);
    assert!(notes.filter(sql::eq("id", "u4")).one().unwrap().is_none(), "one statement: none of its rows");

    // a key twice: the rows go in order, so the second sets what the first wrote, held or not
    let twice = [note("u6", 1, "a"), note("u6", 1, "b")];
    assert_eq!(notes.upsert_all(&twice, owners(1)).unwrap().len(), 2);
    let again = [note("u6", 1, "c"), note("u6", 1, "d")];
    assert_eq!(notes.upsert_all(&again, owners(1)).unwrap().len(), 2);
    assert_eq!(notes.filter(sql::eq("id", "u6")).one().unwrap().unwrap().title, "d", "the last row's title stays");
    assert!(notes.upsert_all(&[], owners(1)).unwrap().is_empty(), "no row writes nothing");
    let unnamed = notes.upsert_all(&[], Upsert::on(["id"])).unwrap_err();
    assert!(unnamed.to_string().contains("the columns its update sets"), "{unnamed}");
}

#[test]
fn a_page_reads_after_the_last_row_of_the_one_before_and_only_its_own_query() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    for n in 0..5 {
        notes.insert(&Note { views: Some(n / 2), ..note(&format!("p{n}"), 8, "x") }).unwrap();
    }
    let query =
        notes.filter(sql::eq("author_id", &8)).order_by("views", Direction::Desc).order_by("id", Direction::Desc);
    let first = query.list(2, None).unwrap();
    let second = query.list(2, first.next.as_deref()).unwrap();
    let third = query.list(2, second.next.as_deref()).unwrap();
    let ids: Vec<String> =
        [first.rows, second.rows, third.rows.clone()].concat().into_iter().map(|row| row.id).collect();
    assert_eq!(ids, ["p4", "p3", "p2", "p1", "p0"]);
    assert!(third.next.is_none());
    let other =
        notes.filter(sql::eq("author_id", &9)).order_by("views", Direction::Desc).order_by("id", Direction::Desc);
    assert_eq!(other.list(2, second.next.as_deref()).unwrap_err().kind(), ErrorKind::Invalid);
    // a cursor of this query with one order value of two is anyone's forgery
    let next = first.next.unwrap();
    let bytes: Vec<u8> =
        (0..next.len()).step_by(2).map(|at| u8::from_str_radix(&next[at..at + 2], 16).unwrap()).collect();
    let mut read: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    read["v"].as_array_mut().unwrap().truncate(1);
    let forged: String = serde_json::to_vec(&read).unwrap().iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(query.list(2, Some(&forged)).unwrap_err().kind(), ErrorKind::Invalid);
}

// contract: SQL-09
#[test]
fn a_page_refuses_a_limit_or_an_offset_its_list_would_drop() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
    let notes = db.table::<Note>("notes");
    for n in 0..3 {
        notes.insert(&note(&format!("p{n}"), 8, "x")).unwrap();
    }
    let by_id = notes.order_by("id", Direction::Asc);
    for query in [by_id.limit(1), by_id.offset(1), by_id.limit(1).offset(1)] {
        let error = query.list(2, None).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        assert!(error.to_string().contains("takes its own limit"), "{error}");
    }
    assert_eq!(by_id.list(2, None).unwrap().rows.len(), 2, "without them a page reads its own limit");
}

#[test]
fn pages_read_on_past_a_null_and_an_infinity_in_their_order() {
    let f = fixture();
    let orders = "create table orders (id integer primary key, shipped_at integer, total real)";
    let db = f.store.database("shop").migrations([("0001_orders.sql", orders)]).open().unwrap();
    db.batch([
        sql!("insert into orders values (1, null, 1.0), (2, null, ?), (3, null, ?)", f64::INFINITY, f64::NEG_INFINITY),
        sql!("insert into orders values (4, 10, 2.0), (5, 20, ?)", f64::INFINITY),
    ])
    .unwrap();
    #[derive(Debug, Deserialize)]
    struct Order {
        id: i64,
    }
    let orders = db.table::<Order>("orders");
    let paged = |query: super::Query<'_, Order>| {
        let mut ids = Vec::new();
        let mut after = None;
        loop {
            let page = query.list(2, after.as_deref()).unwrap();
            ids.extend(page.rows.iter().map(|order| order.id));
            match page.next {
                Some(next) => after = Some(next),
                None => return ids,
            }
        }
    };
    let ascending = orders.order_by("shipped_at", Direction::Asc).order_by("id", Direction::Asc);
    assert_eq!(paged(ascending), [1, 2, 3, 4, 5], "a page that ends among the nulls, first in ascending order");
    let descending = orders.order_by("shipped_at", Direction::Desc).order_by("id", Direction::Desc);
    assert_eq!(paged(descending), [5, 4, 3, 2, 1], "the nulls last in descending order");
    let by_total = orders.order_by("total", Direction::Asc).order_by("id", Direction::Asc);
    assert_eq!(paged(by_total), [3, 1, 4, 2, 5], "a page that ends on an infinity");
}

#[test]
fn a_join_reads_its_first_table_and_a_table_inside_a_transaction_rolls_back_with_it() {
    let f = fixture();
    let books = "create table authors (id integer primary key, name text not null, active integer not null) strict;
        create table books (id integer primary key, author_id integer not null, title text not null) strict;";
    let db = f.store.database("library").migrations([("0001_books.sql", books)]).open().unwrap();
    db.batch([
        "insert into authors (id, name, active) values (1, 'Le Guin', 1), (2, 'Lem', 0)",
        "insert into books (id, author_id, title) values (1, 1, 'The Dispossessed'), (2, 2, 'Solaris')",
    ])
    .unwrap();
    #[derive(Debug, PartialEq, Deserialize)]
    struct Book {
        id: i64,
        title: String,
    }
    let read =
        db.table::<Book>("books as b").join("authors as a", "a.id = b.author_id").filter(sql::eq("a.active", &1)).all();
    assert_eq!(read.unwrap(), [Book { id: 1, title: "The Dispossessed".to_owned() }]);
    assert!(!db.table::<Book>("books").filter(sql::eq("author_id", &1)).explain().unwrap().is_empty());

    let refused = db.tx(|tx| -> crate::Result<()> {
        tx.exec("insert into books (id, author_id, title) values (3, 1, 'Lathe')")?;
        assert_eq!(tx.table::<Book>("books").count()?, 3, "the transaction's table sees its write");
        Err(crate::Error::invalid("changed my mind"))
    });
    assert!(refused.is_err());
    assert_eq!(db.table::<Book>("books").count().unwrap(), 2, "rolled back");
}

const AUTHORS: &str = "create table authors (id integer primary key, name text not null) strict;
create table posts (
    id        integer primary key,
    author_id integer not null references authors (id),
    title     text not null,
    score     real not null,
    raw       blob,
    done      integer not null default 0 check (done in (0, 1))
) strict";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Post {
    id: i64,
    title: String,
    score: f64,
    raw: Option<Vec<u8>>,
    done: bool,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Author {
    id: i64,
    name: String,
    posts: Vec<Post>,
}

/// An integer a float cannot hold.
const WIDE: i64 = (1 << 60) + 1;

fn authors_and_posts(db: &super::Database) -> Vec<Post> {
    db.exec(sql!("insert into authors (id, name) values (1, 'Ann'), (2, 'Bob'), (3, 'Cy')")).unwrap();
    let posts = vec![
        Post { id: 1, title: "first".to_owned(), score: 1.0, raw: None, done: false },
        Post {
            id: 2,
            title: "it's, (odd)\0 text".to_owned(),
            score: 0.1 + 0.2,
            raw: Some(vec![0, 255, 39]),
            done: true,
        },
        Post { id: WIDE, title: String::new(), score: 1e308, raw: None, done: false },
        Post { id: 4, title: "Bob's".to_owned(), score: -2.5, raw: None, done: false },
    ];
    for (post, author) in posts.iter().zip([1, 1, 1, 2]) {
        let insert = "insert into posts (id, author_id, title, score, raw, done) values (?, ?, ?, ?, ?, ?)";
        db.exec(sql!(insert, post.id, author, &post.title, post.score, &post.raw, post.done)).unwrap();
    }
    posts
}

#[test]
fn an_included_query_gives_each_row_its_rows_every_value_as_it_is_kept() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_authors.sql", AUTHORS)]).open().unwrap();
    let posts = authors_and_posts(&db);
    let latest = db
        .table::<Post>("posts as p")
        .select::<Post>("p.id, p.title, p.score, p.raw, p.done")
        .filter("p.author_id = a.id")
        .order_by("p.id", Direction::Desc)
        .limit(2);
    let authors = db.table::<Author>("authors as a").include("posts", &latest).order_by("a.id", Direction::Asc);

    let read = authors.all().unwrap();
    assert_eq!(read.iter().map(|author| author.name.as_str()).collect::<Vec<_>>(), ["Ann", "Bob", "Cy"]);
    assert_eq!(read[0].posts, [posts[2].clone(), posts[1].clone()], "the two latest, each value as it is kept");
    assert_eq!(read[1].posts, [posts[3].clone()]);
    assert!(read[2].posts.is_empty(), "a row with none has an empty list");

    let bob = authors.filter(sql::eq("a.id", &2)).one().unwrap().unwrap();
    assert_eq!((bob.id, bob.posts.len()), (2, 1));
    assert_eq!(authors.count().unwrap(), 3, "a count reads no included rows");
    let inside = db.tx(|tx| tx.table::<Author>("authors as a").include("posts", &latest).all()).unwrap();
    assert_eq!(inside.len(), 3);
}

#[test]
fn an_included_query_names_its_columns_and_its_limit_and_includes_nothing() {
    let f = fixture();
    let db = f.store.database("app").migrations([("0001_authors.sql", AUTHORS)]).open().unwrap();
    let posts = db.table::<Post>("posts as p").filter("p.author_id = a.id");
    let named = posts.select::<Post>("p.id, p.title");
    let authors = db.table::<Author>("authors as a");
    let refused = [
        (authors.include("posts", &posts.limit(2)), "names its columns"),
        (authors.include("posts", &named), "needs a limit"),
        (authors.include("posts", &named.limit(2).include("more", &named.limit(1))), "includes nothing itself"),
        (authors.include("posts", &named.limit(2)).include("posts", &named.limit(2)), "a name of its own"),
        (authors.include("posts", &posts.select::<Post>("p.id, upper(p.title)").limit(2)), "upper(p.title)"),
    ];
    for (query, why) in refused {
        let error = query.all().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
        assert!(error.to_string().contains(why), "{error}");
    }
}

const TASKS: &str = "create table tasks (
    id         integer primary key,
    title      text not null,
    deleted_at integer,
    version    integer not null default 0
) strict";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Task {
    id: i64,
    title: String,
    deleted_at: Option<i64>,
    version: i64,
}

fn task(id: i64, title: &str) -> Task {
    Task { id, title: title.to_owned(), deleted_at: None, version: 0 }
}

/// The fixture's clock, as a deleted column keeps it.
const NOW_MILLIS: i64 = 1_791_547_200_000;

fn tasks_db(f: &Fixture) -> super::Database {
    f.store.database("sp").migrations([("0001_tasks.sql", TASKS)]).open().unwrap()
}

fn titles(rows: Vec<Task>) -> Vec<String> {
    rows.into_iter().map(|row| row.title).collect()
}

// contract: SQL-21, SQL-23, SQL-24
#[test]
fn a_second_writer_from_the_old_version_is_refused_and_a_deleted_row_is_gone_from_the_handle() {
    let f = fixture();
    let db = tasks_db(&f);
    let tasks = db.table::<Task>("tasks").deleted("deleted_at").version("version");
    tasks.insert(&task(1, "first")).unwrap();
    let one = tasks.filter(sql::eq("id", &1));
    let seen = one.one().unwrap().unwrap().version;

    let a = one.if_version(seen).update([("title", sql::value("from a"))]).unwrap();
    assert_eq!(a.changes, 1);
    let b = one.if_version(seen).update([("title", sql::value("from b"))]).unwrap_err();
    assert_eq!(b.kind(), ErrorKind::Conflict, "{b}");
    assert_eq!(b.to_string(), "sql sp: table tasks: the row is at version 1, not 0: conflict");
    assert_eq!(one.one().unwrap().unwrap().title, "from a", "b wrote nothing");

    assert_eq!(one.delete().unwrap().changes, 1);
    assert_eq!(tasks.count().unwrap(), 0, "a count of the live rows");
    assert!(one.one().unwrap().is_none());
    let marked = tasks.with_deleted().filter(sql::eq("id", &1)).one().unwrap().unwrap();
    assert_eq!(
        (marked.deleted_at, marked.version),
        (Some(NOW_MILLIS), 2),
        "the store's clock, and a delete is an edit"
    );
    assert_eq!(one.restore().unwrap().changes, 1);
    assert_eq!(one.one().unwrap().unwrap().version, 3, "a restore is an edit too");
    let plain: i64 = db.scalar("select count(*) from tasks").unwrap();
    assert_eq!(plain, 1, "a statement by hand is not rewritten");
}

// contract: SQL-21, SQL-22
#[test]
fn every_call_of_a_handle_with_a_deleted_column_passes_the_marked_rows_by_unless_asked() {
    let f = fixture();
    let db = tasks_db(&f);
    let tasks = db.table::<Task>("tasks").deleted("deleted_at");
    tasks.insert_all(&[task(1, "a"), task(2, "b"), task(3, "c")]).unwrap();
    assert_eq!(tasks.filter(sql::eq("id", &2)).delete().unwrap().changes, 1);
    assert_eq!(tasks.filter(sql::eq("id", &2)).delete().unwrap().changes, 0, "a marked row is marked once");

    let by_id = tasks.order_by("id", Direction::Asc);
    assert_eq!(titles(by_id.all().unwrap()), ["a", "c"]);
    assert_eq!(titles(by_id.list(10, None).unwrap().rows), ["a", "c"]);
    assert_eq!(titles(by_id.each().unwrap().map(Result::unwrap).collect()), ["a", "c"]);
    assert_eq!(tasks.count().unwrap(), 2);
    assert!(!tasks.filter(sql::eq("id", &1)).explain().unwrap().is_empty());
    let any = sql!("id > ?", 0);
    assert_eq!(tasks.filter(any.clone()).update([("title", sql::value("x"))]).unwrap().changes, 2);
    assert_eq!(tasks.filter(sql::eq("id", &2)).update([("title", sql::value("x"))]).unwrap().changes, 0);
    assert_eq!(titles(tasks.deleted_only().all().unwrap()), ["b"], "the marked row kept its title");
    assert_eq!(tasks.with_deleted().count().unwrap(), 3);

    let live = tasks.filter(any.clone()).purge().unwrap_err();
    assert_eq!(live.kind(), ErrorKind::Invalid, "{live}");
    assert!(live.to_string().starts_with("sql sp: table tasks: a purge deletes for good"), "{live}");
    assert_eq!(tasks.deleted_only().filter(any.clone()).purge().unwrap().changes, 1);
    assert_eq!(tasks.with_deleted().count().unwrap(), 2, "the purge took the marked row alone");
    tasks.filter(sql::eq("id", &3)).delete().unwrap();
    assert_eq!(tasks.with_deleted().filter(any).purge().unwrap().changes, 2, "live and marked");

    let plain = db.table::<Task>("tasks");
    let unmarked = plain.with_deleted().all().unwrap_err();
    assert!(unmarked.to_string().contains("only a table with a deleted column"), "{unmarked}");
    assert_eq!(plain.filter(sql::eq("id", &1)).restore().unwrap_err().kind(), ErrorKind::Invalid);
}

// contract: SQL-24
#[test]
fn an_upsert_onto_a_marked_row_brings_it_back_and_raises_its_version() {
    let f = fixture();
    let db = tasks_db(&f);
    let tasks = db.table::<Task>("tasks").deleted("deleted_at").version("version");
    tasks.insert(&task(1, "a")).unwrap();
    tasks.filter(sql::eq("id", &1)).delete().unwrap();
    let back = tasks.upsert(&task(1, "back"), Upsert::on(["id"]).update(["title"])).unwrap().unwrap();
    assert_eq!((back.title.as_str(), back.deleted_at, back.version), ("back", None, 2));
    assert_eq!(tasks.count().unwrap(), 1);
    let refused = tasks.upsert(&task(1, "v"), Upsert::on(["id"]).update(["title", "version"])).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    let set = tasks.filter(sql::eq("id", &1)).update([("version", sql::value(&9))]).unwrap_err();
    assert!(set.to_string().contains("the version column is the handle's to raise"), "{set}");
}

// contract: SQL-23
#[test]
fn an_edit_with_if_version_writes_every_row_at_its_version_or_none() {
    let f = fixture();
    let db = tasks_db(&f);
    let tasks = db.table::<Task>("tasks").version("version");
    tasks.insert_all(&[task(1, "a"), task(2, "b")]).unwrap();
    tasks.filter(sql::eq("id", &2)).update([("title", sql::value("b2"))]).unwrap();
    let both = tasks.filter(sql::is_in("id", &[1, 2]));
    let refused = both.if_version(0).update([("title", sql::value("z"))]).unwrap_err();
    assert_eq!(refused.to_string(), "sql sp: table tasks: the row is at version 1, not 0: conflict");
    let read = tasks.order_by("id", Direction::Asc).all().unwrap();
    assert_eq!((titles(read.clone()), read[0].version), (vec!["a".to_owned(), "b2".to_owned()], 0), "none");
    tasks.filter(sql::eq("id", &1)).update([("title", sql::value("a2"))]).unwrap();
    assert_eq!(both.if_version(1).update([("title", sql::value("z"))]).unwrap().changes, 2, "all of them");

    assert_eq!(tasks.filter(sql::eq("id", &9)).if_version(0).update([("title", sql::value("z"))]).unwrap().changes, 0);
    let unversioned =
        db.table::<Task>("tasks").filter(sql::eq("id", &1)).if_version(0).update([("title", sql::value("z"))]);
    assert_eq!(unversioned.unwrap_err().kind(), ErrorKind::Invalid);
    let hard = tasks.filter(sql::eq("id", &1)).if_version(2).delete().unwrap_err();
    assert!(hard.to_string().contains("if_version on a delete is a mark's"), "{hard}");
}

// contract: SQL-23
#[test]
fn an_edit_with_if_version_inside_a_transaction_is_the_transactions() {
    let f = fixture();
    let db = tasks_db(&f);
    db.table::<Task>("tasks").insert(&task(1, "a")).unwrap();
    let ran = db.tx(|tx| -> crate::Result<()> {
        let tasks = tx.table::<Task>("tasks").deleted("deleted_at").version("version");
        let one = tasks.filter(sql::eq("id", &1));
        assert_eq!(one.if_version(0).update([("title", sql::value("in tx"))])?.changes, 1);
        let again = one.if_version(0).delete().unwrap_err();
        assert_eq!(again.kind(), ErrorKind::Conflict, "{again}");
        assert_eq!(one.if_version(1).delete()?.changes, 1, "the transaction goes on after a conflict");
        Ok(())
    });
    ran.unwrap();
    let kept = db.table::<Task>("tasks").deleted("deleted_at").with_deleted().all().unwrap();
    assert_eq!((kept[0].title.as_str(), kept[0].version, kept[0].deleted_at), ("in tx", 2, Some(NOW_MILLIS)));
}

#[test]
fn a_plain_handle_writes_the_statements_it_wrote_before_tables_kept_columns() {
    let last = Arc::new(Mutex::new(None));
    let table = Table::<Note>::recorded("notes", Arc::clone(&last));
    let sent = |ran: crate::Result<super::Done>| {
        ran.unwrap();
        last.lock().unwrap().take().map(|statement: Sql| statement.text().to_owned()).unwrap()
    };
    let one = table.filter(sql::eq("id", "n1"));
    assert_eq!(sent(one.update([("title", sql::value("t"))])), "update \"notes\" set \"title\" = ? where \"id\" = ?");
    assert_eq!(sent(one.delete()), "delete from \"notes\" where \"id\" = ?");
    assert_eq!(one.to_sql().unwrap().text(), "select * from \"notes\" where \"id\" = ?");
    let upsert = table.upsert(&note("n1", 1, "t"), Upsert::on(["id"]).update(["title"]));
    assert!(upsert.unwrap().is_none());
    let text = last.lock().unwrap().take().unwrap().text().to_owned();
    assert!(text.ends_with("on conflict (\"id\") do update set \"title\" = excluded.\"title\" returning *"), "{text}");
}

/// The migrations docs/guides/sql/deleted-and-versions.md shows, by the name
/// each block's first line gives it, so that the guide's recipe is the one
/// tested here.
fn guide_migrations() -> Vec<(String, String)> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/guides/sql/deleted-and-versions.md");
    let page = std::fs::read_to_string(path).unwrap();
    let blocks = page.split("```sql\n").skip(1).map(|rest| rest.split("```").next().unwrap());
    let named = blocks.filter_map(|block| {
        let name = block.lines().next()?.strip_prefix("-- ")?;
        name.ends_with(".sql").then(|| (name.to_owned(), block.to_owned()))
    });
    named.collect()
}

#[derive(Debug, Deserialize)]
struct Past {
    version: i64,
    title: String,
}

#[test]
fn the_guides_history_keeps_each_old_version_and_its_last_fifty() {
    let f = fixture();
    let migrations = guide_migrations();
    let names: Vec<&str> = migrations.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["0001_tasks.sql", "0002_tasks_history.sql", "0003_tasks_history_merge.sql"]);
    let db = f.store.database("history").migrations(migrations[..2].to_vec()).open().unwrap();
    let tasks = db.table::<Task>("tasks").deleted("deleted_at").version("version");
    tasks.insert(&task(1, "v0")).unwrap();
    let one = tasks.filter(sql::eq("id", &1));
    for edit in 1..=3 {
        one.update([("title", sql::value(&format!("v{edit}")))]).unwrap();
    }
    let read = "select version, title from tasks_history where id = 1 order by version";
    let past: Vec<Past> = db.all(read).unwrap();
    let kept: Vec<(i64, &str)> = past.iter().map(|each| (each.version, each.title.as_str())).collect();
    assert_eq!(kept, [(0, "v0"), (1, "v1"), (2, "v2")], "each old row with its version");
    one.delete().unwrap();
    assert_eq!(db.all::<Past>(read).unwrap().len(), 4, "a delete is an edit, and leaves its entry");

    for edit in 0..60 {
        tasks.with_deleted().filter(sql::eq("id", &1)).update([("title", sql::value(&format!("w{edit}")))]).unwrap();
    }
    let past: Vec<Past> = db.all(read).unwrap();
    assert_eq!((past.len(), past[0].version), (49, 15), "the last 50 versions, the row's own among them");
}

#[test]
fn the_guides_history_merges_one_persons_edits_within_ten_minutes() {
    let f = fixture();
    let db = f.store.database("history").migrations(guide_migrations()).open().unwrap();
    db.exec("insert into tasks (id, title) values (1, 'v0')").unwrap();
    let tasks = db.table::<Task>("tasks").version("version");
    let edit = |title: &str, by: &str, at: i64| {
        let sets = [("title", sql::value(title)), ("edited_by", sql::value(by)), ("edited_at", sql::value(&at))];
        tasks.filter(sql::eq("id", &1)).update(sets).unwrap();
    };
    edit("ann 1", "ann", 0);
    edit("ann 2", "ann", 60_000);
    edit("ann 3", "ann", 120_000);
    edit("ben 1", "ben", 180_000);
    edit("ben 2", "ben", 180_000 + 600_000);
    let past: Vec<Past> = db.all("select version, title from tasks_history where id = 1 order by version").unwrap();
    let titles: Vec<&str> = past.iter().map(|each| each.title.as_str()).collect();
    assert_eq!(titles, ["v0", "ann 3", "ben 1"], "ann's burst is one entry; ben's edits ten minutes apart are two");
}
