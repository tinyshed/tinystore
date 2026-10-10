//! kv and jobs inside an application's database: where they live, what
//! commits with the rows, and what a transaction refuses.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::sql::Database;
use crate::{Error, ErrorKind, Options, Store, TestClock, sql};

const ORDERS: &str = "create table orders (id integer primary key, total integer not null) strict";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Email {
    order: i64,
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_791_547_200)));
    let store = open(&dir, &clock);
    Fixture { dir, store, clock }
}

fn open(dir: &tempfile::TempDir, clock: &Arc<TestClock>) -> Store {
    let options = Options { clock: Some(clock.clone()), background: false, ..Options::default() };
    Store::open(dir.path(), options).unwrap()
}

impl Fixture {
    fn app(&self) -> Database {
        self.store.database("app").migrations([("0001_orders.sql", ORDERS)]).open().unwrap()
    }

    fn reopen(&mut self) {
        self.store.close().unwrap();
        self.store = open(&self.dir, &self.clock);
    }
}

fn orders(db: &Database) -> i64 {
    db.scalar(sql!("select count(*) from orders")).unwrap()
}

#[test]
fn a_bucket_and_a_queue_opened_from_a_database_live_in_its_file() {
    let mut f = fixture();
    let db = f.app();
    db.bucket::<String>("sessions").open().unwrap().set("t-1", &"ann".to_owned()).unwrap();
    db.queue::<Email>("emails").open().unwrap().id("order-7").add(&Email { order: 7 }).unwrap();
    assert!(!f.dir.path().join("kv.db").exists() && !f.dir.path().join("jobs.db").exists());
    let tables: Vec<String> = db
        .all(sql!("select name from sqlite_master where name in ('_tinystore_kv_cells', '_tinystore_jobs')"))
        .unwrap();
    assert_eq!(tables.len(), 2, "{tables:?}");

    f.reopen();
    let db = f.app();
    let sessions = db.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("t-1").unwrap().as_deref(), Some("ann"));
    assert!(db.queue::<Email>("emails").open().unwrap().get("order-7").unwrap().is_some());
    assert!(
        f.store.bucket::<String>("sessions").open().unwrap().get("t-1").unwrap().is_none(),
        "kv.db is another file"
    );
}

#[test]
fn rows_keys_and_jobs_commit_together_or_not_at_all() {
    let f = fixture();
    let db = f.app();
    let (sessions, emails) =
        (db.bucket::<String>("sessions").open().unwrap(), db.queue::<Email>("emails").open().unwrap());

    db.tx(|tx| -> crate::Result<()> {
        let order: i64 = tx.scalar(sql!("insert into orders (total) values (?) returning id", 70))?;
        assert!(tx.with(&emails).id(format!("order-{order}")).add(&Email { order })?);
        tx.with(&sessions).set("t-1", &"ann".to_owned())?;
        assert!(tx.with(&emails).get("order-1")?.is_some(), "a read inside sees what the transaction wrote");
        Ok(())
    })
    .unwrap();
    assert_eq!(orders(&db), 1);
    assert!(emails.get("order-1").unwrap().is_some());
    assert_eq!(sessions.get("t-1").unwrap().as_deref(), Some("ann"));

    let failed = db.tx(|tx| -> crate::Result<()> {
        tx.exec(sql!("insert into orders (total) values (?)", 80))?;
        tx.with(&emails).id("order-2").add(&Email { order: 2 })?;
        tx.with(&sessions).delete("t-1")?;
        Err(Error::invalid("the card was declined"))
    });
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(orders(&db), 1, "the row rolled back");
    assert!(emails.get("order-2").unwrap().is_none(), "and the job");
    assert_eq!(sessions.get("t-1").unwrap().as_deref(), Some("ann"), "and the delete");
}

#[test]
fn a_worker_runs_a_job_added_in_a_transaction_once_it_commits() {
    let f = fixture();
    let db = f.app();
    let emails = db.queue::<Email>("emails").open().unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&sent);
    let worker = emails
        .work(move |email: Email, _run| -> crate::Result<()> {
            kept.lock().unwrap().push(email.order);
            Ok(())
        })
        .unwrap();
    thread::sleep(Duration::from_millis(50)); // the worker finds nothing and sleeps

    db.tx(|tx| tx.with(&emails).add(&Email { order: 9 })).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while sent.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the worker slept past the commit");
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(*sent.lock().unwrap(), [9]);
    worker.stop();
}

#[test]
fn a_transaction_refuses_a_handle_kept_in_another_file() {
    let f = fixture();
    let db = f.app();
    let in_kv = f.store.bucket::<String>("sessions").open().unwrap();
    let in_db = db.bucket::<String>("sessions").open().unwrap();
    let emails = db.queue::<Email>("emails").open().unwrap();

    let error = db.tx(|tx| tx.with(&in_kv).set("t-1", &"ann".to_owned())).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert!(error.to_string().contains("kept in kv.db, outside this transaction of sql app"), "{error}");

    let error = f.store.tx(|tx| tx.with(&in_db).set("t-1", &"ann".to_owned())).unwrap_err();
    assert!(error.to_string().starts_with("sql app: kv bucket sessions: key \"t-1\": kept in sql app"), "{error}");

    let error = db.tx(|_| emails.add(&Email { order: 1 })).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "a call around the transaction would wait for its writer: {error}");
}

#[test]
fn a_cancel_that_rolls_back_leaves_the_job() {
    let f = fixture();
    let db = f.app();
    let emails = db.queue::<Email>("emails").open().unwrap();
    emails.id("order-7").add(&Email { order: 7 }).unwrap();

    let failed = db.tx(|tx| -> crate::Result<()> {
        assert!(tx.with(&emails).cancel("order-7")?);
        assert!(tx.with(&emails).get("order-7")?.is_none());
        Err(Error::invalid("changed its mind"))
    });
    assert!(failed.is_err());
    assert!(emails.get("order-7").unwrap().is_some());

    db.tx(|tx| tx.with(&emails).cancel("order-7")).unwrap();
    assert!(emails.get("order-7").unwrap().is_none());
}

#[test]
fn the_store_closes_what_a_database_keeps_before_its_file() {
    let mut f = fixture();
    let db = f.app();
    let views = db.bucket::<i64>("views").idle(Duration::from_secs(3600)).open().unwrap();
    views.set("home", &1).unwrap();
    f.clock.advance(Duration::from_secs(600));
    views.get("home").unwrap(); // a renewal waits in memory for the next flush
    let emails = db.queue::<Email>("emails").open().unwrap();
    let _worker = emails.work(|_: Email, _run| -> crate::Result<()> { Ok(()) }).unwrap();

    f.reopen();
    let entry = f.app().bucket::<i64>("views").open().unwrap().entry("home").unwrap().unwrap();
    let expires = entry.expires_at.unwrap().duration_since(f.store.now()).unwrap();
    assert_eq!(expires, Duration::from_secs(3600), "the renewal was written before the file closed");
}
