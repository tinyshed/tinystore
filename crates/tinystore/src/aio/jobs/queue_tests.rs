//! A queue's awaited writes and reads, its claims, its watch and its walk,
//! each against the blocking call on the same store.

use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Waker};
use std::thread;

use super::super::fixture::{Draft, HOUR, draft, eventually, fixture, next};
use super::*;
use crate::jobs::State;
use crate::testing::{PATIENCE, holds_within};

/// A draft past 512 bytes, which its job keeps in a row of its own.
fn long_draft() -> Draft {
    Draft { chat: 7, text: "a long draft ".repeat(64) }
}

#[tokio::test]
async fn each_call_answers_as_the_blocking_call_does_on_the_same_store() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    let blocking = later.blocking();

    assert!(later.id("chat:42:1").add(&draft(1)).await.unwrap());
    assert!(!later.id("chat:42:1").add(&draft(2)).await.unwrap(), "an id adds once");
    assert!(!blocking.id("chat:42:1").add(&draft(2)).unwrap(), "and so it does blocking");
    blocking.id("chat:42:2").delay(HOUR).add(&draft(2)).unwrap();
    later.set("chat:42:3", &draft(3)).await.unwrap();
    assert!(later.id("chat:42:2").delay(2 * HOUR).update(&draft(20)).await.unwrap());
    assert!(!later.update("chat:42:9", &draft(9)).await.unwrap(), "no job under the id: nothing to update");

    let job = later.get("chat:42:2").await.unwrap();
    assert_eq!(job, blocking.get("chat:42:2").unwrap());
    let job = job.unwrap();
    assert_eq!((job.value, job.state, job.at), (draft(20), State::Scheduled, f.store.now() + 2 * HOUR));
    assert_eq!(later.get("chat:42:9").await.unwrap(), None);

    let filter = Filter::prefix("chat:42:").limit(2);
    assert_eq!(later.list(&filter).await.unwrap(), blocking.list(&filter).unwrap());
    let mut all = later.all(filter.clone());
    let mut walked = Vec::new();
    while let Some(job) = next(&mut all).await {
        walked.push(job.unwrap());
    }
    let blocking_walk: Vec<Job<Draft>> = blocking.all(filter).map(Result::unwrap).collect();
    assert_eq!(walked, blocking_walk);
    assert_eq!(walked.len(), 3, "two pages of a limit of two");

    assert!(later.cancel("chat:42:3").await.unwrap());
    assert!(!later.cancel("chat:42:3").await.unwrap(), "the job is gone");
    assert_eq!(blocking.get("chat:42:3").unwrap(), None);
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_call_refused_before_its_write_answers_as_the_blocking_one() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    let refused = later.id("x").every(HOUR).at(f.store.now()).delay(HOUR).add(&draft(1)).await.unwrap_err();
    let blocking = later.blocking().id("x").every(HOUR).at(f.store.now()).delay(HOUR).add(&draft(1)).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocking.kind(), blocking.to_string()));
    let unnamed = later.call().every(HOUR).add(&draft(1)).await.unwrap_err();
    assert_eq!(unnamed.kind(), ErrorKind::Invalid, "a repeat needs an id: {unnamed}");
    f.store.close().await.unwrap();
}

/// Holds jobs.db's writer on a thread of its own until the sender is used or
/// a second passes, so that no commit of the file can end meanwhile.
fn hold_the_writer(queue: &Queue<Draft>) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (release, held_until) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let engine = Arc::clone(&queue.queue.jobs);
    let holder = thread::spawn(move || {
        let _writer = engine.file().hold_writer();
        holding.send(()).unwrap();
        let _ = held_until.recv_timeout(std::time::Duration::from_secs(1));
    });
    held.recv().unwrap();
    (release, holder)
}

// contract: STORE-10
// contract: DUR-04
// contract: DUR-11
#[test]
fn an_add_polled_once_waits_for_no_commit_and_commits_though_its_future_is_dropped() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    let (release, holder) = hold_the_writer(&later);
    {
        let mut adding = pin!(later.id("held").add(&draft(1)));
        let polled = adding.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        assert!(polled.is_pending(), "the writer is held: the poll returned rather than wait for it");
        let mut cancelling = pin!(later.cancel("another"));
        assert!(cancelling.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    holds_within(PATIENCE, "the add dropped after its poll", || later.blocking().get("held").unwrap().is_some());
    drop(later);
    f.store.blocking().close().unwrap();
}

// contract: DUR-04
// contract: DUR-11
#[tokio::test]
async fn a_write_whose_future_is_never_polled_writes_nothing() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    later.id("kept").add(&draft(1)).await.unwrap();
    drop(later.id("never").add(&draft(2)));
    drop(later.set("never", &draft(2)));
    drop(later.cancel("kept"));
    later.id("after").add(&draft(3)).await.unwrap();
    assert_eq!(later.get("never").await.unwrap(), None, "no write was polled");
    assert!(later.get("kept").await.unwrap().is_some(), "the cancel was never polled");
    f.store.close().await.unwrap();
}

// contract: STORE-09
#[test]
fn a_get_whose_future_is_dropped_while_it_waits_for_a_reader_stops_at_once() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    later.blocking().id("a").add(&draft(1)).unwrap();
    let engine = Arc::clone(&later.queue.jobs);
    // Sixteen reads held open take every reader jobs.db opens, and wait for one.
    let gate = Arc::new(std::sync::RwLock::new(()));
    let closed = gate.write().unwrap();
    let holders: Vec<_> = (0..16)
        .map(|_| {
            let (engine, gate) = (Arc::clone(&engine), Arc::clone(&gate));
            thread::spawn(move || {
                engine.file().read(|_| {
                    drop(gate.read().unwrap());
                    Ok(())
                })
            })
        })
        .collect();
    let file = engine.file();
    holds_within(PATIENCE, "every holder reads or waits", || file.readers_open() + file.readers_waiting() == 16);
    let before = file.readers_waiting();
    {
        let mut getting = pin!(later.get("a"));
        assert!(getting.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        holds_within(PATIENCE, "the get waits for a reader on the pool", || file.readers_waiting() == before + 1);
    }
    let dropped = std::time::Instant::now();
    holds_within(PATIENCE, "the dropped get left the readers' line", || file.readers_waiting() == before);
    assert!(dropped.elapsed() < std::time::Duration::from_secs(5), "it stopped rather than waited out its patience");
    drop(closed);
    for holder in holders {
        holder.join().unwrap().unwrap();
    }
    drop(later);
    f.store.blocking().close().unwrap();
}

#[tokio::test]
async fn a_claim_and_its_settlement_answer_as_the_blocking_ones_do() {
    let f = fixture();
    let imports = f.store.queue::<Draft>("imports").open().unwrap();
    assert!(imports.claim().await.unwrap().is_none(), "nothing due: none, without waiting");

    imports.id("short").add(&draft(1)).await.unwrap();
    imports.id("long").add(&long_draft()).await.unwrap();
    let short = imports.claim().await.unwrap().unwrap();
    assert_eq!((&short.value, short.run().id(), short.run().attempt()), (&draft(1), Some("short"), 1));
    assert_eq!(imports.get("short").await.unwrap().unwrap().state, State::Running);
    short.settle(()).await.unwrap();
    assert_eq!(imports.get("short").await.unwrap(), None, "done");

    let long = imports.claim().await.unwrap().unwrap();
    assert_eq!(long.value, long_draft(), "a value in a row of its own is read after the claim");
    let later = long.run().retry(HOUR);
    long.settle(later).await.unwrap();
    let job = imports.blocking().get("long").unwrap().unwrap();
    assert_eq!((job.state, job.attempt, job.at), (State::Scheduled, 1, f.store.now() + HOUR));

    imports.id("again").add(&draft(2)).await.unwrap();
    let blocking = imports.blocking().claim().unwrap().unwrap();
    assert_eq!(blocking.value, draft(2));
    assert!(imports.claim().await.unwrap().is_none(), "the blocking claim holds the one job due");
    f.store.close().await.unwrap();
}

// contract: JOB-05
#[tokio::test]
async fn a_settlement_whose_lease_another_claim_took_is_a_conflict_and_changes_nothing() {
    let f = fixture();
    let imports = f.store.queue::<Draft>("imports").timeout(HOUR).open().unwrap();
    imports.id("a").add(&draft(1)).await.unwrap();
    let first = imports.claim().await.unwrap().unwrap();
    f.advance(2 * HOUR);
    let second = imports.claim().await.unwrap().unwrap();
    assert_eq!(second.run().attempt(), 2, "the first lease ended");
    let conflict = first.settle(()).await.unwrap_err();
    assert_eq!(conflict.kind(), ErrorKind::Conflict, "{conflict}");
    assert!(conflict.to_string().contains("jobs queue imports: id \"a\""), "{conflict}");
    assert_eq!(imports.get("a").await.unwrap().unwrap().state, State::Running, "the second claim runs it still");
    second.settle(()).await.unwrap();
    f.store.close().await.unwrap();
}

#[test]
fn a_settlement_polled_once_lets_its_job_go_though_its_future_is_dropped() {
    let f = fixture();
    let imports = f.store.queue::<Draft>("imports").open().unwrap();
    imports.blocking().id("a").add(&draft(1)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let claimed = runtime.block_on(imports.claim()).unwrap().unwrap();
    {
        let mut settling = pin!(claimed.settle(()));
        let _ = settling.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    holds_within(PATIENCE, "the settlement dropped after its poll", || imports.blocking().get("a").unwrap().is_none());
    drop(imports);
    f.store.blocking().close().unwrap();
}

// contract: JOB-12
#[tokio::test]
async fn a_claimed_value_that_no_longer_reads_fails_its_job_and_the_claim_takes_the_next() {
    let f = fixture();
    let imports = f.store.queue::<Draft>("imports").open().unwrap();
    let as_text = f.store.queue::<String>("imports").open().unwrap();
    as_text.id("old").add(&"a value of an older type".to_owned()).await.unwrap();
    imports.id("new").add(&draft(1)).await.unwrap();
    let claimed = imports.claim().await.unwrap().unwrap();
    assert_eq!(claimed.run().id(), Some("new"));
    let old = imports.blocking().get("new").unwrap();
    assert_eq!(old.map(|job| job.state), Some(State::Running));
    let failed = as_text.get("old").await.unwrap().unwrap();
    assert_eq!(failed.state, State::Failed);
    assert!(failed.error.unwrap().contains("no longer reads"), "its error says why");
    claimed.settle(()).await.unwrap();
    f.store.close().await.unwrap();
}

// contract: JOB-02
#[tokio::test]
async fn a_queue_joins_an_awaited_transaction_through_its_blocking_calls() {
    let f = fixture();
    let later = f.store.queue::<Draft>("later").open().unwrap();
    let queue = later.clone();
    let added = f.store.tx(move |tx| tx.with(&queue).id("in-tx").add(&draft(1))).await.unwrap();
    assert!(added);
    assert!(later.get("in-tx").await.unwrap().is_some(), "it committed with the transaction");

    let queue = later.clone();
    let rolled_back = f
        .store
        .tx(move |tx| -> Result<()> {
            tx.with(&queue).id("rolled-back").add(&draft(2))?;
            Err(crate::Error::invalid("the order was refused"))
        })
        .await;
    assert!(rolled_back.is_err());
    assert_eq!(later.get("rolled-back").await.unwrap(), None, "nothing of a transaction rolled back");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn from_and_blocking_cross_between_the_two_apis_on_one_queue() {
    let f = fixture();
    let blocking = f.store.blocking().queue::<Draft>("later").open().unwrap();
    let later = Queue::from(blocking.clone());
    blocking.id("written").add(&draft(1)).unwrap();
    assert_eq!(later.get("written").await.unwrap(), blocking.get("written").unwrap());
    assert_eq!(format!("{:?}", later.blocking()), format!("{blocking:?}"));

    let gone = {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path(), crate::Options::default()).unwrap();
        let queue = store.queue::<Draft>("later").open().unwrap();
        store.close().unwrap();
        Queue::from(queue)
    };
    let closed = gone.get("x").await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = gone.id("x").add(&draft(1)).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_watch_yields_the_job_each_time_it_changes_and_ends_with_it() {
    let f = fixture();
    let pushes = f.store.queue::<Draft>("pushes").open().unwrap();
    pushes.id("p").delay(HOUR).add(&draft(1)).await.unwrap();
    let mut watch = pushes.watch("p");
    assert_eq!(next(&mut watch).await.unwrap().unwrap().state, State::Scheduled);
    pushes.id("p").at(f.store.now()).update(&draft(2)).await.unwrap();
    assert_eq!(next(&mut watch).await.unwrap().unwrap().state, State::Waiting);
    let claimed = pushes.claim().await.unwrap().unwrap();
    assert_eq!(next(&mut watch).await.unwrap().unwrap().state, State::Running);
    claimed.settle(()).await.unwrap();
    assert_eq!(next(&mut watch).await.unwrap().unwrap().state, State::Done);
    assert!(next(&mut watch).await.is_none(), "it ends with its job");

    pushes.id("q").delay(HOUR).add(&draft(3)).await.unwrap();
    let mut cancelled = pushes.watch("q");
    assert_eq!(next(&mut cancelled).await.unwrap().unwrap().state, State::Scheduled);
    pushes.cancel("q").await.unwrap();
    assert_eq!(next(&mut cancelled).await.unwrap().unwrap().state, State::Cancelled);
    assert!(next(&mut cancelled).await.is_none());
    assert!(next(&mut pushes.watch("nothing")).await.is_none(), "an id with no job yields nothing");
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_dropped_watch_stops_watching() {
    let f = fixture();
    let pushes = f.store.queue::<Draft>("pushes").open().unwrap();
    pushes.id("p").delay(HOUR).add(&draft(1)).await.unwrap();
    let mut watch = pushes.watch("p");
    let watchers = || format!("{:?}", pushes.queue.state.watchers);
    assert_eq!(watchers(), "Watchers(0)", "it watches from its first poll");
    next(&mut watch).await.unwrap().unwrap();
    assert_eq!(watchers(), "Watchers(1)");
    drop(watch);
    eventually("the watch gave its place up", || watchers() == "Watchers(0)").await;
    f.store.close().await.unwrap();
}

#[tokio::test]
async fn a_watch_and_a_walk_end_with_closed_when_the_store_closes() {
    let f = fixture();
    let pushes = f.store.queue::<Draft>("pushes").open().unwrap();
    for chat in [1, 2] {
        pushes.id(format!("chat:{chat}")).delay(HOUR).add(&draft(chat)).await.unwrap();
    }
    let mut watch = pushes.watch("chat:1");
    assert_eq!(next(&mut watch).await.unwrap().unwrap().state, State::Scheduled);
    let mut all = pushes.all(Filter::prefix("chat:").limit(1));
    assert_eq!(next(&mut all).await.unwrap().unwrap().id.as_deref(), Some("chat:1"));

    f.store.close().await.unwrap();
    let closed = next(&mut watch).await.unwrap().unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(next(&mut watch).await.is_none(), "the watch ends");
    let closed = next(&mut all).await.unwrap().unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(next(&mut all).await.is_none(), "the walk ends");
}
