//! A database's awaited calls: that each answers as its blocking twin does on
//! the same database, where its work runs, what a dropped future does, and
//! a transaction's line.

use std::pin::pin;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::Database;
use crate::aio::Store;
use crate::sql::{Direction, Upsert};
use crate::{CancellationToken, Error, ErrorKind, Options, TestClock, sql};

const NOTES: &str = "create table notes (
    id         text primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0 check (done in (0, 1))
) strict";

const AUTHORS: &str = "create table authors (author_id integer primary key, name text not null) strict";

/// Counts to a billion, minutes of work: a read only its stop ends.
const COUNT_FOR_MINUTES: &str =
    "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) select count(*) from c";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Note {
    id: String,
    author_id: i64,
    title: String,
    done: bool,
}

fn note(id: &str, author_id: i64, title: &str) -> Note {
    Note { id: id.to_owned(), author_id, title: title.to_owned(), done: false }
}

struct Fixture {
    store: Store,
    clock: Arc<TestClock>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    Fixture { store, clock, _dir: dir }
}

impl Fixture {
    fn app(&self) -> Database {
        let migrations = [("0001_notes.sql", NOTES), ("0002_authors.sql", AUTHORS)];
        self.store.database("app").migrations(migrations).open().unwrap()
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
}

fn ids(db: &Database) -> Vec<String> {
    db.blocking().all("select id from notes order by id").unwrap()
}

/// Waits until the notes' ids are `want`, as a write committed on the pool
/// makes them.
fn wait_for_ids(db: &Database, want: &[&str], what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while ids(db) != want {
        assert!(Instant::now() < deadline, "{what}: the ids are {:?}", ids(db));
        thread::sleep(Duration::from_millis(1));
    }
}

fn insert_note(id: &str, title: &str) -> sql::Sql {
    sql!("insert into notes (id, author_id, title) values (?, ?, ?)", id, 42, title)
}

/// Holds `db`'s writer in a blocking transaction on a thread of its own,
/// until the sender is told or dropped, ten seconds at most.
fn holding_the_writer(db: &Database) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (release, released) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let db = db.blocking().clone();
    let holder = thread::spawn(move || {
        db.tx(|_| -> crate::Result<()> {
            holding.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(10));
            Ok(())
        })
        .unwrap();
    });
    held.recv_timeout(Duration::from_secs(10)).expect("the writer was never held");
    (release, holder)
}

#[tokio::test]
async fn reads_answer_as_the_blocking_ones_do_on_the_same_database() {
    let f = fixture();
    let db = f.app();
    for (id, title) in [("n1", "Buy milk"), ("n2", "Call mom")] {
        db.exec(insert_note(id, title)).await.unwrap();
    }
    db.exec("insert into notes (id, author_id, title) values ('n3', 7, 'Water plants')").await.unwrap();

    let mine = sql!("select * from notes where author_id = ? order by id", 42);
    let awaited: Vec<Note> = db.all(mine.clone()).await.unwrap();
    assert_eq!(awaited, [note("n1", 42, "Buy milk"), note("n2", 42, "Call mom")]);
    assert_eq!(awaited, db.blocking().all::<Note>(mine).unwrap());
    let theirs = sql!("select * from notes where id = ?", "n3");
    assert_eq!(db.one::<Note>(theirs.clone()).await.unwrap(), db.blocking().one::<Note>(theirs).unwrap());
    assert_eq!(db.one::<Note>(sql!("select * from notes where id = ?", "n9")).await.unwrap(), None);
    assert_eq!(db.scalar::<i64>("select count(*) from notes where not done").await.unwrap(), 3);
    let none: Option<i64> = db.scalar("select max(author_id) from notes where id = 'n9'").await.unwrap();
    assert_eq!(none, None, "a NULL, as max over no rows gives");

    let several = db.one::<Note>("select * from notes").await.unwrap_err();
    assert_eq!(several.kind(), ErrorKind::Invalid);
    let blocking = db.blocking().one::<Note>("select * from notes").unwrap_err();
    assert_eq!(several.to_string(), blocking.to_string(), "the same error, naming the statement and the database");

    // each future is Send and 'static, so a program spawns it
    let spawned = tokio::spawn(db.all::<Note>("select * from notes order by id")).await.unwrap().unwrap();
    assert_eq!(spawned.len(), 3);
    f.store.close().await.unwrap();
}

// contract: SQL-01
#[tokio::test]
async fn a_statement_with_returning_goes_on_from_its_read_to_the_writer() {
    let f = fixture();
    let db = f.app();
    let insert =
        "insert into notes (id, author_id, title) values ('n1', 42, 'Buy milk'), ('n2', 42, 'Call mom') returning id";
    let added: Vec<String> = db.all(insert).await.unwrap();
    assert_eq!(added, ["n1", "n2"]);
    assert!(db.blocking().known_to_write(&insert.into()), "the read that found it writes says so");

    let done = "update notes set done = 1 where id = ? returning *";
    let first: Note = db.one(sql!(done, "n1")).await.unwrap().unwrap();
    assert!(first.done);
    // known to write now: it goes to the writer without a read first
    let second: Note = db.one(sql!(done, "n2")).await.unwrap().unwrap();
    assert!(second.done);
    let title: String =
        db.scalar(sql!("update notes set title = title || '!' where id = ? returning title", "n2")).await.unwrap();
    assert_eq!(title, "Call mom!");

    let several = db.one::<Note>("update notes set done = 0 returning *").await.unwrap_err();
    assert_eq!(several.kind(), ErrorKind::Invalid, "{several}");
    let still_done: i64 = db.blocking().scalar("select count(*) from notes where done = 1").unwrap();
    assert_eq!(still_done, 2, "a write that gave one two rows rolled back");
    f.store.close().await.unwrap();
}

// contract: DUR-11, DUR-04, STORE-10
#[test]
fn a_write_polled_once_commits_though_its_future_is_dropped_unawaited() {
    let f = fixture();
    let db = f.app();
    let notes = db.table::<Note>("notes");
    let (release, holder) = holding_the_writer(&db);
    {
        let mut context = Context::from_waker(Waker::noop());
        let mut exec = pin!(db.exec(insert_note("n1", "exec")));
        let mut batch = pin!(db.batch([insert_note("n2", "batch")]));
        let mut insert = pin!(notes.insert(&note("n3", 42, "insert")));
        // the writer is held: a poll that led its write's commit would wait for it here
        assert!(exec.as_mut().poll(&mut context).is_pending());
        assert!(batch.as_mut().poll(&mut context).is_pending());
        assert!(insert.as_mut().poll(&mut context).is_pending());
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    wait_for_ids(&db, &["n1", "n2", "n3"], "a write dropped after its first poll never committed");
    f.store.blocking().close().unwrap();
}

// contract: DUR-11, DUR-04
#[tokio::test]
async fn a_write_whose_future_is_never_polled_writes_nothing() {
    let f = fixture();
    let db = f.app();
    let notes = db.table::<Note>("notes");
    drop(db.exec(insert_note("n1", "exec")));
    drop(db.batch([insert_note("n2", "batch")]));
    drop(notes.insert(&note("n3", 42, "insert")));
    drop(db.all::<String>("insert into notes (id, author_id, title) values ('n4', 42, 'returning') returning id"));
    drop(db.tx(|tx| tx.exec(insert_note("n5", "tx"))));
    db.exec(insert_note("n6", "awaited")).await.unwrap();
    assert_eq!(ids(&db), ["n6"], "only the write that was polled is there");
    f.store.close().await.unwrap();
}

// contract: DUR-11
#[test]
fn a_statement_whose_read_found_it_writes_commits_though_its_future_is_dropped_then() {
    let f = fixture();
    let db = f.app();
    let insert: sql::Sql = "insert into notes (id, author_id, title) values ('n1', 42, 'Buy milk') returning id".into();
    // held, so that the drop below comes before the commit, whatever the threads do
    let (release, holder) = holding_the_writer(&db);
    {
        let mut added = pin!(db.all::<String>(insert.clone()));
        assert!(added.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending(), "it is read on the pool");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !db.blocking().known_to_write(&insert) {
            assert!(Instant::now() < deadline, "the read never found that the statement writes");
            thread::sleep(Duration::from_millis(1));
        }
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    wait_for_ids(&db, &["n1"], "a statement dropped once its read found it writes never committed");
    f.store.blocking().close().unwrap();
}

/// A note whose decoding waits until [`DECODING`] lets it go, thirty seconds
/// at most.
#[derive(Debug, PartialEq, Serialize)]
#[serde(transparent)]
struct SlowToRead(Note);

/// Whether a [`SlowToRead`]'s decoding may end.
static DECODING: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

impl<'de> Deserialize<'de> for SlowToRead {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let note = Note::deserialize(deserializer)?;
        let (may_end, changed) = &DECODING;
        let waited = changed.wait_timeout_while(may_end.lock().unwrap(), Duration::from_secs(30), |may_end| !*may_end);
        drop(waited.unwrap());
        Ok(SlowToRead(note))
    }
}

#[test]
fn a_writes_rows_are_decoded_on_the_pool_and_hold_back_no_write_committed_beside_it() {
    let f = fixture();
    let db = f.app();
    let slow = db.table::<SlowToRead>("notes");
    let mut context = Context::from_waker(Waker::noop());
    // held, so that both are queued before the commit that takes them
    let (release, holder) = holding_the_writer(&db);
    let mut inserted = pin!(slow.insert(&SlowToRead(note("n1", 42, "Buy milk"))));
    let mut beside = pin!(db.exec(insert_note("n2", "Call mom")));
    assert!(inserted.as_mut().poll(&mut context).is_pending());
    assert!(beside.as_mut().poll(&mut context).is_pending());
    release.send(()).unwrap();
    holder.join().unwrap();

    let runtime = runtime();
    let beside = runtime.block_on(async { tokio::time::timeout(Duration::from_secs(10), beside.as_mut()).await });
    let (may_end, changed) = &DECODING;
    *may_end.lock().unwrap() = true;
    changed.notify_all();
    assert!(beside.is_ok(), "the write beside the insert waited for the insert's rows to be decoded");
    beside.unwrap().unwrap();
    assert_eq!(runtime.block_on(inserted.as_mut()).unwrap(), SlowToRead(note("n1", 42, "Buy milk")));
    assert_eq!(ids(&db), ["n1", "n2"]);
    f.store.blocking().close().unwrap();
}

// contract: STORE-09, STORE-10
#[tokio::test]
async fn reads_whose_futures_are_dropped_stop_and_give_their_threads_and_readers_back() {
    let f = fixture();
    let db = f.app();
    {
        let mut count = pin!(db.scalar::<i64>(COUNT_FOR_MINUTES));
        let polled = count.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        assert!(polled.is_pending(), "the count runs on the pool, never in the poll");
    }
    // More than the pool's threads: a read its drop did not stop would hold one for minutes.
    let reads: Vec<_> = (0..24)
        .map(|_| tokio::spawn(tokio::time::timeout(Duration::from_millis(100), db.scalar::<i64>(COUNT_FOR_MINUTES))))
        .collect();
    for read in reads {
        assert!(read.await.unwrap().is_err(), "the count never ends: its timeout dropped it");
    }
    let next = Instant::now();
    assert_eq!(db.scalar::<i64>("select 1").await.unwrap(), 1);
    assert_eq!(db.table::<Note>("notes").count().await.unwrap(), 0);
    assert_eq!(db.blocking().scalar::<i64>("select 1").unwrap(), 1);
    assert!(next.elapsed() < Duration::from_secs(10), "the next reads waited {:?}", next.elapsed());
    f.store.close().await.unwrap();
}

// contract: STORE-09
#[tokio::test]
async fn a_token_around_the_call_that_made_the_future_stops_its_read() {
    let f = fixture();
    let db = f.app();
    let token = CancellationToken::timeout(Duration::from_millis(50));
    let count = token.run(|| db.scalar::<i64>(COUNT_FOR_MINUTES));
    let error = count.await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
    assert!(error.to_string().starts_with(r#"sql app: statement "with recursive"#), "{error}");
    assert!(error.to_string().ends_with("a read past its timeout of 50ms: cancelled"), "{error}");
    f.store.close().await.unwrap();
}

// contract: SQL-07, STORE-10
#[tokio::test]
async fn a_transaction_runs_its_closure_on_the_pool_and_commits_or_rolls_back() {
    let f = fixture();
    let db = f.app();
    let ran_on = db
        .tx(|tx| -> crate::Result<Option<String>> {
            tx.exec(insert_note("n1", "Buy milk"))?;
            let seen: i64 = tx.scalar("select count(*) from notes")?;
            assert_eq!(seen, 1, "a read inside sees the transaction's own write");
            assert_eq!(tx.table::<Note>("notes").all()?, [note("n1", 42, "Buy milk")]);
            Ok(thread::current().name().map(str::to_owned))
        })
        .await
        .unwrap();
    assert_eq!(ran_on.as_deref(), Some(crate::pool::THREAD), "the closure ran on the store's pool");
    assert_eq!(ids(&db), ["n1"]);

    let refused = db
        .tx(|tx| -> crate::Result<()> {
            tx.exec("delete from notes")?;
            Err(Error::invalid("the order was refused"))
        })
        .await
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid);
    assert_eq!(ids(&db), ["n1"], "a closure that fails rolls back");

    let panicking = db.tx(|tx| -> crate::Result<()> {
        tx.exec("delete from notes")?;
        panic!("the closure panicked");
    });
    let resumed = tokio::spawn(panicking).await.unwrap_err();
    assert!(resumed.is_panic(), "the panic is resumed where the future is awaited");
    assert_eq!(ids(&db), ["n1"], "the panic rolled the transaction back");
    f.store.close().await.unwrap();
}

/// A transaction of `db` that inserts `id`, then says it holds the line and
/// waits until it is let go, ten seconds at most.
fn holding_the_line(
    db: &Database,
    id: &'static str,
) -> (impl Future<Output = crate::Result<()>> + Send + 'static, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (release, released) = mpsc::channel::<()>();
    let (entered, holding) = mpsc::channel();
    let first = db.tx(move |tx| -> crate::Result<()> {
        tx.exec(insert_note(id, "first"))?;
        entered.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(10));
        Ok(())
    });
    (first, holding, release)
}

// contract: SQL-07
#[test]
fn a_transactions_five_seconds_count_from_its_first_poll_its_wait_in_the_line_among_them() {
    let f = fixture();
    let db = f.app();
    let mut context = Context::from_waker(Waker::noop());
    let (first, holding, release) = holding_the_line(&db, "n1");
    let mut first = pin!(first);
    assert!(first.as_mut().poll(&mut context).is_pending());
    holding.recv_timeout(Duration::from_secs(10)).expect("the first transaction never began");
    let mut second = pin!(db.tx(|tx| tx.exec(insert_note("n2", "second"))));
    assert!(second.as_mut().poll(&mut context).is_pending(), "it waits in its database's line");

    f.clock.advance(Duration::from_secs(6));
    release.send(()).unwrap();
    let runtime = runtime();
    let first = runtime.block_on(first.as_mut()).unwrap_err();
    assert_eq!(first.kind(), ErrorKind::Limit, "the first held the writer past its bound: {first}");
    let second = runtime.block_on(second.as_mut()).unwrap_err();
    assert_eq!(second.kind(), ErrorKind::Limit, "its wait in the line counted: {second}");
    assert!(ids(&db).is_empty(), "both rolled back");
    f.store.blocking().close().unwrap();
}

// contract: DUR-11
#[test]
fn a_transaction_dropped_while_it_waits_in_its_line_runs_all_the_same() {
    let f = fixture();
    let db = f.app();
    let mut context = Context::from_waker(Waker::noop());
    let (first, holding, release) = holding_the_line(&db, "n1");
    let mut first = pin!(first);
    assert!(first.as_mut().poll(&mut context).is_pending());
    holding.recv_timeout(Duration::from_secs(10)).expect("the first transaction never began");
    {
        let mut second = pin!(db.tx(|tx| tx.exec(insert_note("n2", "second"))));
        assert!(second.as_mut().poll(&mut context).is_pending());
    }
    release.send(()).unwrap();
    runtime().block_on(first.as_mut()).unwrap();
    wait_for_ids(&db, &["n1", "n2"], "a transaction dropped in its line never ran");
    f.store.blocking().close().unwrap();
}

// contract: DUR-07
#[test]
fn an_awaited_call_from_inside_the_databases_transaction_is_refused_rather_than_wait_for_itself() {
    let f = fixture();
    let db = f.app();
    let runtime = runtime();
    let refused = db
        .blocking()
        .tx(|tx| -> crate::Result<Vec<ErrorKind>> {
            tx.exec(insert_note("n1", "inside"))?;
            let write = runtime.block_on(db.exec(insert_note("n2", "around"))).unwrap_err();
            let read = runtime.block_on(db.scalar::<i64>("select count(*) from notes")).unwrap_err();
            let nested = runtime.block_on(db.tx(|tx| tx.exec(insert_note("n3", "nested")))).unwrap_err();
            Ok(vec![write.kind(), read.kind(), nested.kind()])
        })
        .unwrap();
    assert_eq!(refused, [ErrorKind::Invalid; 3]);
    assert_eq!(ids(&db), ["n1"], "the transaction around them went on and committed");
    f.store.blocking().close().unwrap();
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Titled {
    id: String,
    title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Author {
    author_id: i64,
    name: String,
    notes: Vec<Titled>,
}

// contract: SQL-09, SQL-10
#[tokio::test]
async fn a_tables_calls_answer_as_the_blocking_ones_do() {
    let f = fixture();
    let db = f.app();
    let (notes, blocking) = (db.table::<Note>("notes"), db.blocking().table::<Note>("notes"));
    assert_eq!(notes.name(), "notes");
    assert_eq!(notes.insert(&note("n1", 42, "Buy milk")).await.unwrap(), note("n1", 42, "Buy milk"));
    let rows = [note("n2", 42, "Call mom"), note("n3", 7, "Water plants"), note("n4", 42, "Pay rent")];
    assert_eq!(notes.insert_all(&rows).await.unwrap(), rows);
    assert!(notes.insert_all(&[]).await.unwrap().is_empty());
    let renamed = notes.upsert(&note("n1", 42, "Buy oat milk"), Upsert::on(["id"]).update(["title"])).await.unwrap();
    assert_eq!(renamed.map(|row| row.title).as_deref(), Some("Buy oat milk"));
    let theirs = Upsert::on(["id"]).update(["title"]).only_if(sql::eq("author_id", &7));
    assert_eq!(notes.upsert(&note("n1", 42, "Not theirs"), theirs).await.unwrap(), None, "its condition kept it");

    let mine = notes.filter(sql::eq("author_id", &42)).order_by("id", Direction::Desc).limit(2);
    let same = blocking.filter(sql::eq("author_id", &42)).order_by("id", Direction::Desc).limit(2);
    assert_eq!(mine.to_sql().unwrap().text(), same.to_sql().unwrap().text(), "the builder writes one statement");
    assert_eq!(mine.all().await.unwrap(), same.all().unwrap());
    assert_eq!(mine.all().await.unwrap().len(), 2);
    let one = notes.filter(sql::eq("id", "n2"));
    assert_eq!(one.one().await.unwrap(), blocking.filter(sql::eq("id", "n2")).one().unwrap());
    let title = notes.select::<String>("title").filter(sql::eq("id", "n3"));
    assert_eq!(title.scalar::<String>().await.unwrap(), "Water plants");
    assert_eq!(notes.filter(sql::eq("author_id", &42)).count().await.unwrap(), 3);
    assert_eq!(notes.group_by(["author_id"]).count().await.unwrap(), 2);
    let plan = mine.explain().await.unwrap();
    assert!(!plan.is_empty());
    assert_eq!(plan, same.explain().unwrap());

    let (newest, blocking_newest) = (notes.order_by("id", Direction::Desc), blocking.order_by("id", Direction::Desc));
    let first = newest.list(3, None).await.unwrap();
    assert_eq!(first, blocking_newest.list(3, None).unwrap());
    let rest = newest.list(3, first.next.as_deref()).await.unwrap();
    assert_eq!(rest, blocking_newest.list(3, first.next.as_deref()).unwrap());
    assert_eq!((rest.rows.len(), rest.next), (1, None), "the last page");
    let unordered = notes.list(3, None).await.unwrap_err();
    assert_eq!(unordered.kind(), ErrorKind::Invalid, "{unordered}");

    db.exec("insert into authors (author_id, name) values (42, 'Ann'), (7, 'Bob')").await.unwrap();
    let latest = db
        .table::<Titled>("notes as n")
        .select::<Titled>("n.id, n.title")
        .filter("n.author_id = a.author_id")
        .order_by("n.id", Direction::Desc)
        .limit(2);
    let authors = db.table::<Author>("authors as a").include("notes", &latest).order_by("a.author_id", Direction::Asc);
    let blocking_latest = db
        .blocking()
        .table::<Titled>("notes as n")
        .select::<Titled>("n.id, n.title")
        .filter("n.author_id = a.author_id")
        .order_by("n.id", Direction::Desc)
        .limit(2);
    let blocking_authors = db.blocking().table::<Author>("authors as a").include("notes", &blocking_latest);
    let included = authors.all().await.unwrap();
    assert_eq!(included, blocking_authors.order_by("a.author_id", Direction::Asc).all().unwrap());
    assert_eq!(included[1].notes.iter().map(|note| note.id.as_str()).collect::<Vec<_>>(), ["n4", "n2"]);

    let done = notes.filter(sql::eq("author_id", &42)).update([("done", sql::value(&true))]).await.unwrap();
    assert_eq!(done.changes, 3);
    assert_eq!(notes.filter(sql::eq("id", "n3")).delete().await.unwrap().changes, 1);
    assert_eq!(notes.delete().await.unwrap_err().kind(), ErrorKind::Invalid, "every row is said in SQL");
    let everyone = notes.update([("done", sql::value(&false))]).await.unwrap_err();
    assert_eq!(everyone.kind(), ErrorKind::Invalid);
    assert_eq!(ids(&db), ["n1", "n2", "n4"]);

    let spawned = tokio::spawn(notes.filter(sql::eq("done", &true)).all()).await.unwrap().unwrap();
    assert_eq!(spawned.len(), 3);
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn from_and_blocking_cross_between_the_two_apis_on_one_database() {
    let f = fixture();
    let db = f.app();
    db.blocking().exec(insert_note("n1", "written blocking")).unwrap();
    let again = Database::from(db.blocking().clone());
    assert_eq!(again.name(), "app");
    assert_eq!(again.all::<Note>("select * from notes").await.unwrap(), [note("n1", 42, "written blocking")]);
    again.exec(insert_note("n2", "written awaited")).await.unwrap();
    assert_eq!(ids(&db), ["n1", "n2"], "one database, whichever API wrote");

    f.store.close().await.unwrap();
    let closed = db.scalar::<i64>("select 1").await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = db.exec(insert_note("n3", "too late")).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let made_after = Database::from(db.blocking().clone());
    let closed = made_after.tx(|tx| tx.exec(insert_note("n3", "too late"))).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}

#[cfg(feature = "jobs")]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Index {
    note: String,
}

// contract: JOB-02, DUR-06
#[cfg(feature = "jobs")]
#[tokio::test]
async fn a_batch_commits_a_queues_job_with_its_rows_or_neither() {
    let f = fixture();
    let db = f.app();
    let indexing = db.queue::<Index>("indexing").open().unwrap();
    db.batch([insert_note("n1", "Buy milk").into(), indexing.id("index-n1").adding(&Index { note: "n1".to_owned() })])
        .await
        .unwrap();
    assert_eq!(ids(&db), ["n1"]);
    assert!(indexing.get("index-n1").await.unwrap().is_some(), "the job committed with the row");

    let taken = insert_note("n1", "again").into();
    let failed = db.batch([indexing.id("index-n2").adding(&Index { note: "n2".to_owned() }), taken]).await;
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Conflict);
    assert!(indexing.get("index-n2").await.unwrap().is_none(), "the job went back with the batch");
    f.store.close().await.unwrap();
}

// contract: DUR-06
#[cfg(feature = "kv")]
#[tokio::test]
async fn a_bucket_opened_from_the_database_commits_with_its_rows_in_an_awaited_transaction() {
    let f = fixture();
    let db = f.app();
    let sessions = db.bucket::<String>("sessions").open().unwrap();
    let joining = sessions.clone();
    db.tx(move |tx| -> crate::Result<()> {
        tx.exec(insert_note("n1", "Buy milk"))?;
        tx.with(&joining).set("t-1", &"ann".to_owned())
    })
    .await
    .unwrap();
    assert_eq!(sessions.get("t-1").await.unwrap().as_deref(), Some("ann"));

    let joining = sessions.clone();
    let failed = db
        .tx(move |tx| -> crate::Result<()> {
            tx.with(&joining).delete("t-1")?;
            tx.exec(insert_note("n1", "again")).map(drop)
        })
        .await;
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Conflict);
    assert_eq!(sessions.get("t-1").await.unwrap().as_deref(), Some("ann"), "the delete rolled back with the row");
    f.store.close().await.unwrap();
}

// contract: DUR-11
#[cfg(feature = "search")]
#[test]
fn a_tables_index_drops_in_the_databases_line_and_runs_though_its_future_is_dropped_there() {
    let f = fixture();
    let db = f.app();
    let index = |name: &str| db.search_index::<Note>(name).table("notes").field("title", 1.0).open().unwrap();
    let (titles, kept) = (index("notes_search"), index("notes_kept"));
    let runtime = runtime();
    assert!(runtime.block_on(db.drop_search_index("notes_kept")).unwrap(), "there was one");
    assert!(!runtime.block_on(db.drop_search_index("notes_kept")).unwrap(), "and then none");
    let refused = runtime.block_on(kept.search("milk").run()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "a handle of a dropped index is refused: {refused}");

    let mut context = Context::from_waker(Waker::noop());
    let (first, holding, release) = holding_the_line(&db, "n1");
    let mut first = pin!(first);
    assert!(first.as_mut().poll(&mut context).is_pending());
    holding.recv_timeout(Duration::from_secs(10)).expect("the first transaction never began");
    {
        let mut dropping = pin!(db.drop_search_index("notes_search"));
        assert!(dropping.as_mut().poll(&mut context).is_pending(), "it waits in its database's line");
    }
    release.send(()).unwrap();
    runtime.block_on(first.as_mut()).unwrap();
    let after = runtime.block_on(db.drop_search_index("notes_search")).unwrap();
    assert!(!after, "the drop whose future went ran in its turn, before this one");
    let refused = runtime.block_on(titles.search("milk").run()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "a handle of it is refused from then on: {refused}");
    f.store.blocking().close().unwrap();
}
