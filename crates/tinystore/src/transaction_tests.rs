//! Transactions as a program meets them: what a call that ends one leaves,
//! and what a call that would wait for the writer it holds answers.
#![cfg(any(feature = "kv", feature = "sql"))]

use std::sync::mpsc;
use std::time::Duration;

use crate::{ErrorKind, Options, Store};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    (dir, store)
}

/// Runs `scenario` on a thread of its own, and fails the test when it has
/// not ended within a minute, as a call waiting for its own writer never does.
fn within_a_minute<T: Send + 'static>(scenario: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, ended) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(scenario());
    });
    ended.recv_timeout(Duration::from_secs(60)).expect("it waited for good")
}

// contract: DUR-02, SQL-07
#[cfg(feature = "sql")]
#[test]
fn a_call_after_one_that_rolled_the_transaction_back_fails_and_so_does_its_commit() {
    let (_dir, store) = store();
    let db = store.database("app").migrations([("0001_t.sql", "create table t (v text unique)")]).open().unwrap();
    db.exec("insert into t values ('taken')").unwrap();
    let ended = db.tx(|tx| -> crate::Result<()> {
        tx.exec("insert into t values ('inside')")?;
        let rolled_back = tx.exec("insert or rollback into t values ('taken')").unwrap_err();
        assert_eq!(rolled_back.kind(), ErrorKind::Conflict, "{rolled_back}");
        // a function that catches the failure and goes on
        let after = tx.exec("insert into t values ('after')").unwrap_err();
        assert_eq!(after.kind(), ErrorKind::Invalid, "{after}");
        Ok(())
    });
    let error = ended.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert_eq!(db.all::<String>("select v from t").unwrap(), ["taken"], "nothing of the transaction stays");
    db.exec("insert into t values ('next')").unwrap();
}

// contract: DUR-07
#[cfg(feature = "kv")]
#[test]
fn a_close_from_inside_the_stores_transaction_fails_rather_than_wait_for_its_writer() {
    let (_dir, store) = store();
    let stock = store.bucket::<i64>("stock").open().unwrap();
    let inside = store.clone();
    let refused = within_a_minute(move || {
        inside.tx(|tx| -> crate::Result<ErrorKind> {
            tx.with(&stock).set("sku-1", &1)?;
            Ok(inside.close().unwrap_err().kind())
        })
    });
    assert_eq!(refused.unwrap(), ErrorKind::Invalid);
    assert_eq!(store.bucket::<i64>("stock").open().unwrap().get("sku-1").unwrap(), Some(1), "the store goes on");
    store.close().unwrap();
}

// contract: DUR-07
#[cfg(feature = "sql")]
#[test]
fn a_close_from_inside_a_databases_transaction_fails_rather_than_wait_for_its_writer() {
    let (_dir, store) = store();
    let db = store.database("app").migrations([("0001_t.sql", "create table t (v text)")]).open().unwrap();
    let inside = store.clone();
    let refused = within_a_minute(move || {
        db.tx(|tx| -> crate::Result<ErrorKind> {
            tx.exec("insert into t values ('a')")?;
            Ok(inside.close().unwrap_err().kind())
        })
    });
    assert_eq!(refused.unwrap(), ErrorKind::Invalid);
    store.close().unwrap();
}
