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
            query.order_by(argument[0].as_str().unwrap(), direction)
        }
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

#[test]
fn every_query_of_the_vectors_is_the_statement_every_sdk_writes() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/sql/queries.json");
    let vectors: Json = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for vector in vectors["queries"].as_array().unwrap() {
        let name = vector["name"].as_str().unwrap();
        let last = Arc::new(Mutex::new(None));
        let table = Table::<Ordered>::recorded(vector["table"].as_str().unwrap(), Arc::clone(&last));
        let steps = vector.get("steps").and_then(Json::as_array).cloned().unwrap_or_default();
        let query = steps.iter().fold((*table).clone(), apply);
        let statement = if let Some(sets) = vector.get("update") {
            let sets = sets.as_array().unwrap().iter().map(|pair| {
                let value = &pair[1];
                let piece = if value.get("sql").is_some() { condition(value) } else { sql::value(value) };
                (pair[0].as_str().unwrap().to_owned(), piece)
            });
            query.update(sets.collect::<Vec<_>>()).unwrap();
            last.lock().unwrap().take().unwrap()
        } else if vector.get("delete").is_some() {
            query.delete().unwrap();
            last.lock().unwrap().take().unwrap()
        } else if let Some(rows) = vector.get("insert") {
            let rows: Vec<Ordered> = rows.as_array().unwrap().iter().map(ordered).collect();
            table.insert_all(&rows).unwrap();
            last.lock().unwrap().take().unwrap()
        } else if let Some(upsert) = vector.get("upsert") {
            let options = &upsert[1];
            let update = options["update"].as_array().unwrap().iter().map(|name| name.as_str().unwrap().to_owned());
            let mut asked = Upsert::on([options["conflict"].as_str().unwrap()]).update(update);
            if let Some(only_if) = options.get("where") {
                asked = asked.only_if(condition(only_if));
            }
            table.upsert(&ordered(&upsert[0]), asked).unwrap();
            last.lock().unwrap().take().unwrap()
        } else if vector.get("count").is_some() {
            query.count().unwrap();
            last.lock().unwrap().take().unwrap()
        } else {
            query.to_sql().unwrap()
        };
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
    _dir: tempfile::TempDir,
    store: Store,
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
    assert_eq!(notes.filter(sql::eq("done", &true)).delete().unwrap().changes, 1);
    assert_eq!(notes.count().unwrap(), 1);
    let short = notes.filter(sql!("id = ? and author_id = ?", "n2")).all().unwrap_err();
    assert!(
        short.to_string().contains("1 placeholders and 0 values")
            || short.to_string().contains("2 placeholders and 1 values"),
        "{short}"
    );
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
