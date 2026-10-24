use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

use super::*;
use crate::ErrorKind;
use crate::kv::fixture::{Fixture, MINUTE, SECOND, eventually, fixture};

fn awaited(f: &Fixture) -> Store {
    Store::from(f.store.clone())
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// What a sequence of calls answers, for counters written each add and for
/// counters kept in memory.
async fn answers(counters: &Counters) -> (Vec<i64>, Vec<bool>, i64) {
    let mut sums = Vec::new();
    for n in [1, 2, -5] {
        sums.push(counters.add("10.0.0.7", n).await.unwrap());
    }
    sums.push(counters.under("eu").add("10.0.0.7", 3).await.unwrap());
    sums.push(counters.get("10.0.0.7").await.unwrap());
    sums.push(counters.get("unknown").await.unwrap());
    let deleted = vec![counters.delete("10.0.0.7").await.unwrap(), counters.delete("10.0.0.7").await.unwrap()];
    counters.clear().await.unwrap();
    (sums, deleted, counters.under("eu").get("10.0.0.7").await.unwrap())
}

fn answers_blocking(counters: &crate::kv::Counters) -> (Vec<i64>, Vec<bool>, i64) {
    let mut sums = Vec::new();
    for n in [1, 2, -5] {
        sums.push(counters.add("10.0.0.7", n).unwrap());
    }
    sums.push(counters.under("eu").add("10.0.0.7", 3).unwrap());
    sums.push(counters.get("10.0.0.7").unwrap());
    sums.push(counters.get("unknown").unwrap());
    let deleted = vec![counters.delete("10.0.0.7").unwrap(), counters.delete("10.0.0.7").unwrap()];
    counters.clear().unwrap();
    (sums, deleted, counters.under("eu").get("10.0.0.7").unwrap())
}

#[tokio::test]
async fn each_call_answers_as_its_blocking_twin_written_or_in_memory() {
    let f = fixture();
    let store = awaited(&f);
    let written = store.counters("attempts").ttl(15 * MINUTE).open().unwrap();
    let in_memory = store.counters("views").flush_every(SECOND).open().unwrap();
    let blocking_written = f.store.counters("attempts-blocking").ttl(15 * MINUTE).open().unwrap();
    let blocking_in_memory = f.store.counters("views-blocking").flush_every(SECOND).open().unwrap();
    assert_eq!(answers(&written).await, answers_blocking(&blocking_written));
    assert_eq!(answers(&in_memory).await, answers_blocking(&blocking_in_memory));

    written.add("big", i64::MAX).await.unwrap();
    let overflow = written.add("big", 1).await.unwrap_err();
    let blocking = written.blocking().add("big", 1).unwrap_err();
    assert_eq!((overflow.kind(), overflow.to_string()), (blocking.kind(), blocking.to_string()));
    assert_eq!(written.get("big").await.unwrap(), i64::MAX, "a sum past an i64 changes nothing");
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_step_in_memory_answers_in_its_first_poll_and_a_write_queues_there() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    let attempts = store.counters("attempts").open().unwrap();
    views.blocking().add("/notes", 1).unwrap();
    let _writer = f.hold_writer();
    let mut added = pin!(views.add("/notes", 1));
    assert!(matches!(poll_once(added.as_mut()), Poll::Ready(Ok(2))), "a step in memory, the writer held or not");
    let mut read = pin!(views.get("/notes"));
    assert!(matches!(poll_once(read.as_mut()), Poll::Ready(Ok(2))));
    let mut written = pin!(attempts.add("10.0.0.7", 1));
    assert!(poll_once(written.as_mut()).is_pending(), "queued at the writer without waiting for it");
    drop(_writer);
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_step_that_must_flush_memory_first_runs_on_the_pool() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    views.blocking().buffer.as_ref().unwrap().bound(1);
    views.blocking().add("/notes", 1).unwrap();
    assert_eq!(views.blocking().get("/missing").unwrap(), 0, "a read leaves its reader idle");
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let writer = f.hold_writer();
    let mut added = pin!(views.add("/about", 1));
    assert!(poll_once(added.as_mut()).is_pending(), "memory holds its bound: the flush waits for the writer elsewhere");
    drop(writer);
    assert_eq!(runtime.block_on(added).unwrap(), 1);
    assert_eq!(views.blocking().buffer.as_ref().unwrap().held(), 1, "the flush let go of what it wrote");
    assert_eq!(views.blocking().get("/notes").unwrap(), 1, "read back from kv.db");
    f.store.close().unwrap();
}

// contract: DUR-11
#[test]
fn a_step_whose_future_is_dropped_on_the_pool_counts_all_the_same() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    views.blocking().buffer.as_ref().unwrap().bound(1);
    views.blocking().add("/notes", 1).unwrap();
    assert_eq!(views.blocking().get("/missing").unwrap(), 0, "a read leaves its reader idle");
    {
        let _writer = f.hold_writer();
        let mut added = pin!(views.add("/about", 1));
        assert!(poll_once(added.as_mut()).is_pending(), "memory holds its bound: the step went to the pool");
        // dropped while the flush before the step waits for the writer, let go after it
    }
    eventually("the dropped add counted", || views.blocking().get("/about").unwrap() == 1);
    assert_eq!(views.blocking().get("/notes").unwrap(), 1, "flushed before it");
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_step_tried_while_another_call_holds_memory_goes_to_the_pool_rather_than_wait() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    views.blocking().add("/notes", 1).unwrap();
    let buffer = Arc::clone(views.blocking().buffer.as_ref().unwrap());
    let ((held, holding), (release, released)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    // held for ten seconds at most: a poll that waited for it fails the test rather than hang it
    let holder = std::thread::spawn(move || {
        buffer.while_held(|| {
            held.send(()).unwrap();
            let _ = released.recv_timeout(10 * SECOND);
        });
    });
    holding.recv().unwrap();
    let started = Instant::now();
    let mut added = pin!(views.add("/notes", 1));
    assert!(poll_once(added.as_mut()).is_pending(), "refused in the poll, and sent to the pool");
    assert!(started.elapsed() < SECOND, "a step in a poll waited for memory another call holds");
    drop(release);
    holder.join().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(added).unwrap(), 2, "the step refused in the poll changed nothing there");
    f.store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_step_tried_while_a_clear_holds_memory_across_its_commit_goes_to_the_pool() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    views.blocking().add("/notes", 1).unwrap();
    let writer = f.hold_writer();
    let clearing = {
        let views = views.blocking().clone();
        std::thread::spawn(move || views.clear())
    };
    // tried in a poll until the clear holds memory: each try answers at once
    eventually("a step refused while the clear commits", || {
        let started = Instant::now();
        let tried = crate::polled::without_waiting(|| views.blocking().get("/notes"));
        assert!(started.elapsed() < 2 * SECOND, "a step in a poll waited for the clear's commit");
        tried.is_none()
    });
    drop(writer);
    clearing.join().unwrap().unwrap();
    assert_eq!(
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(views.get("/notes")).unwrap(),
        0
    );
    f.store.close().unwrap();
}

// contract: DUR-11
#[test]
fn a_clear_of_counters_in_memory_runs_on_the_pool_to_its_end_though_its_future_is_dropped() {
    let f = fixture();
    let store = awaited(&f);
    let views = store.counters("views").flush_every(SECOND).open().unwrap();
    views.blocking().add("/notes", 1).unwrap();
    {
        let _writer = f.hold_writer();
        let mut cleared = pin!(views.clear());
        assert!(poll_once(cleared.as_mut()).is_pending());
    }
    eventually("the dropped clear ran", || views.blocking().get("/notes").unwrap() == 0);
    f.store.close().unwrap();
}

#[tokio::test]
async fn counters_cross_to_the_blocking_api_and_back_as_the_same_counters() {
    let f = fixture();
    let blocking = f.store.counters("attempts").open().unwrap();
    let attempts = Counters::from(blocking.clone());
    assert_eq!(attempts.add("10.0.0.7", 1).await.unwrap(), 1);
    assert_eq!(blocking.add("10.0.0.7", 1).unwrap(), 2);
    assert_eq!(attempts.blocking().get("10.0.0.7").unwrap(), 2);
    assert_eq!(tokio::spawn(attempts.get("10.0.0.7")).await.unwrap().unwrap(), 2);
    f.store.close().unwrap();
    let closed = attempts.add("10.0.0.7", 1).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}
