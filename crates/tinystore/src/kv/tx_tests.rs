use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Barrier;
use std::thread;

use super::*;
use crate::kv::fixture::*;
use crate::{ErrorKind, Options};

#[test]
fn a_transaction_commits_what_it_wrote_or_nothing() {
    let f = fixture();
    let accounts = f.store.bucket::<i64>("accounts").open().unwrap();
    let log = f.store.bucket::<String>("log").open().unwrap();
    f.store
        .tx(|tx| -> Result<()> {
            tx.with(&accounts).set("a", &10)?;
            tx.with(&log).set("1", &"opened a".to_owned())
        })
        .unwrap();
    assert_eq!((accounts.get("a").unwrap(), log.get("1").unwrap().as_deref()), (Some(10), Some("opened a")));

    let refused = f.store.tx(|tx| -> Result<()> {
        tx.with(&log).set("2", &"moved a".to_owned())?;
        Err(Error::invalid("changed its mind"))
    });
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::Invalid);
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        f.store.tx(|tx| -> Result<()> {
            tx.with(&log).set("3", &"moved a".to_owned())?;
            panic!("a bug")
        })
    }));
    assert!(panicked.is_err());
    assert_eq!((log.get("2").unwrap(), log.get("3").unwrap()), (None, None));
}

#[test]
fn a_transaction_reads_its_own_writes() {
    let f = fixture();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    let seen = f
        .store
        .tx(|tx| -> Result<_> {
            tx.with(&notes).set("b", &"written".to_owned())?;
            let written = tx.with(&notes).get("b")?;
            tx.with(&notes).delete("b")?;
            Ok((written, tx.with(&notes).has("b")?))
        })
        .unwrap();
    assert_eq!(seen, (Some("written".to_owned()), false));
}

#[test]
fn of_two_transactions_taking_the_last_item_one_gets_it() {
    let f = fixture();
    let stock = f.store.bucket::<i64>("stock").open().unwrap();
    let orders = f.store.bucket::<String>("orders").open().unwrap();
    stock.set("sku", &1).unwrap();
    let start = Barrier::new(8);
    let placed = thread::scope(|scope| {
        let buyers: Vec<_> = (0..8)
            .map(|buyer| {
                let (stock, orders, start) = (&stock, &orders, &start);
                let store = &f.store;
                scope.spawn(move || {
                    start.wait();
                    store.tx(|tx| -> Result<bool> {
                        let left = tx.with(&stock).get("sku")?.unwrap_or(0);
                        if left < 1 {
                            return Ok(false);
                        }
                        tx.with(&stock).set("sku", &(left - 1))?;
                        tx.with(&orders).set(buyer, &"sku".to_owned())?;
                        Ok(true)
                    })
                })
            })
            .collect();
        buyers.into_iter().map(|buyer| buyer.join().unwrap().unwrap()).filter(|placed| *placed).count()
    });
    assert_eq!(placed, 1);
    assert_eq!(stock.get("sku").unwrap(), Some(0));
    assert_eq!(orders.all().count(), 1);
}

#[test]
fn a_call_made_around_a_transaction_from_inside_it_fails_rather_than_waits() {
    let f = fixture();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    let error = f.store.tx(|_| -> Result<()> { notes.set("a", &"outside the transaction".to_owned()) }).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(error.to_string().contains("make it through the transaction"), "{error}");
    let read = f.store.tx(|_| notes.get("a")).unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "a read around it would miss what it wrote: {read}");
    let nested = f.store.tx(|_| f.store.tx(|_| Ok::<_, Error>(()))).unwrap_err();
    assert_eq!(nested.kind(), ErrorKind::Invalid);
}

#[test]
fn a_take_that_fails_inside_a_transaction_keeps_its_value_and_the_rest_goes_on() {
    let f = fixture();
    f.store.bucket::<String>("mixed").open().unwrap().set("a", &"text".to_owned()).unwrap();
    let as_numbers = f.store.bucket::<i64>("mixed").open().unwrap();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    f.store
        .tx(|tx| -> Result<()> {
            let taken = tx.with(&as_numbers).take("a");
            assert_eq!(taken.unwrap_err().kind(), ErrorKind::Corrupt);
            tx.with(&notes).set("after", &"kept".to_owned())
        })
        .unwrap();
    assert_eq!(f.store.bucket::<String>("mixed").open().unwrap().get("a").unwrap().as_deref(), Some("text"));
    assert_eq!(notes.get("after").unwrap().as_deref(), Some("kept"));
}

#[test]
fn counters_join_a_transaction_unless_they_live_in_memory() {
    let f = fixture();
    let hits = f.store.counters("hits").open().unwrap();
    let refused = f.store.tx(|tx| -> Result<()> {
        assert_eq!(tx.with(&hits).add("k", 5)?, 5);
        assert_eq!(tx.with(&hits).get("k")?, 5);
        Err(Error::invalid("rolled back"))
    });
    assert!(refused.is_err());
    assert_eq!(hits.get("k").unwrap(), 0, "the add rolled back with it");

    let in_memory = f.store.counters("views").durability(SECOND).open().unwrap();
    let error = f.store.tx(|tx| tx.with(&in_memory).add("k", 1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[test]
fn a_handle_of_another_store_is_refused() {
    let f = fixture();
    let other_dir = tempfile::tempdir().unwrap();
    let other = Store::open(other_dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let elsewhere = other.bucket::<i64>("notes").open().unwrap();
    let error = f.store.tx(|tx| tx.with(&elsewhere).set("a", &1)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    other.close().unwrap();
}

#[test]
fn a_key_call_inside_a_transaction_commits_with_it() {
    let f = fixture();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    notes.set("a", &"one".to_owned()).unwrap();
    let seen = notes.entry("a").unwrap().unwrap().version;
    let stale = f.store.tx(|tx| -> Result<()> {
        tx.with(&notes).key("b").ttl(MINUTE).set(&"new".to_owned())?;
        tx.with(&notes).key("a").if_version(seen).set(&"two".to_owned())?;
        tx.with(&notes).key("a").if_version(seen).set(&"three".to_owned())
    });
    assert_eq!(stale.unwrap_err().kind(), ErrorKind::Conflict, "the second set found the first one's version");
    assert_eq!((notes.get("a").unwrap().as_deref(), notes.get("b").unwrap()), (Some("one"), None), "all rolled back");
}
