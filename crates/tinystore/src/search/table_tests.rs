//! A table's index: every way a row changes reaches it in the commit that
//! makes it, a write that rolls back leaves neither, one from beside the
//! store reaches it at the store's next write, and a search answers what an
//! index of the table's rows made afresh would.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::TableIndex;
use crate::search::{self, Status, eq};
use crate::sql::Database;
use crate::{Error, ErrorKind, Options, Store, sql};

const ARTICLES: &str = "create table articles (
    id        integer primary key,
    title     text    not null,
    body      text    not null default '',
    category  text,
    done      integer not null default 0,
    views     integer not null default 0,
    embedding blob
)";

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Article {
    id: i64,
    title: String,
    body: String,
    category: Option<String>,
    done: bool,
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    db: Database,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").migrations([("0001_articles.sql", ARTICLES)]).open().unwrap();
    Fixture { dir, store, db }
}

impl Fixture {
    fn articles(&self) -> TableIndex<Article> {
        let opened = self.db.search_index::<Article>("articles_search").table("articles");
        let opened = opened.field("title", 2.0).field("body", 1.0).filters(["category", "done"]).open().unwrap();
        self.ready(&opened);
        opened
    }

    fn ready<T: serde::de::DeserializeOwned>(&self, index: &TableIndex<T>) {
        search::maintain(&self.store).unwrap();
        index.ready().unwrap();
    }

    fn insert(&self, id: i64, title: &str, body: &str) {
        self.db.exec(sql!("insert into articles (id, title, body) values (?, ?, ?)", id, title, body)).unwrap();
    }

    /// A connection of another process's, beside the store.
    fn beside(&self) -> rusqlite::Connection {
        let beside = rusqlite::Connection::open(self.dir.path().join("sql").join("app.db")).unwrap();
        beside.busy_timeout(Duration::from_secs(5)).unwrap();
        beside
    }
}

fn ids<T>(found: &search::Found<T>) -> Vec<&str> {
    found.hits.iter().map(|hit| hit.id.as_str()).collect()
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

// contract: SRCH-07
#[test]
fn every_way_a_row_changes_reaches_its_index_and_a_hit_is_the_row() {
    let f = fixture();
    f.insert(1, "Sourdough bread", "flour, water and salt");
    let articles = f.articles();
    let found = articles.search("bread").run().unwrap();
    assert_eq!(ids(&found), ["1"], "a row the table held before the index read in the background");
    assert_eq!(found.hits[0].value.title, "Sourdough bread", "a hit's value is its row");
    f.insert(2, "Rye", "bread from rye flour");
    assert_eq!(ids(&articles.search("bread").run().unwrap()), ["1", "2"], "the next search sees an insert");
    f.db.exec(sql!("update articles set title = 'Butter' where id = 1")).unwrap();
    assert!(articles.search("sourdough").run().unwrap().hits.is_empty(), "an update takes the old words out");
    assert_eq!(ids(&articles.search("butter").run().unwrap()), ["1"]);
    f.db.batch([
        sql!("insert into articles (id, title) values (3, 'Milk')"),
        sql!("delete from articles where id = 2"),
    ])
    .unwrap();
    f.db.tx(|tx| -> crate::Result<()> {
        tx.exec(sql!("insert into articles (id, title, category) values (4, 'Cream', 'dairy')"))?;
        tx.exec(sql!("update articles set done = 1, category = 'dairy' where id = 3"))?;
        Ok(())
    })
    .unwrap();
    assert!(articles.search("rye").run().unwrap().hits.is_empty(), "a delete in a batch");
    assert_eq!(ids(&articles.search("milk cream").run().unwrap()).len(), 2, "rows of a transaction");
    let done = articles.search("milk cream").filter(eq("done", &true)).run().unwrap();
    assert_eq!(ids(&done), ["3"], "a boolean passes the integer SQLite keeps it as");
    let dairy = articles.search("milk cream").filter(eq("category", "dairy")).run().unwrap();
    assert_eq!(dairy.hits.len(), 2);
    assert!(dairy.hits.iter().all(|hit| hit.value.category.as_deref() == Some("dairy")));
}

// contract: SRCH-07
#[test]
fn a_search_in_the_transaction_that_wrote_sees_its_writes() {
    let f = fixture();
    let articles = f.articles();
    let found =
        f.db.tx(|tx| -> crate::Result<Vec<String>> {
            tx.exec(sql!("insert into articles (id, title) values (1, 'inside')"))?;
            let found = tx.with(&articles).search("inside").run()?;
            Ok(found.hits.into_iter().map(|hit| hit.id).collect())
        })
        .unwrap();
    assert_eq!(found, ["1"]);
    let around = f.db.tx(|_| articles.search("inside").run().map(drop)).unwrap_err();
    assert_eq!(around.kind(), ErrorKind::Invalid, "a search beside the transaction would not see its writes");
}

// contract: SRCH-07
#[test]
fn a_search_in_a_transaction_goes_on_while_a_switch_of_its_index_waits_for_that_transaction() {
    let f = fixture();
    let titles = f.db.search_index::<Article>("articles_search").table("articles").field("title", 1.0).open().unwrap();
    f.ready(&titles);
    f.insert(1, "bread", "salt");
    let both = f.db.search_index::<Article>("articles_search").table("articles");
    let both = both.field("title", 1.0).field("body", 1.0).open().unwrap();
    assert!(matches!(both.status().unwrap(), Status::Rebuilding { .. }));
    let (answer, answered) = std::sync::mpsc::channel();
    // A thread of its own, which takes the store along, so that a deadlock
    // fails the test rather than hanging it in the store's close.
    std::thread::spawn(move || {
        let f = f;
        let searched = f.db.tx(|tx| -> crate::Result<_> {
            tx.exec(sql!("insert into articles (id, title) values (2, 'fresh bread')"))?;
            let (search, open) = (Arc::clone(&both.inner().search), Arc::clone(&both.inner().open));
            let switch = std::thread::spawn(move || search.rebuild(&open));
            while !both.inner().open.changing() {
                assert!(!switch.is_finished(), "the switch ended before it held the index");
                std::thread::yield_now();
            }
            let found = tx.with(&both).search("fresh").run()?;
            Ok((ids(&found).join(" "), switch))
        });
        let searched = searched.and_then(|(found, switch)| switch.join().unwrap().map(|()| found));
        let switched = both.search("salt").run().map(|found| ids(&found).join(" "));
        answer.send((searched, switched)).unwrap();
    });
    let waited = "the search waited for the switch, which waited for the search's transaction";
    let (searched, switched) = answered.recv_timeout(Duration::from_secs(30)).expect(waited);
    assert_eq!(searched.unwrap(), "2", "the search saw its transaction's write");
    assert_eq!(switched.unwrap(), "1", "the switch went on once the transaction committed");
}

// contract: SRCH-07
#[test]
fn a_search_in_a_transaction_that_rolls_back_leaves_later_searches_what_committed() {
    let f = fixture();
    let articles = f.articles();
    f.insert(1, "bread", "");
    f.insert(2, "bread", "");
    search::maintain(&f.store).unwrap();
    let rolled: crate::Result<()> = f.db.tx(|tx| {
        tx.exec(sql!("update articles set title = 'bread zebrafish' where id = 1"))?;
        let found = tx.with(&articles).search("bread zebrafisj").run()?;
        assert_eq!(found.corrected.as_deref(), Some("bread zebrafish"), "the transaction's own word");
        Err(Error::invalid("changed its mind"))
    });
    assert!(rolled.is_err());
    // The rows' counts come back as they were, and these writes count them on again.
    f.db.exec(sql!("delete from articles where id = 2")).unwrap();
    f.insert(3, "quokkas", "");
    let corrected = articles.search("quokkaz").run().unwrap();
    assert_eq!(corrected.corrected.as_deref(), Some("quokkas"), "a word committed after the rollback");
    assert_eq!(ids(&articles.search("bread").run().unwrap()), ["1"], "the row the rollback kept, not the one deleted");
}

// contract: ERR-06
#[test]
fn a_drop_refused_inside_a_transaction_leaves_the_index_as_it_was() {
    let f = fixture();
    let articles = f.articles();
    f.insert(1, "bread", "");
    let refused = f.db.tx(|_| f.db.drop_search_index("articles_search")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert_eq!(ids(&articles.search("bread").run().unwrap()), ["1"], "a handle searches as before");
    f.insert(2, "rye", "");
    assert_eq!(ids(&articles.search("rye").run().unwrap()), ["2"], "the triggers follow the table as before");
    assert!(f.db.drop_search_index("articles_search").unwrap());
}

// contract: SRCH-07
#[test]
fn a_write_that_rolls_back_changes_neither_the_table_nor_its_index() {
    let f = fixture();
    let articles = f.articles();
    f.insert(1, "kept", "");
    let rolled: crate::Result<()> = f.db.tx(|tx| {
        tx.exec(sql!("insert into articles (id, title) values (2, 'ghost words')"))?;
        tx.exec(sql!("update articles set title = 'gone' where id = 1"))?;
        Err(Error::invalid("changed its mind"))
    });
    assert!(rolled.is_err());
    assert!(articles.search("ghost gone").run().unwrap().hits.is_empty());
    assert_eq!(ids(&articles.search("kept").run().unwrap()), ["1"]);
    let refused = f.db.exec(sql!("insert into articles (id, title) values (1, 'twice')")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Conflict);
    assert!(articles.search("twice").run().unwrap().hits.is_empty(), "a write that failed in its savepoint");
}

#[test]
fn a_row_whose_vector_does_not_read_fails_its_write_and_one_beside_the_store_is_left_out() {
    let f = fixture();
    let opened = f.db.search_index::<Article>("by_meaning").table("articles").field("title", 1.0);
    let index = opened.vectors("embedding", 3).open().unwrap();
    f.ready(&index);
    let insert = |id: i64, title: &str, embedding: Option<Vec<u8>>| {
        f.db.exec(sql!("insert into articles (id, title, embedding) values (?, ?, ?)", id, title, embedding))
    };
    insert(1, "red shoes", Some(floats(&[1.0, 0.0, 0.0]))).unwrap();
    insert(2, "blue shoes", None).unwrap();
    let ragged = insert(3, "ragged", Some(vec![0; 5])).unwrap_err();
    assert_eq!(ragged.kind(), ErrorKind::Invalid);
    assert!(ragged.to_string().contains("embedding") && ragged.to_string().contains("row 3"), "{ragged}");
    assert_eq!(insert(4, "not a number", Some(floats(&[f32::NAN, 0.0, 0.0]))).unwrap_err().kind(), ErrorKind::Invalid);
    let held: i64 = f.db.scalar(sql!("select count(*) from articles where id in (3, 4)")).unwrap();
    assert_eq!(held, 0, "the rows failed with their vectors");
    assert_eq!(ids(&index.search_vector(&[1.0, 0.1, 0.0]).run().unwrap()), ["1"], "a row of none is not found by one");
    assert_eq!(ids(&index.search("shoes").run().unwrap()), ["1", "2"]);
    f.beside().execute("insert into articles (id, title, embedding) values (5, 'beside', x'00')", []).unwrap();
    insert(6, "after", None).unwrap();
    assert!(index.search("beside").run().unwrap().hits.is_empty(), "left out, and the store's write went on");
    assert_eq!(ids(&index.search("after").run().unwrap()), ["6"]);
}

// contract: SRCH-08
#[test]
fn a_write_from_beside_the_store_reaches_the_index_at_the_store_s_next_write() {
    let f = fixture();
    let articles = f.articles();
    f.insert(1, "first", "");
    let beside = f.beside();
    beside.execute("insert into articles (id, title) values (2, 'outside words')", []).unwrap();
    beside.execute("update articles set title = 'changed outside' where id = 1", []).unwrap();
    assert!(articles.search("outside").run().unwrap().hits.is_empty(), "waiting among the pending keys");
    f.db.exec(sql!("update articles set views = views + 1 where id = 2")).unwrap();
    let found = articles.search("outside").run().unwrap();
    assert_eq!(ids(&found), ["2", "1"], "equal scores, in the order the keys were left");
    assert!(articles.search("first").run().unwrap().hits.is_empty());
}

// contract: SRCH-08
#[test]
fn keys_left_from_beside_the_store_for_a_table_it_then_renamed_hold_up_no_write_and_no_drop() {
    let f = fixture();
    let articles = f.articles();
    f.insert(1, "bread", "");
    f.beside().execute_batch("delete from articles where id = 1; alter table articles rename to posts;").unwrap();
    f.db.exec(sql!("create table notes (id integer primary key, body text)")).unwrap();
    f.db.exec(sql!("insert into notes (body) values ('after')")).unwrap();
    let renamed = f.db.exec(sql!("insert into posts (id, title) values (2, 'rye')")).unwrap_err();
    assert_eq!(renamed.kind(), ErrorKind::Invalid, "{renamed}");
    assert!(renamed.to_string().contains("index articles_search"), "{renamed}");
    assert!(f.db.drop_search_index("articles_search").unwrap());
    let pending: i64 = f.db.scalar(sql!("select count(*) from _tinystore_search_pending")).unwrap();
    assert_eq!(pending, 0, "the drop took its keys with it");
    f.db.exec(sql!("insert into posts (id, title) values (2, 'rye')")).unwrap();
    assert_eq!(articles.search("bread").run().unwrap_err().kind(), ErrorKind::Invalid);
}

// contract: SRCH-08
#[test]
fn a_row_written_while_no_process_had_its_index_open_reaches_it_at_its_next_open() {
    let f = fixture();
    drop(f.articles());
    f.store.close().unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").open().unwrap();
    db.exec(sql!("insert into articles (id, title) values (1, 'while closed')")).unwrap();
    let articles = db.search_index::<Article>("articles_search").table("articles");
    let articles = articles.field("title", 2.0).field("body", 1.0).filters(["category", "done"]).open().unwrap();
    assert_eq!(ids(&articles.search("closed").run().unwrap()), ["1"]);
}

#[test]
fn a_table_s_index_follows_one_table_by_a_key_of_unique_values() {
    let f = fixture();
    f.db.exec(sql!("create table notes (slug text, body text)")).unwrap();
    let opened = |builder: crate::search::TableIndexBuilder<Value>| builder.open().map(drop).unwrap_err().to_string();
    let index = |name: &str| f.db.search_index::<Value>(name);
    assert!(opened(index("n").field("body", 1.0)).contains("names its table"));
    assert!(opened(index("n").table("nothing").field("body", 1.0)).contains("no table nothing"));
    assert!(opened(index("n").table("articles").field("nope", 1.0)).contains("no column nope"));
    assert!(opened(index("n").table("notes").field("body", 1.0)).contains("no primary key"));
    assert!(opened(index("n").table("notes").key("slug").field("body", 1.0)).contains("no key"));
    assert!(opened(index("n").table("articles").field("title.x", 1.0)).contains("reads columns"));
    f.db.exec(sql!("create unique index notes_by_slug on notes (slug)")).unwrap();
    let notes = index("n").table("notes").key("slug").field("body", 1.0).open().unwrap();
    f.db.exec(sql!("insert into notes values ('a-note', 'hello')")).unwrap();
    assert_eq!(ids(&notes.search("hello").run().unwrap()), ["a-note"], "a hit's id is its key as text");
    assert!(opened(index("n").table("articles").field("title", 1.0)).contains("follows table notes by slug"));
}

#[test]
fn a_column_added_reads_the_table_again_beside_the_index_and_a_drop_takes_the_triggers_off() {
    let f = fixture();
    let titles = f.db.search_index::<Article>("articles_search").table("articles").field("title", 1.0).open().unwrap();
    f.ready(&titles);
    f.insert(1, "bread", "flour and salt");
    let both = f.db.search_index::<Article>("articles_search").table("articles");
    let both = both.field("title", 1.0).field("body", 1.0).open().unwrap();
    assert!(matches!(both.status().unwrap(), Status::Rebuilding { .. }));
    assert!(both.search("salt").run().unwrap().hits.is_empty(), "the old settings answer until the switch");
    f.insert(2, "rye", "salt again");
    f.ready(&both);
    assert_eq!(ids(&both.search("salt").run().unwrap()), ["2", "1"], "the shorter body first");
    assert!(f.db.drop_search_index("articles_search").unwrap());
    let triggers: i64 = f.db.scalar(sql!("select count(*) from sqlite_schema where type = 'trigger'")).unwrap();
    assert_eq!(triggers, 0);
    f.insert(3, "after the drop", "");
    assert_eq!(both.search("salt").run().unwrap_err().kind(), ErrorKind::Invalid);
    let pending: i64 = f.db.scalar(sql!("select count(*) from _tinystore_search_pending")).unwrap();
    assert_eq!(pending, 0);
}

// contract: SRCH-06
#[test]
fn a_row_changed_while_its_index_is_read_again_reaches_the_new_rows_by_what_only_they_read() {
    let f = fixture();
    let titles = f.db.search_index::<Article>("articles_search").table("articles").field("title", 1.0).open().unwrap();
    f.ready(&titles);
    for id in 1..=3 {
        f.insert(id, &format!("shoes {id}"), "");
    }
    let opened = f.db.search_index::<Article>("articles_search").table("articles").field("title", 1.0);
    let both = opened.filters(["category"]).vectors("embedding", 3).open().unwrap();
    assert!(matches!(both.status().unwrap(), Status::Rebuilding { .. }));
    let search = Arc::clone(&both.inner().search);
    assert!(!search.copy_step(&both.inner().open).unwrap(), "every row copied in one step");
    let embedding = floats(&[1.0, 0.0, 0.0]);
    f.db.exec(sql!("update articles set embedding = ?, category = 'red' where id = 2", embedding)).unwrap();
    f.ready(&both);
    assert_eq!(ids(&both.search_vector(&[1.0, 0.1, 0.0]).run().unwrap()), ["2"], "a vector only the new rows read");
    let red = both.search("shoes").filter(eq("category", "red")).run().unwrap();
    assert_eq!(ids(&red), ["2"], "a filter only the new rows read");
}

/// Words the mix writes, few, so that searches find many rows.
const WORDS: [&str; 12] =
    ["bread", "salt", "flour", "rye", "milk", "cream", "butter", "honey", "tea", "jam", "oat", "corn"];

/// A deterministic stream of numbers, so that a failure repeats.
struct Steps(u64);

impl Steps {
    fn next(&mut self, below: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % below
    }

    fn words(&mut self, count: u64) -> String {
        (0..1 + self.next(count)).map(|_| WORDS[self.next(WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
    }
}

#[test]
fn through_a_long_mix_of_writes_a_search_answers_what_an_index_made_afresh_of_the_rows_would() {
    let f = fixture();
    for id in 0..300 {
        f.insert(id, &format!("{} {id}", WORDS[(id % 12) as usize]), "");
    }
    let opened = f.db.search_index::<Article>("articles_search").table("articles").field("title", 2.0);
    let articles = opened.field("body", 1.0).filters(["category", "done"]).open().unwrap();
    let search = std::sync::Arc::clone(&articles.inner().search);
    assert!(!search.file_write(|tx| search.fill_step(tx, "articles_search")).unwrap(), "a first step of 256");
    f.db.exec(sql!("update articles set body = 'honey jam' where id = 10")).unwrap();
    f.db.exec(sql!("update articles set body = 'honey tea' where id = 290")).unwrap();
    f.db.exec(sql!("delete from articles where id = 280")).unwrap();
    f.ready(&articles);
    same_as_afresh(&f, &articles, 0);
    let mut steps = Steps(0x9e37_79b9_7f4a_7c15);
    for round in 1..=6 {
        for _ in 0..60 {
            write_one(&f, &mut steps);
        }
        if round % 2 == 0 {
            search::maintain(&f.store).unwrap();
        }
        same_as_afresh(&f, &articles, round);
    }
}

/// One write of the mix, through a statement, a batch, a transaction or a
/// connection beside the store, whose keys a write of the store's settles.
fn write_one(f: &Fixture, steps: &mut Steps) {
    let id = steps.next(400) as i64;
    let (title, body) = (steps.words(3), steps.words(5));
    let category = ["dairy", "bakery"][steps.next(2) as usize];
    let upsert = sql!(
        "insert into articles (id, title, body, category) values (?, ?, ?, ?)
         on conflict (id) do update set title = excluded.title, body = excluded.body",
        id,
        title,
        body,
        category
    );
    match steps.next(6) {
        0 => drop(f.db.exec(upsert).unwrap()),
        1 => drop(f.db.exec(sql!("delete from articles where id = ?", id)).unwrap()),
        2 => drop(f.db.batch([upsert, sql!("update articles set done = 1 - done where id = ?", id / 2)]).unwrap()),
        3 => f.db.tx(|tx| tx.exec(upsert).map(drop)).unwrap(),
        4 => drop(f.db.exec(sql!("update articles set views = views + 1 where id = ?", id)).unwrap()),
        _ => {
            let beside = f.beside();
            beside.execute("delete from articles where id = ?1", [id]).unwrap();
            beside.execute("insert into articles (id, title, body) values (?1, ?2, ?3)", (id, &title, &body)).unwrap();
            f.db.exec(sql!("update articles set views = views where id = -1")).unwrap();
        }
    }
}

/// Searches the table's index and an index made afresh of the table's rows
/// as it now holds them, and finds the same hits with the same scores.
fn same_as_afresh(f: &Fixture, articles: &TableIndex<Article>, round: u32) {
    let afresh = f.store.index::<Value>(&format!("afresh-{round}")).field("title", 2.0).field("body", 1.0);
    let afresh = afresh.filters(["category", "done"]).open().unwrap();
    let rows: Vec<Article> = f.db.all(sql!("select * from articles")).unwrap();
    let documents: Vec<(String, Value)> = rows
        .iter()
        .map(|row| {
            let document =
                json!({ "title": row.title, "body": row.body, "category": row.category, "done": i64::from(row.done) });
            (row.id.to_string(), document)
        })
        .collect();
    for chunk in documents.chunks(500) {
        afresh.set_many(chunk.iter().map(|(id, document)| (id.as_str(), document))).unwrap();
    }
    for word in WORDS {
        let hits = |found: Vec<(String, f64)>| {
            found.into_iter().map(|(id, score)| (id, score.to_bits())).collect::<BTreeSet<_>>()
        };
        let table = articles.search(word).limit(1000).run().unwrap().hits;
        let made = afresh.search(word).limit(1000).run().unwrap().hits;
        assert_eq!(
            hits(table.into_iter().map(|hit| (hit.id, hit.score)).collect()),
            hits(made.into_iter().map(|hit| (hit.id, hit.score)).collect()),
            "round {round}: {word}"
        );
        let table = articles.search(word).filter(eq("done", &true)).limit(1000).run().unwrap();
        let made = afresh.search(word).filter(eq("done", &1)).limit(1000).run().unwrap();
        assert_eq!(ids(&table).into_iter().collect::<BTreeSet<_>>(), ids(&made).into_iter().collect(), "round {round}");
    }
}

// contract: SRCH-08
#[test]
fn a_write_from_beside_the_store_reaches_the_index_within_a_second_when_the_store_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let db = store.database("app").migrations([("0001_articles.sql", ARTICLES)]).open().unwrap();
    let articles = db.search_index::<Article>("articles_search").table("articles").field("title", 1.0).open().unwrap();
    articles.ready().unwrap();
    let beside = rusqlite::Connection::open(dir.path().join("sql").join("app.db")).unwrap();
    beside.busy_timeout(Duration::from_secs(5)).unwrap();
    beside.execute("insert into articles (id, title) values (1, 'outside words')", []).unwrap();
    let written = std::time::Instant::now();
    while articles.search("outside").run().unwrap().hits.is_empty() {
        assert!(written.elapsed() < Duration::from_secs(5), "the index never read the row");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_column_a_migration_dropped_reads_as_empty_until_the_index_is_opened_without_it() {
    let f = fixture();
    let articles = f.db.search_index::<Value>("articles_search").table("articles").field("title", 2.0);
    let articles = articles.field("body", 1.0).filters(["category", "done"]).open().unwrap();
    f.ready(&articles);
    f.insert(1, "bread", "salt");
    f.db.exec(sql!("alter table articles drop column body")).unwrap();
    f.db.exec(sql!("insert into articles (id, title) values (2, 'rye')")).unwrap();
    f.db.exec(sql!("update articles set title = 'oat' where id = 1")).unwrap();
    assert_eq!(ids(&articles.search("oat rye").run().unwrap()).len(), 2, "the table's writes go on");
    assert!(articles.search("salt").run().unwrap().hits.is_empty(), "the dropped column read as empty");
    let titles = f.db.search_index::<Value>("articles_search").table("articles").field("title", 2.0);
    let titles = titles.filters(["category", "done"]).open().unwrap();
    assert_eq!(ids(&titles.search("oat").run().unwrap()), ["1"]);
}

#[test]
fn a_table_a_migration_rebuilt_is_read_again_when_its_index_opens() {
    let f = fixture();
    drop(f.articles());
    f.insert(1, "bread", "");
    f.insert(2, "milk", "");
    f.store.close().unwrap();
    let rebuilt = "create table new_articles (
        id integer primary key, title text not null, body text not null default '', category text,
        done integer not null default 0, views integer not null default 0, embedding blob, extra text);
    insert into new_articles (id, title) select id, title || ' rye' from articles where id != 2;
    insert into new_articles (id, title) values (3, 'honey');
    drop table articles;
    alter table new_articles rename to articles;";
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let migrations = [("0001_articles.sql", ARTICLES), ("0002_rebuilt.sql", rebuilt)];
    let db = store.database("app").migrations(migrations).open().unwrap();
    let triggers: i64 = db.scalar(sql!("select count(*) from sqlite_schema where type = 'trigger'")).unwrap();
    assert_eq!(triggers, 0, "the triggers went with the old table");
    let articles = db.search_index::<Article>("articles_search").table("articles");
    let articles = articles.field("title", 2.0).field("body", 1.0).filters(["category", "done"]).open().unwrap();
    assert!(matches!(articles.status().unwrap(), Status::Rebuilding { .. }));
    search::maintain(&store).unwrap();
    articles.ready().unwrap();
    assert_eq!(ids(&articles.search("rye").run().unwrap()), ["1"], "a row the migration changed");
    assert!(articles.search("milk").run().unwrap().hits.is_empty(), "a row the migration deleted");
    assert_eq!(ids(&articles.search("honey").run().unwrap()), ["3"], "a row the migration added");
    db.exec(sql!("insert into articles (id, title) values (4, 'after')")).unwrap();
    assert_eq!(ids(&articles.search("after").run().unwrap()), ["4"], "the triggers are back");
}
