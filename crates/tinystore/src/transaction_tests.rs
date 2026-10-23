//! Transactions as a program meets them: what a call that ends one leaves.
#![cfg(feature = "sql")]

use crate::{ErrorKind, Options, Store};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    (dir, store)
}

// contract: DUR-02, SQL-07
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
