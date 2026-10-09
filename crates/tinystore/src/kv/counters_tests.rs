use std::sync::Barrier;
use std::thread;

use super::*;
use crate::ErrorKind;
use crate::kv::{self, fixture::*};

/// The ways counters count: each add written, or kept in memory a second.
fn both(store: &Store, name: &str) -> [Counters; 2] {
    let written = store.counters(&format!("{name}-written")).open().unwrap();
    let in_memory = store.counters(&format!("{name}-in-memory")).durability(SECOND).open().unwrap();
    [written, in_memory]
}

#[test]
fn a_counter_adds_up_from_zero_and_is_zero_when_gone() {
    let f = fixture();
    for hits in both(&f.store, "hits") {
        assert_eq!(hits.get("page").unwrap(), 0);
        assert_eq!(hits.add("page", 1).unwrap(), 1);
        assert_eq!(hits.add("page", 4).unwrap(), 5);
        assert_eq!(hits.add("page", -2).unwrap(), 3);
        assert_eq!(hits.under("tenant-7").get("page").unwrap(), 0, "a branch counts apart");
        assert!(hits.delete("page").unwrap());
        assert!(!hits.delete("page").unwrap());
        assert_eq!(hits.get("page").unwrap(), 0);
    }
}

#[test]
fn a_counters_window_starts_at_its_first_add_and_does_not_slide() {
    let f = fixture();
    let written = f.store.counters("attempts-written").ttl(15 * MINUTE).open().unwrap();
    let in_memory = f.store.counters("attempts-in-memory").ttl(15 * MINUTE).durability(SECOND).open().unwrap();
    for attempts in [written.under("ip"), in_memory.under("ip")] {
        for want in 1..=3 {
            assert_eq!(attempts.add("10.0.0.1", 1).unwrap(), want);
            f.clock.advance(5 * MINUTE);
        }
        assert_eq!(attempts.get("10.0.0.1").unwrap(), 0, "fifteen minutes after the first add");
        assert_eq!(attempts.add("10.0.0.1", 1).unwrap(), 1, "an expired counter starts again");
    }
}

#[test]
fn an_overflowing_counter_is_refused_rather_than_rounded() {
    let f = fixture();
    for big in both(&f.store, "big") {
        for (key, start, then) in [("up", i64::MAX - 1, 2), ("down", i64::MIN, -1)] {
            big.add(key, start).unwrap();
            let error = big.add(key, then).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
            assert_eq!(big.get(key).unwrap(), start, "the refused sum left the counter as it was");
        }
    }
}

#[test]
fn adds_from_many_threads_each_count_once() {
    let f = fixture();
    for hits in both(&f.store, "hits") {
        let start = Barrier::new(16);
        thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    start.wait();
                    for _ in 0..20 {
                        hits.add("page", 1).unwrap();
                    }
                });
            }
        });
        assert_eq!(hits.get("page").unwrap(), 320);
    }
    kv::maintain(&f.store).unwrap();
    let in_memory = f.store.counters("hits-in-memory").durability(SECOND).open().unwrap();
    assert_eq!(in_memory.get("page").unwrap(), 320);
}

#[test]
fn counters_in_memory_reach_the_file_at_a_flush_and_at_close() {
    let f = fixture();
    let hits = f.store.counters("hits").durability(SECOND).open().unwrap();
    let commits = f.commits();
    for _ in 0..100 {
        hits.add("page", 1).unwrap();
    }
    assert_eq!(f.commits(), commits, "an add in memory commits nothing");
    assert_eq!(f.rows("_tinystore_kv_cells"), 0);
    assert_eq!(kv::maintain(&f.store).unwrap().flushed, 1);
    assert_eq!(f.rows("_tinystore_kv_cells"), 1);

    hits.add("page", 1).unwrap();
    hits.add("other", 7).unwrap();
    let f = f.reopen();
    let hits = f.store.counters("hits").durability(SECOND).open().unwrap();
    assert_eq!((hits.get("page").unwrap(), hits.get("other").unwrap()), (101, 7), "close wrote what memory held");
}

#[test]
fn counters_in_memory_reach_the_file_when_the_store_is_dropped_unclosed() {
    let f = fixture();
    let hits = f.store.counters("hits").durability(SECOND).open().unwrap();
    hits.add("page", 5).unwrap();
    let Fixture { dir, store, clock } = f;
    drop(store);
    assert_eq!(hits.add("page", 1).unwrap_err().kind(), ErrorKind::Closed, "a handle outliving its store");
    let f = Fixture { store: crate::Store::open(dir.path(), crate::Options::default()).unwrap(), dir, clock };
    assert_eq!(f.store.counters("hits").durability(SECOND).open().unwrap().get("page").unwrap(), 5);
}

#[test]
fn counters_of_a_name_count_one_way_in_a_process() {
    let f = fixture();
    f.store.counters("hits").open().unwrap();
    let error = f.store.counters("hits").durability(SECOND).open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    f.store.counters("views").durability(SECOND).open().unwrap();
    f.store.counters("views").durability(SECOND).open().unwrap();
    assert!(f.store.counters("views").durability(2 * SECOND).open().is_err());
    assert!(f.store.counters("views").open().is_err());
    assert!(f.store.counters("other").durability(crate::Durability::Os).open().is_err());
}

#[test]
fn a_name_holding_counters_opens_as_nothing_else() {
    let f = fixture();
    f.store.counters("hits").open().unwrap().add("x", 1).unwrap();
    let error = f.store.bucket::<i64>("hits").open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    assert_eq!(error.to_string(), "kv bucket hits: the name holds counters, not values: invalid");
}

#[test]
fn a_clear_of_counters_in_memory_never_brings_them_back() {
    let f = fixture();
    let hits = f.store.counters("hits").durability(SECOND).open().unwrap();
    hits.under("a").add("x", 1).unwrap();
    kv::maintain(&f.store).unwrap();
    hits.under("a").add("x", 1).unwrap();
    hits.under("a").under("deeper").add("y", 1).unwrap();
    hits.under("b").add("z", 1).unwrap();

    hits.under("a").clear().unwrap();
    assert_eq!(hits.under("a").get("x").unwrap(), 0);
    kv::maintain(&f.store).unwrap();
    assert_eq!(hits.under("a").get("x").unwrap(), 0, "the flush wrote nothing the clear removed");
    assert_eq!(hits.under("a").under("deeper").get("y").unwrap(), 0);
    assert_eq!(hits.under("b").get("z").unwrap(), 1);
    assert_eq!(hits.under("a").add("x", 1).unwrap(), 1, "a counter after the clear is a new one");
}

#[test]
fn counters_in_memory_stay_within_their_bound() {
    let f = fixture();
    let hits = f.store.counters("hits").durability(SECOND).open().unwrap();
    let buffer = hits.buffer.clone().unwrap();
    buffer.bound(8);
    for n in 0..20 {
        hits.add(n, 1).unwrap();
        assert!(buffer.held() <= 8, "{} counters held past a bound of 8", buffer.held());
    }
    for n in 0..20 {
        assert_eq!(hits.get(n).unwrap(), 1, "counter {n}");
    }
}
