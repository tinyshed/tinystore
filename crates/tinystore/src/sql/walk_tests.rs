//! A walk of a statement's rows: a part at a time from one snapshot, past
//! what `all` holds, and on past its snapshot by its key.

use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::Deserialize;

use super::super::{Database, Direction, Each};
use super::{AHEAD, BOUND};
use crate::{ErrorKind, Options, Store, TestClock, sql};

/// 140 rows of half a MiB each: 70 MiB, past the 64 MiB `all` holds.
const BIG: &str = "create table big (id integer primary key, data blob not null);
with recursive n(i) as (select 1 union all select i + 1 from n where i < 140)
insert into big (id, data) select i, zeroblob(524288) from n";

/// 60 rows of 100 KiB each, ten to a part of a walk.
const NOTES: &str = "create table notes (id integer primary key, grp integer not null, body text not null);
with recursive n(i) as (select 1 union all select i + 1 from n where i < 60)
insert into notes (id, grp, body) select i, 1, replace(hex(zeroblob(51200)), '0', 'x') from n";

#[derive(Debug, Deserialize)]
struct Big {
    id: i64,
    data: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct Note {
    id: i64,
    body: String,
}

struct Fixture {
    store: Store,
    clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    _dir: tempfile::TempDir,
}

fn fixture(memory: Option<u64>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let options = Options { memory, clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { store, clock, _dir: dir }
}

impl Fixture {
    fn database(&self, migration: &str) -> Database {
        self.store.database("app").migrations([("0001_rows.sql", migration)]).open().unwrap()
    }

    /// Moves the store's clock past one snapshot's bound once the walk's
    /// reader waits with its parts ahead, and waits for it to end its
    /// snapshot there.
    fn outlast_the_snapshot<T>(&self, each: &Each<T>) -> bool {
        let walk = each.walking().expect("a walk");
        until(|| walk.queued() >= AHEAD, "the reader waits for room");
        self.clock.advance(BOUND + Duration::from_secs(1));
        until(|| walk.resuming() || walk.ended(), "the reader ends its snapshot");
        walk.resuming()
    }
}

fn until(done: impl Fn() -> bool, what: &str) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn each_reads_rows_past_what_all_holds_with_a_few_parts_in_memory() {
    // contract: SQL-13
    let fixture = fixture(Some(16 << 20));
    let db = fixture.database(BIG);

    let all = db.all::<Big>("select id, data from big order by id").unwrap_err();
    assert_eq!(all.kind(), ErrorKind::Limit, "all holds every row: {all}");

    let (mut rows, mut bytes, mut most) = (0, 0, 0);
    for row in db.each::<Big>("select id, data from big order by id").unwrap() {
        let row = row.unwrap();
        rows += 1;
        bytes += row.data.len();
        assert_eq!(row.id, rows);
        most = most.max(fixture.store.memory().used());
    }
    assert_eq!((rows, bytes), (140, 140 << 19), "every row of 70 MiB");
    assert!(most <= 8 << 20, "a few parts held at once, not the rows: {most} bytes");
    assert_eq!(fixture.store.memory().used(), 0, "the last part gave its memory back");

    let by_table: usize =
        db.table::<Big>("big").order_by("id", Direction::Asc).each().unwrap().map(|row| row.unwrap().data.len()).sum();
    assert_eq!(by_table, 140 << 19, "a table's walk reads them all too");
}

#[test]
fn a_walk_ordered_by_its_key_reads_on_past_its_snapshot_seeing_rows_as_they_are_then() {
    // contract: SQL-13
    let fixture = fixture(None);
    let db = fixture.database(NOTES);
    let notes = db.table::<Note>("notes").order_by("id", Direction::Asc);

    let mut each = notes.each().unwrap();
    let first = each.next().unwrap().unwrap();
    assert!(fixture.outlast_the_snapshot(&each), "the snapshot ended, and the walk reads on once a part is taken");
    db.exec(sql!("update notes set body = 'changed' where id = ?", 55)).unwrap();
    db.exec("insert into notes (id, grp, body) values (61, 1, 'new')").unwrap();

    let rest: Vec<Note> = each.map(Result::unwrap).collect();
    let ids: Vec<i64> = std::iter::once(first.id).chain(rest.iter().map(|note| note.id)).collect();
    assert_eq!(ids, (1..=61).collect::<Vec<_>>(), "each row once, in order, the new one among them");
    let changed = rest.iter().find(|note| note.id == 55).unwrap();
    assert_eq!(changed.body, "changed", "a row read after the snapshot ended is read as it is then");
    assert_eq!(rest.iter().find(|note| note.id == 2).unwrap().body.len(), 102_400);
}

#[test]
fn a_walk_without_a_key_fails_limit_past_its_snapshot_naming_list() {
    // contract: SQL-13
    let fixture = fixture(None);
    let db = fixture.database(NOTES);

    let mut each = db.each::<Note>("select id, body from notes order by id").unwrap();
    each.next().unwrap().unwrap();
    assert!(!fixture.outlast_the_snapshot(&each), "the walk ended at its snapshot");
    let read: Vec<_> = each.collect();
    let failed = read.last().unwrap().as_ref().unwrap_err();
    assert_eq!(failed.kind(), ErrorKind::Limit, "{failed}");
    assert!(failed.to_string().contains("list"), "it names list: {failed}");
    assert_eq!(failed.fact("limit"), Some("snapshot millis"));
    assert!(read[..read.len() - 1].iter().all(Result::is_ok), "the rows read before it come first");

    let mut ordered_by_a_shared_value = db.table::<Note>("notes").order_by("grp", Direction::Asc).each().unwrap();
    ordered_by_a_shared_value.next().unwrap().unwrap();
    assert!(!fixture.outlast_the_snapshot(&ordered_by_a_shared_value));
    let failed = ordered_by_a_shared_value.find_map(Result::err).unwrap();
    assert_eq!(failed.kind(), ErrorKind::Limit, "reading on after a shared value would skip rows: {failed}");
    assert!(failed.to_string().contains("unique"), "{failed}");
}

#[test]
fn a_walk_fails_where_its_statement_does_and_gives_a_writes_rows_as_all_does() {
    let fixture = fixture(None);
    let db = fixture.database("create table t (id integer primary key, name text)");

    let wrong = db.each::<i64>("select nothing from nowhere").unwrap_err();
    assert_eq!(wrong.kind(), ErrorKind::Invalid, "{wrong}");
    assert!(wrong.to_string().contains("sql app"), "named by its database: {wrong}");

    let none: Vec<i64> = db.each("select id from t").unwrap().map(Result::unwrap).collect();
    assert!(none.is_empty());
    let written: Vec<String> =
        db.each("insert into t (name) values ('a'), ('b') returning name").unwrap().map(Result::unwrap).collect();
    assert_eq!(written, ["a", "b"], "a write's rows, once it committed");
    assert_eq!(db.scalar::<i64>("select count(*) from t").unwrap(), 2);

    let in_tx = db.tx(|tx| -> crate::Result<Vec<String>> {
        tx.exec("insert into t (name) values ('c')")?;
        tx.table::<String>("t").select::<String>("name").order_by("id", Direction::Asc).each()?.collect()
    });
    assert_eq!(in_tx.unwrap(), ["a", "b", "c"], "inside a transaction it reads what the transaction wrote");
    let refused = db.tx(|_| db.each::<i64>("select id from t").map(|_| ())).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "a walk beside its own transaction: {refused}");
}

#[test]
fn a_walk_reads_on_by_its_keys_index() {
    let fixture = fixture(None);
    let db = fixture.database("create table t (id integer primary key, k text unique, v text)");
    let key = |column: &str| vec![super::Key { column: column.to_owned(), direction: Direction::Asc }];
    let read_on = |column: &str, after: super::Value| {
        let statement = sql::Sql::new(format!("select * from t order by {column}"));
        let walked = super::read_on(&statement, &key(column), &[after]).unwrap();
        let explained =
            sql::Sql::piece(format!("explain query plan {}", walked.text()), walked.values().to_vec(), false);
        db.all::<(i64, i64, i64, String)>(explained).unwrap()
    };
    let by_id = read_on("id", super::Value::Integer(5));
    assert!(by_id[0].3.contains("rowid>?"), "a search past the last row, not a scan: {by_id:?}");
    let by_k = read_on("k", super::Value::Text("a".to_owned()));
    assert!(by_k[0].3.contains("(k>?)"), "{by_k:?}");
}

#[test]
fn a_walk_dropped_half_way_gives_its_reader_back() {
    let fixture = fixture(Some(64 << 20));
    let db = fixture.database(BIG);
    let mut each = db.each::<Big>("select id, data from big").unwrap();
    each.next().unwrap().unwrap();
    drop(each);
    until(|| fixture.store.memory().used() == 0, "the parts read ahead are let go");
    let count: i64 = db.scalar("select count(*) from big").unwrap();
    assert_eq!(count, 140);
}
