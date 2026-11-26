use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use super::*;
use crate::ErrorKind;
use crate::kv::engine::Kv;
use crate::kv::fixture::*;
use crate::kv::once::age_of;

/// Gradebooks kept twelve hours, fresh for ten minutes.
fn books(f: &Fixture) -> Fresh<String> {
    f.store.once::<String>("books").fresh(10 * MINUTE).keep(12 * HOUR).open().unwrap()
}

/// A build that counts its runs and answers with the count.
fn counted_book(runs: &Arc<AtomicUsize>) -> impl FnOnce() -> Result<String> + Send + 'static {
    let runs = Arc::clone(runs);
    move || Ok(format!("book {}", runs.fetch_add(1, Ordering::SeqCst) + 1))
}

/// Waits until no run of a key is under way: the build in the background ended.
fn built(f: &Fixture) {
    let kv = Kv::of(&f.store).unwrap();
    eventually("the build in the background", || kv.runs().under_way() == (0, 0));
}

// contract: KV-18
#[test]
fn an_answer_younger_than_fresh_is_answered_as_it_is_and_says_its_age() {
    let f = fixture();
    let books = books(&f);
    let runs = Arc::new(AtomicUsize::new(0));
    let first = books.answer("user-7", counted_book(&runs)).unwrap();
    assert_eq!(first, Answer { value: "book 1".to_owned(), age: Duration::ZERO, stale: false });

    f.clock.advance(10 * MINUTE - Duration::from_millis(1));
    let kept = books.answer("user-7", counted_book(&runs)).unwrap();
    assert_eq!(kept, Answer { value: "book 1".to_owned(), age: 10 * MINUTE - Duration::from_millis(1), stale: false });
    assert_eq!(runs.load(Ordering::SeqCst), 1, "nothing built");

    let plain = f.store.once::<String>("books").keep(12 * HOUR).open().unwrap();
    let seen = plain.answer("user-7", || -> Result<String> { panic!("ran beside a kept answer") }).unwrap();
    assert_eq!((seen.age, seen.stale), (10 * MINUTE - Duration::from_millis(1), false), "never stale without fresh");
}

// contract: KV-16
#[test]
fn a_stale_answer_is_answered_at_once_while_one_build_makes_the_next() {
    let f = fixture();
    let books = books(&f);
    let runs = Arc::new(AtomicUsize::new(0));
    books.run("user-7", counted_book(&runs)).unwrap();
    f.clock.advance(10 * MINUTE);

    let (started, starting) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let slow = {
        let runs = Arc::clone(&runs);
        move || {
            started.send(()).unwrap();
            released.recv().unwrap();
            Ok::<_, Error>(format!("book {}", runs.fetch_add(1, Ordering::SeqCst) + 1))
        }
    };
    let stale = books.answer("user-7", slow).unwrap();
    assert_eq!(stale, Answer { value: "book 1".to_owned(), age: 10 * MINUTE, stale: true }, "at once");
    starting.recv().unwrap();
    let beside = Arc::new(AtomicUsize::new(0));
    thread::scope(|scope| {
        let callers: Vec<_> = (0..8).map(|_| scope.spawn(|| books.answer("user-7", counted_book(&beside)))).collect();
        for caller in callers {
            assert_eq!(caller.join().unwrap().unwrap().value, "book 1", "the stale one, waiting for nothing");
        }
    });
    release.send(()).unwrap();
    built(&f);
    assert_eq!(beside.load(Ordering::SeqCst), 0, "no second build beside the first");
    let renewed = books.answer("user-7", counted_book(&runs)).unwrap();
    assert_eq!(renewed, Answer { value: "book 2".to_owned(), age: Duration::ZERO, stale: false }, "both terms again");
    assert_eq!(runs.load(Ordering::SeqCst), 2, "one build in the background");

    f.clock.advance(12 * HOUR);
    assert_eq!(books.run("user-7", counted_book(&runs)).unwrap(), "book 3", "past keep, built while it waits");
}

// contract: KV-17
#[test]
fn a_build_that_fails_keeps_the_stale_answer_and_holds_off_the_next() {
    let f = fixture();
    let books = books(&f);
    let runs = Arc::new(AtomicUsize::new(0));
    books.run("user-7", counted_book(&runs)).unwrap();
    f.clock.advance(11 * MINUTE);

    let down = || Err::<String, _>(Error::not_found("the cabinet is down"));
    assert_eq!(books.run("user-7", down).unwrap(), "book 1");
    built(&f);
    assert_eq!(books.get("user-7").unwrap().as_deref(), Some("book 1"), "the kept answer stays");

    // a minute of pause: a tenth of fresh
    f.clock.advance(MINUTE - Duration::from_millis(1));
    assert_eq!(books.run("user-7", counted_book(&runs)).unwrap(), "book 1");
    built(&f);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "no build within the pause");
    f.clock.advance(Duration::from_millis(1));
    assert_eq!(books.run("user-7", counted_book(&runs)).unwrap(), "book 1", "stale still, while it builds");
    built(&f);
    assert_eq!(books.get("user-7").unwrap().as_deref(), Some("book 2"), "the pause over, built again");

    f.clock.advance(12 * HOUR);
    let gone = books.run("user-7", down).unwrap_err();
    assert_eq!(gone.kind(), ErrorKind::NotFound, "past keep the build's error is the caller's");
    assert_eq!(books.get("user-7").unwrap(), None, "and nothing is kept");
}

// contract: KV-19
#[test]
fn a_clear_removes_a_branch_and_the_branches_under_it() {
    let f = fixture();
    let books = books(&f);
    let runs = Arc::new(AtomicUsize::new(0));
    books.under("course-1").run("user-7", counted_book(&runs)).unwrap();
    books.under("course-1").under("group-2").run("user-8", counted_book(&runs)).unwrap();
    books.under("course-2").run("user-7", counted_book(&runs)).unwrap();
    books.under("course-1").clear().unwrap();
    assert_eq!(books.under("course-1").get("user-7").unwrap(), None);
    assert_eq!(books.under("course-1").under("group-2").get("user-8").unwrap(), None, "the branch under it too");
    assert_eq!(books.under("course-2").get("user-7").unwrap().as_deref(), Some("book 3"), "a branch beside stays");

    let charges = f.store.once::<String>("charges").open().unwrap();
    charges.run("req-1", || Ok::<_, Error>("receipt".to_owned())).unwrap();
    charges.clear().unwrap();
    assert_eq!(charges.get("req-1").unwrap(), None, "answers without fresh clear too");
}

#[test]
fn a_fresh_of_zero_or_as_long_as_keep_is_refused() {
    let f = fixture();
    for fresh in [Duration::ZERO, HOUR, DAY] {
        let refused = f.store.once::<String>("books").fresh(fresh).keep(HOUR).open().unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        assert!(refused.to_string().starts_with("kv once books: "), "{refused}");
    }
}

#[test]
fn an_age_reads_through_keep_and_stays_within_it() {
    let keep = HOUR;
    let now = 1_800_000_000_000;
    assert_eq!(age_of(Some(now + millis(keep)), now, keep), Duration::ZERO);
    assert_eq!(age_of(Some(now + 1_000), now, keep), keep - SECOND);
    assert_eq!(age_of(Some(now + millis(2 * keep)), now, keep), Duration::ZERO, "a longer keep wrote it");
    assert_eq!(age_of(Some(now - 1), now, keep), keep, "never past keep");
    assert_eq!(pause_after(10 * MINUTE), MINUTE);
    assert_eq!(pause_after(SECOND), SECOND);
    assert_eq!(pause_after(DAY), MINUTE);
}
