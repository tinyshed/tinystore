//! Awaited holds of a lock's and a semaphore's keys, beside blocking holders
//! of the same store.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread;

use super::*;
use crate::kv::locks::Locks;
use crate::testing::{PATIENCE, holds_within};
use crate::{ErrorKind, Options};

fn opened(dir: &tempfile::TempDir) -> crate::aio::Store {
    crate::aio::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap()
}

fn locks(store: &crate::aio::Store) -> Arc<Locks> {
    Locks::of(store.blocking()).unwrap()
}

/// Holds `key` through the blocking lock on a thread of its own until told,
/// saying once it holds; answers its lease's fence.
fn held_elsewhere(lock: &Lock, key: &'static str) -> (mpsc::Sender<()>, thread::JoinHandle<Result<Fence>>) {
    let (holds, held) = mpsc::channel();
    let (go, told) = mpsc::channel::<()>();
    let lock = lock.blocking().clone();
    let holder = thread::spawn(move || {
        lock.hold(key, |lease| {
            holds.send(()).unwrap();
            told.recv_timeout(PATIENCE).unwrap();
            Ok(lease.fence())
        })
    });
    held.recv_timeout(PATIENCE).unwrap();
    (go, holder)
}

fn poll<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    std::pin::Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

/// Whether `key` is free, asked from another thread: the one that polls a
/// hold's work is marked as holding its key, and a hold of it there is refused.
fn free_elsewhere(key: &'static str, lock: &crate::locks::Lock) -> bool {
    let lock = lock.clone();
    thread::spawn(move || lock.try_hold(key, |_| Ok::<_, Error>(())).unwrap().is_some()).join().unwrap()
}

// contract: LOCK-01, LOCK-02
#[tokio::test]
async fn an_awaited_hold_answers_what_its_work_answers_and_holds_the_key_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let before = invoices.blocking().hold("42", |lease| Ok::<_, Error>(lease.fence())).unwrap();

    let blocking = invoices.blocking().clone();
    let (fence, free) = invoices
        .hold("42", |lease| async move { Ok::<_, Error>((lease.fence(), free_elsewhere("42", &blocking))) })
        .await
        .unwrap();
    assert!(!free, "nobody else held the key while work ran");
    assert!(fence > before, "a later lease's fence is larger: {before:?} then {fence:?}");
    assert_eq!(invoices.try_hold("42", |_| async { Ok::<_, Error>("free") }).await.unwrap(), Some("free"));

    let refused = || Error::invalid("the charge was refused");
    let failed = invoices.hold("42", move |_| async move { Err::<(), _>(refused()) }).await;
    assert_eq!(failed.unwrap_err().to_string(), refused().to_string(), "work's own error, as in blocking Rust");
    assert!(invoices.blocking().try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some(), "a failed hold lets go");
    store.close().await.unwrap();
}

// contract: STORE-10, LOCK-01
#[tokio::test]
async fn a_hold_of_a_held_key_parks_no_thread_and_waiters_are_served_in_the_order_they_came() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let reports = store.lock("reports").open().unwrap();
    let (go, first) = held_elsewhere(&reports, "sales");

    let order = Arc::new(Mutex::new(Vec::new()));
    let mut waiting = Vec::new();
    for holder in ["second", "third", "fourth"] {
        let order = Arc::clone(&order);
        let mut hold = Box::pin(reports.hold("sales", move |_| async move {
            order.lock().unwrap().push(holder);
            Ok::<_, Error>(())
        }));
        assert!(poll(&mut hold).is_pending(), "{holder} waits in line, and this thread goes on");
        waiting.push(tokio::spawn(hold));
    }
    assert_eq!(locks(&store).waiting(), 3);
    go.send(()).unwrap();
    first.join().unwrap().unwrap();
    for hold in waiting {
        hold.await.unwrap().unwrap();
    }
    assert_eq!(*order.lock().unwrap(), ["second", "third", "fourth"]);
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_hold_past_its_wait_is_in_use_as_the_blocking_one_and_try_hold_answers_none_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let (go, holder) = held_elsewhere(&invoices, "42");

    let wait = Duration::from_millis(50);
    let busy = invoices.key("42").wait(wait).hold(|_| async { Ok::<_, Error>(()) }).await.unwrap_err();
    let blocking_busy = invoices.blocking().key("42").wait(wait).hold(|_| Ok::<_, Error>(())).unwrap_err();
    assert_eq!(busy.kind(), ErrorKind::InUse, "{busy}");
    assert_eq!(busy.to_string(), blocking_busy.to_string());

    let mut tried = Box::pin(invoices.try_hold("42", |_| async { Ok::<_, Error>(()) }));
    assert!(matches!(poll(&mut tried), Poll::Ready(Ok(None))), "answered in its first poll, without waiting");
    assert_eq!(locks(&store).waiting(), 0);
    go.send(()).unwrap();
    holder.join().unwrap().unwrap();
    store.close().await.unwrap();
}

// contract: LOCK-06
#[tokio::test]
async fn a_dropped_hold_leaves_the_line_and_one_dropped_while_holding_lets_the_key_go() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let (go, holder) = held_elsewhere(&invoices, "42");
    let mut waiting = Box::pin(invoices.hold("42", |_| async { Ok::<_, Error>(()) }));
    assert!(poll(&mut waiting).is_pending());
    assert_eq!(locks(&store).waiting(), 1);
    drop(waiting);
    assert_eq!(locks(&store).waiting(), 0, "a dropped hold left the line");

    let mut given = Box::pin(invoices.hold("42", |_| async { Ok::<_, Error>(()) }));
    assert!(poll(&mut given).is_pending());
    go.send(()).unwrap();
    holder.join().unwrap().unwrap(); // its release gave the key to the hold, which nobody polls again
    drop(given);
    assert!(invoices.blocking().try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some(), "a grant never taken goes");

    let mut holding = Box::pin(invoices.hold("42", |_| std::future::pending::<Result<(), Error>>()));
    assert!(poll(&mut holding).is_pending(), "work waits for ever, holding the key");
    assert!(invoices.blocking().try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_none());
    drop(holding);
    assert!(
        invoices.blocking().try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some(),
        "work dropped, the key went"
    );

    let timed_out = tokio::time::timeout(
        Duration::from_millis(20),
        invoices.hold("42", |_| std::future::pending::<Result<(), Error>>()),
    );
    assert!(timed_out.await.is_err(), "the runtime's timeout dropped the hold");
    assert!(invoices.blocking().try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some());
    store.close().await.unwrap();
}

// contract: LOCK-03
#[tokio::test]
async fn a_hold_past_max_hold_loses_its_lease_and_fails_lost_though_work_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").max_hold(Duration::from_millis(50)).open().unwrap();
    let blocking = invoices.blocking().clone();
    let lost = invoices
        .hold("42", |lease| async move {
            holds_within(PATIENCE, "the lease revoked", || lease.lost());
            Ok::<_, Error>(free_elsewhere("42", &blocking))
        })
        .await
        .unwrap_err();
    assert_eq!(lost.kind(), ErrorKind::Lost, "{lost}");
    store.close().await.unwrap();
}

// contract: LOCK-04
#[tokio::test]
async fn a_hold_of_a_key_inside_its_own_hold_or_inside_a_transaction_is_refused_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let inner = invoices.clone();
    let refused = invoices
        .hold("42", |_| async move {
            let nested = inner.hold("42", |_| async { Ok::<_, Error>(()) }).await.unwrap_err();
            let other_key = inner.hold("43", |_| async { Ok::<_, Error>(()) }).await;
            let blocking = inner.blocking().hold("42", |_| Ok::<_, Error>(())).unwrap_err();
            Ok::<_, Error>((nested, other_key.is_ok(), blocking.kind()))
        })
        .await
        .unwrap();
    let (nested, other_key, blocking) = refused;
    assert_eq!(nested.kind(), ErrorKind::Invalid, "{nested}");
    assert!(nested.to_string().contains("inside its own hold"), "{nested}");
    assert!(other_key, "another key of the lock is held inside");
    assert_eq!(blocking, ErrorKind::Invalid, "a blocking hold in a poll of work is refused too");

    let spawned = invoices.clone();
    let outer = invoices
        .hold("42", |_| async move {
            let task = tokio::spawn(async move {
                spawned.key("42").wait(Duration::from_millis(20)).hold(|_| async { Ok::<_, Error>(()) }).await
            });
            Ok::<_, Error>(task.await.unwrap().unwrap_err().kind())
        })
        .await
        .unwrap();
    assert_eq!(outer, ErrorKind::InUse, "a hold in a task of its own waits as another holder");
    store.close().await.unwrap();
}

// contract: LOCK-04
#[test]
fn an_awaited_hold_inside_a_transactions_closure_is_refused_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let bucket = store.blocking().bucket::<u32>("orders").open().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let in_tx = store.blocking().tx(|tx| -> Result<ErrorKind> {
        tx.with(&bucket).set("o-1", &1)?;
        let refused = runtime.block_on(invoices.hold("42", |_| async { Ok::<_, Error>(()) })).unwrap_err();
        Ok(refused.kind())
    });
    assert_eq!(in_tx.unwrap(), ErrorKind::Invalid);
    assert_eq!(bucket.get("o-1").unwrap(), Some(1), "the transaction around it went on and committed");
    store.blocking().close().unwrap();
}

// contract: LOCK-06
#[tokio::test]
async fn the_stores_close_answers_every_waiter_closed_and_ends_every_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let (entered, holding) = tokio::sync::oneshot::channel();
    let (closed, told) = tokio::sync::oneshot::channel::<()>();
    let held = tokio::spawn(invoices.hold("42", |lease| async move {
        entered.send(()).unwrap();
        told.await.unwrap();
        Ok::<_, Error>(lease.lost())
    }));
    holding.await.unwrap();
    let mut waiting = Box::pin(invoices.hold("42", |_| async { Ok::<_, Error>(()) }));
    assert!(poll(&mut waiting).is_pending());

    store.close().await.unwrap();
    let error = waiting.await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
    closed.send(()).unwrap();
    let ended = held.await.unwrap().unwrap_err();
    assert_eq!(ended.kind(), ErrorKind::Closed, "the lease ended with the store: {ended}");
}

#[tokio::test]
async fn a_lease_or_a_permit_kept_past_its_hold_says_it_is_lost() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let invoices = store.lock("invoices").open().unwrap();
    let (live, lease) =
        invoices.hold("42", |lease| async move { Ok::<_, Error>((!lease.lost(), lease)) }).await.unwrap();
    assert!(live, "live while its hold ran, and the hold answered what work answered");
    let next = invoices.try_hold("42", |next| async move { Ok::<_, Error>(next.fence()) }).await.unwrap();
    assert!(next.is_some_and(|fence| fence > lease.fence()), "the key went to another holder");
    assert!(lease.lost(), "the lease returned from work says it is lost");

    let (go, told) = tokio::sync::oneshot::channel::<()>();
    let task = invoices
        .hold("43", |lease| async move {
            Ok::<_, Error>(tokio::spawn(async move {
                told.await.unwrap();
                lease.lost()
            }))
        })
        .await
        .unwrap();
    go.send(()).unwrap();
    assert!(task.await.unwrap(), "a lease moved into a task that work spawned says it is lost after the hold");

    let uploads = store.semaphore("uploads").limit(1).open().unwrap();
    let permit = uploads.hold("u-1", |permit| async move { Ok::<_, Error>(permit) }).await.unwrap();
    assert!(uploads.try_hold("u-1", |_| async { Ok::<_, Error>(()) }).await.unwrap().is_some());
    assert!(permit.lost(), "the permit returned from work says it is lost");
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_semaphore_lets_its_limit_hold_a_key_at_once_and_the_next_waits() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let uploads = store.semaphore("uploads").limit(2).open().unwrap();
    let (go, first) = {
        let (holds, held) = mpsc::channel();
        let (go, told) = mpsc::channel::<()>();
        let blocking = uploads.blocking().clone();
        let holder = thread::spawn(move || {
            blocking.hold("u-1", |_| {
                holds.send(()).unwrap();
                told.recv_timeout(PATIENCE).unwrap();
                Ok::<_, Error>(())
            })
        });
        held.recv_timeout(PATIENCE).unwrap();
        (go, holder)
    };
    let second = uploads.hold("u-1", |permit| async move { Ok::<_, Error>(permit.lost()) }).await.unwrap();
    assert!(!second, "a second of the limit of two held beside the first");

    let blocking = uploads.blocking().clone();
    let full = move || thread::spawn(move || blocking.try_hold("u-1", |_| Ok::<_, Error>(())).unwrap().is_none());
    let third = uploads.hold("u-1", |_| async move { Ok::<_, Error>(full().join().unwrap()) }).await;
    assert!(third.unwrap(), "two held it: a third found it full");
    let mut waiting = Box::pin(uploads.key("u-1").wait(PATIENCE).hold(|_| async { Ok::<_, Error>(()) }));
    let mut full = Box::pin(uploads.hold("u-1", |_| std::future::pending::<Result<(), Error>>()));
    assert!(poll(&mut full).is_pending());
    assert!(poll(&mut waiting).is_pending(), "the limit held: the next waits");
    assert_eq!(uploads.try_hold("u-1", |_| async { Ok::<_, Error>(()) }).await.unwrap(), None);
    drop(full);
    go.send(()).unwrap();
    first.join().unwrap().unwrap();
    waiting.await.unwrap();
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_blocking_lock_crosses_to_the_async_api_and_its_holds_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let blocking = store.blocking().lock("invoices").open().unwrap();
    let invoices = Lock::from(blocking.clone());
    // spawned, so that the future is Send and 'static, as a program's runtime asks
    let fence = tokio::spawn(invoices.hold("42", |lease| async move { Ok::<_, Error>(lease.fence()) }));
    let fence = fence.await.unwrap().unwrap();
    let later = invoices.blocking().hold("42", |lease| Ok::<_, Error>(lease.fence())).unwrap();
    assert!(later > fence);
    assert!(blocking.try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some());

    let semaphore = Semaphore::from(store.blocking().semaphore("uploads").limit(1).open().unwrap());
    let permit = tokio::spawn(semaphore.hold("u-1", |permit| async move { Ok::<_, Error>(permit.lost()) }));
    assert!(!permit.await.unwrap().unwrap());
    store.close().await.unwrap();
}
