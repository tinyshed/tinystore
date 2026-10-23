use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::*;
use crate::kv::fixture::*;
use crate::testing::{PATIENCE, holds_within};
use crate::{Clock, unix_millis};

/// Long enough for a thread to reach where a test waits for it.
fn locks(store: &Store) -> Arc<Locks> {
    Locks::of(store).unwrap()
}

/// Holds `key` on a thread of its own until told, saying once it holds.
fn held_elsewhere(lock: &Lock, key: &'static str) -> (mpsc::Sender<()>, thread::JoinHandle<Result<Fence>>) {
    let (holds, held) = mpsc::channel();
    let (go, told) = mpsc::channel::<()>();
    let lock = lock.clone();
    let holder = thread::spawn(move || {
        lock.hold(key, |lease| {
            holds.send(lease.fence()).unwrap();
            told.recv_timeout(PATIENCE).unwrap();
            Ok(lease.fence())
        })
    });
    held.recv_timeout(PATIENCE).unwrap();
    (go, holder)
}

// contract: LOCK-01
#[test]
fn a_key_is_held_by_one_holder_at_a_time_and_given_on_when_let_go() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let (go, first) = held_elsewhere(&invoices, "42");

    assert!(invoices.try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_none(), "held: nothing ran");
    assert_eq!(invoices.try_hold("43", |_| Ok::<_, Error>("another key")).unwrap(), Some("another key"));

    let second = {
        let invoices = invoices.clone();
        thread::spawn(move || invoices.hold("42", |lease| Ok::<_, Error>(lease.fence())))
    };
    let locks = locks(&f.store);
    holds_within(PATIENCE, "the second in line", || locks.waiting() == 1);
    go.send(()).unwrap();
    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();
    assert!(second > first, "a later lease's fence is larger: {first:?} then {second:?}");
    assert_eq!(locks.waiting(), 0);
}

// contract: LOCK-01
#[test]
fn waiters_are_served_in_the_order_they_came() {
    let f = fixture();
    let reports = f.store.lock("reports").open().unwrap();
    let (go, first) = held_elsewhere(&reports, "sales");
    let locks = locks(&f.store);
    let served = Arc::new(Mutex::new(Vec::new()));
    let mut waiters = Vec::new();
    for name in ["b", "c", "d"] {
        let (reports, served) = (reports.clone(), Arc::clone(&served));
        waiters.push(thread::spawn(move || {
            reports.hold("sales", |_| {
                served.lock().unwrap().push(name);
                Ok::<_, Error>(())
            })
        }));
        let behind = waiters.len();
        holds_within(PATIENCE, "the next in line", || locks.waiting() == behind);
    }
    go.send(()).unwrap();
    first.join().unwrap().unwrap();
    for waiter in waiters {
        waiter.join().unwrap().unwrap();
    }
    assert_eq!(*served.lock().unwrap(), ["b", "c", "d"]);
}

#[test]
fn a_key_still_held_after_the_wait_is_in_use() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let (go, first) = held_elsewhere(&invoices, "42");
    let waiter = {
        let invoices = invoices.clone();
        thread::spawn(move || invoices.key("42").wait(5 * SECOND).hold(|_| Ok::<_, Error>(())))
    };
    let locks = locks(&f.store);
    holds_within(PATIENCE, "the waiter in line", || locks.waiting() == 1);
    f.clock.advance(5 * SECOND - Duration::from_millis(1));
    f.store.clock_moved();
    thread::sleep(Duration::from_millis(20));
    assert_eq!(locks.waiting(), 1, "a millisecond short of its wait");
    f.clock.advance(Duration::from_millis(1));
    f.store.clock_moved();
    let error = waiter.join().unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InUse, "{error}");
    assert!(error.to_string().starts_with("lock invoices: key \"42\""), "{error}");
    go.send(()).unwrap();
    first.join().unwrap().unwrap();
}

// contract: LOCK-03
#[test]
fn max_hold_revokes_a_lease_gives_the_key_on_and_fails_the_hold_lost() {
    let f = fixture();
    let reports = f.store.lock("reports").max_hold(10 * SECOND).open().unwrap();
    let (stuck, told) = mpsc::channel::<()>();
    let (ran, running) = mpsc::channel();
    let hung = {
        let reports = reports.clone();
        thread::spawn(move || {
            reports.hold("sales", |lease| {
                ran.send(()).unwrap();
                told.recv_timeout(PATIENCE).unwrap();
                Ok::<_, Error>(lease.lost())
            })
        })
    };
    running.recv_timeout(PATIENCE).unwrap();
    let next = {
        let reports = reports.clone();
        thread::spawn(move || reports.key("sales").wait(MINUTE).hold(|lease| Ok::<_, Error>(lease.fence())))
    };
    let locks = locks(&f.store);
    holds_within(PATIENCE, "the next in line", || locks.waiting() == 1);
    f.clock.advance(10 * SECOND);
    f.store.clock_moved();
    next.join().unwrap().expect("revoked at max_hold, the key goes to the next in line");
    stuck.send(()).unwrap();
    let error = hung.join().unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Lost, "{error}");
}

// contract: LOCK-04
#[test]
fn a_hold_inside_its_own_hold_or_inside_a_transaction_is_invalid() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let nested = invoices.hold("42", |_| invoices.hold("42", |_| Ok::<_, Error>(())));
    assert_eq!(nested.unwrap_err().kind(), ErrorKind::Invalid);
    let other = invoices.hold("42", |_| invoices.hold("43", |_| Ok::<_, Error>("another key")));
    assert_eq!(other.unwrap(), "another key");

    let stock = f.store.bucket::<i64>("stock").open().unwrap();
    let inside = f.store.tx(|tx| {
        tx.with(&stock).set("sku", &1)?;
        invoices.hold("42", |_| Ok::<_, Error>(()))
    });
    let error = inside.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert!(invoices.try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some(), "nothing was left held");
}

// contract: LOCK-02
#[test]
fn fences_grow_with_every_lease_and_across_a_restart() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let first = invoices.hold("42", |lease| Ok::<_, Error>(lease.fence())).unwrap();
    let second = invoices.hold("43", |lease| Ok::<_, Error>(lease.fence())).unwrap();
    assert!(second > first, "one count for every key");
    let epoch = locks(&f.store).epoch();
    assert_eq!(first.get() >> 64, u128::from(epoch));

    let f = f.reopen();
    assert_eq!(locks(&f.store).epoch(), epoch + 1, "the clock did not move: one past the last epoch");
    let invoices = f.store.lock("invoices").open().unwrap();
    let after = invoices.hold("42", |lease| Ok::<_, Error>(lease.fence())).unwrap();
    assert!(after > second, "{second:?} before the restart, {after:?} after");

    f.clock.advance(HOUR);
    let f = f.reopen();
    let now = u64::try_from(unix_millis(f.clock.now())).unwrap();
    assert_eq!(locks(&f.store).epoch(), now, "the clock went past the last epoch: its time");
}

#[test]
fn a_semaphore_admits_its_limit_a_key_and_a_lock_of_its_name_is_another() {
    let f = fixture();
    let uploads = f.store.semaphore("uploads").limit(2).open().unwrap();
    let inside =
        uploads.hold("ann", |_| uploads.hold("ann", |_| Ok::<_, Error>("nested on one thread is the same key")));
    assert_eq!(inside.unwrap_err().kind(), ErrorKind::Invalid);

    let (go, holders): (Vec<_>, Vec<_>) = (0..2)
        .map(|_| {
            let (held, holds) = mpsc::channel();
            let (go, told) = mpsc::channel::<()>();
            let uploads = uploads.clone();
            let holder = thread::spawn(move || {
                uploads.hold("ann", |_| {
                    held.send(()).unwrap();
                    told.recv_timeout(PATIENCE).unwrap();
                    Ok::<_, Error>(())
                })
            });
            holds.recv_timeout(PATIENCE).unwrap();
            (go, holder)
        })
        .unzip();
    assert!(uploads.try_hold("ann", |_| Ok::<_, Error>(())).unwrap().is_none(), "two of two hold it");
    assert!(uploads.try_hold("bob", |_| Ok::<_, Error>(())).unwrap().is_some(), "a limit a key");
    let lock = f.store.lock("uploads").open().unwrap();
    assert!(lock.try_hold("ann", |_| Ok::<_, Error>(())).unwrap().is_some(), "the lock of the name is another");
    for (go, holder) in go.into_iter().zip(holders) {
        go.send(()).unwrap();
        holder.join().unwrap().unwrap();
    }
}

// contract: LOCK-06
#[test]
fn a_panic_lets_the_key_go() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        invoices.hold("42", |_| -> Result<()> { panic!("the holder's function panicked") })
    }));
    assert!(panicked.is_err());
    assert!(invoices.try_hold("42", |_| Ok::<_, Error>(())).unwrap().is_some());
}

// contract: LOCK-06
#[test]
fn closing_the_store_ends_every_lease_and_answers_every_waiter() {
    let f = fixture();
    let invoices = f.store.lock("invoices").open().unwrap();
    let (stuck, told) = mpsc::channel::<()>();
    let (ran, running) = mpsc::channel();
    let holder = {
        let invoices = invoices.clone();
        thread::spawn(move || {
            invoices.hold("42", |lease| {
                ran.send(()).unwrap();
                told.recv_timeout(PATIENCE).unwrap();
                Ok::<_, Error>(lease.lost())
            })
        })
    };
    running.recv_timeout(PATIENCE).unwrap();
    let waiter = {
        let invoices = invoices.clone();
        thread::spawn(move || invoices.hold("42", |_| Ok::<_, Error>(())))
    };
    let locks = locks(&f.store);
    holds_within(PATIENCE, "the waiter in line", || locks.waiting() == 1);
    f.store.close().unwrap();
    assert_eq!(waiter.join().unwrap().unwrap_err().kind(), ErrorKind::Closed);
    stuck.send(()).unwrap();
    assert_eq!(holder.join().unwrap().unwrap_err().kind(), ErrorKind::Closed);
    assert_eq!(invoices.try_hold("42", |_| Ok::<_, Error>(())).unwrap_err().kind(), ErrorKind::Closed);
}

#[test]
fn names_keys_limits_and_bounds_are_checked() {
    let f = fixture();
    assert_eq!(f.store.lock("Invoices").open().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(f.store.semaphore("uploads").open().unwrap_err().kind(), ErrorKind::Invalid, "no limit");
    assert_eq!(f.store.semaphore("uploads").limit(0).open().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(f.store.semaphore("uploads").limit(1001).open().unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(f.store.lock("a").max_hold(Duration::ZERO).open().unwrap_err().kind(), ErrorKind::Invalid);
    let invoices = f.store.lock("invoices").open().unwrap();
    assert_eq!(invoices.hold("", |_| Ok::<_, Error>(())).unwrap_err().kind(), ErrorKind::Invalid);
    let long = "k".repeat(1025);
    assert_eq!(invoices.hold(long.as_str(), |_| Ok::<_, Error>(())).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(invoices.hold(42, |_| Ok::<_, Error>("an integer key")).unwrap(), "an integer key");
}

/// What a holder through a connection asks: a lease that lasts `ttl` unless
/// renewed.
#[cfg(feature = "wire")]
fn remote(key: &str, ttl: Duration, wait: Duration, request: Option<lease::RequestId>) -> Ask {
    let place = Place::new(Kind::Lock, Arc::from("invoices"), key.to_owned());
    Ask { place, limit: 1, ttl: Some(ttl), max_hold: None, wait, request }
}

#[cfg(feature = "wire")]
fn asked(locks: &Locks, ask: Ask) -> (Option<u64>, mpsc::Receiver<Result<Answer>>) {
    let (answered, answer) = mpsc::channel();
    let waiting = locks.ask(ask, Box::new(move |answer| drop(answered.send(answer))));
    (waiting, answer)
}

#[cfg(feature = "wire")]
fn granted(answer: &mpsc::Receiver<Result<Answer>>) -> Granted {
    match answer.recv_timeout(PATIENCE).unwrap().unwrap() {
        Answer::Granted(granted) => granted,
        Answer::Busy => panic!("busy where a grant was due"),
    }
}

// contract: LOCK-05
#[cfg(feature = "wire")]
#[test]
fn a_lease_through_a_connection_lasts_its_ttl_from_its_last_renewal() {
    let f = fixture();
    let locks = locks(&f.store);
    let (_, first) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, None));
    let first = granted(&first);
    let (waiting, second) = asked(&locks, remote("42", 10 * SECOND, MINUTE, None));
    assert!(waiting.is_some());

    f.clock.advance(9 * SECOND);
    let deadline = locks.renew(&first.lease, 10 * SECOND).unwrap();
    assert_eq!(deadline, unix_millis(f.clock.now()) + 10_000, "ten seconds from the renewal");
    f.clock.advance(9 * SECOND);
    f.store.clock_moved();
    thread::sleep(Duration::from_millis(20));
    assert!(second.try_recv().is_err(), "renewed, the first still holds it");

    f.clock.advance(SECOND);
    f.store.clock_moved();
    let second = granted(&second);
    assert!(second.fence > first.fence);
    assert_eq!(first.watch.ending(), Ending::Lost, "the first is told it lost the key");
    let renewed = locks.renew(&first.lease, 10 * SECOND).unwrap_err();
    assert_eq!(renewed.kind(), ErrorKind::Lost, "a lease that ran out is never renewed");
    assert!(!locks.release(&first.lease), "nor let go of the key another holds now");
    assert!(locks.release(&second.lease));
}

#[cfg(feature = "wire")]
#[test]
fn a_request_asked_again_answers_its_lease_or_takes_over_its_place_in_line() {
    let f = fixture();
    let locks = locks(&f.store);
    let request = Some([7; 16]);
    let (_, answer) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, request));
    let lease = granted(&answer);
    let (_, again) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, request));
    let again = granted(&again);
    assert_eq!((again.lease, again.fence), (lease.lease, lease.fence), "the same lease, not the key twice");

    let waiting_request = Some([8; 16]);
    let (waiting, lost) = asked(&locks, remote("42", 10 * SECOND, MINUTE, waiting_request));
    assert!(waiting.is_some());
    let (waiting_again, taken_over) = asked(&locks, remote("42", 10 * SECOND, MINUTE, waiting_request));
    assert_eq!(waiting_again, waiting, "the place taken over keeps its waiter's id, which a cancel names");
    assert_eq!(lost.recv_timeout(PATIENCE).unwrap().err().map(|error| error.kind()), Some(ErrorKind::Cancelled));
    assert_eq!(locks.waiting(), 1, "one place in line");
    assert!(locks.release(&lease.lease));
    granted(&taken_over);
}

// contract: LOCK-01
// contract: LOCK-05
#[cfg(feature = "wire")]
#[test]
fn a_request_asked_again_once_its_lease_ran_out_is_asked_afresh_and_the_key_is_held_once() {
    let f = fixture();
    let locks = locks(&f.store);
    let request = Some([7; 16]);
    let (_, answer) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, request));
    let lease = granted(&answer);
    // the answer was lost, and the client asks again after the ttl, nobody asking between
    f.clock.advance(11 * SECOND);
    let (_, again) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, request));
    let again = granted(&again);
    assert!(again.fence > lease.fence, "a lease of its own, not the one that ran out");
    assert_eq!(lease.watch.ending(), Ending::Lost);
    let (_, other) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, None));
    let busy = other.recv_timeout(PATIENCE).unwrap().unwrap();
    assert!(matches!(busy, Answer::Busy), "one holder at a time");
    assert!(locks.release(&again.lease));
}

// contract: LOCK-06
#[cfg(feature = "wire")]
#[test]
fn a_cancelled_waiter_leaves_its_line() {
    let f = fixture();
    let locks = locks(&f.store);
    let (_, held) = asked(&locks, remote("42", 10 * SECOND, Duration::ZERO, None));
    let held = granted(&held);
    let (waiting, gone) = asked(&locks, remote("42", 10 * SECOND, MINUTE, None));
    let (_, next) = asked(&locks, remote("42", 10 * SECOND, MINUTE, None));
    locks.cancel(waiting.unwrap());
    assert_eq!(locks.waiting(), 1);
    assert!(locks.release(&held.lease));
    granted(&next);
    assert!(gone.try_recv().is_err(), "a cancelled waiter is answered nothing");
}
