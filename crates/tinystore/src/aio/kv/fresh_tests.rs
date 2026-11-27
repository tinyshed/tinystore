use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::ErrorKind;
use crate::aio::Store;
use crate::kv::fixture::{HOUR, MINUTE, fixture};

/// Waits on the test's runtime, which runs the builds spawned, until `holds`.
async fn until(what: &str, mut holds: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if holds() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("{what}: not within ten seconds");
}

// contract: KV-16
#[tokio::test]
async fn a_stale_answer_is_answered_at_once_and_its_new_one_built_where_spawn_runs_it() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let spawned = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&spawned);
    let books = store
        .once::<String>("books")
        .fresh(MINUTE)
        .keep(HOUR)
        .spawn(move |build| {
            counting.fetch_add(1, Ordering::SeqCst);
            drop(tokio::spawn(build));
        })
        .open()
        .unwrap();
    let first = books.fetch("user-7", || async { Ok::<_, Error>("book 1".to_owned()) }).await.unwrap();
    assert_eq!((first.value.as_str(), first.age, first.stale), ("book 1", Duration::ZERO, false));

    f.clock.advance(MINUTE);
    let stale = books.fetch("user-7", || async { Ok::<_, Error>("book 2".to_owned()) }).await.unwrap();
    assert_eq!((stale.value.as_str(), stale.age, stale.stale), ("book 1", MINUTE, true), "the kept one, at once");
    let blocking = f.store.once::<String>("books").open().unwrap();
    until("the new answer", || blocking.get("user-7").unwrap().as_deref() == Some("book 2")).await;
    assert_eq!(spawned.load(Ordering::SeqCst), 1, "one build, where spawn put it");

    let renewed = books.run("user-7", || async { Ok::<_, Error>("never".to_owned()) }).await.unwrap();
    assert_eq!(renewed, "book 2", "the new answer, fresh again");
    books.under("eu").clear().await.unwrap();
    books.clear().await.unwrap();
    assert_eq!(books.get("user-7").await.unwrap(), None, "cleared");
    f.store.close().unwrap();
}

/// Answers kept an hour, fresh for a minute, whose builds wait in `held`
/// until the test runs them.
fn held_books(store: &Store, held: &Arc<std::sync::Mutex<Vec<Build>>>) -> Fresh<String> {
    let holding = Arc::clone(held);
    let spawn = move |build| holding.lock().unwrap().push(build);
    store.once::<String>("books").fresh(MINUTE).keep(HOUR).spawn(spawn).open().unwrap()
}

// contract: KV-16
#[tokio::test]
async fn a_build_whose_answer_was_made_fresh_before_it_ran_builds_nothing() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let books = held_books(&store, &held);
    books.run("user-7", || async { Ok::<_, Error>("book 1".to_owned()) }).await.unwrap();
    f.clock.advance(MINUTE);
    let built = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&built);
    let stale = books.fetch("user-7", move || async move {
        counting.fetch_add(1, Ordering::SeqCst);
        Ok::<_, Error>("book 3".to_owned())
    });
    assert!(stale.await.unwrap().stale);
    books.blocking().once().answers().key("user-7").ttl(HOUR).set(&"book 2".to_owned()).unwrap();
    let build = held.lock().unwrap().pop().expect("a build spawned");
    build.await;
    assert_eq!(built.load(Ordering::SeqCst), 0, "no build of an answer fresh again");
    assert_eq!(books.get("user-7").await.unwrap().as_deref(), Some("book 2"));
    f.store.close().unwrap();
}

// contract: KV-16
#[tokio::test]
async fn a_build_whose_stale_answer_is_deleted_while_it_runs_keeps_nothing() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let books = held_books(&store, &held);
    books.run("user-7", || async { Ok::<_, Error>("book 1".to_owned()) }).await.unwrap();
    f.clock.advance(MINUTE);
    let deleting = books.clone();
    let build = move || async move {
        deleting.delete("user-7").await.unwrap();
        Ok::<_, Error>("book 2".to_owned())
    };
    assert!(books.fetch("user-7", build).await.unwrap().stale);
    let build = held.lock().unwrap().pop().expect("a build spawned");
    build.await;
    assert_eq!(books.get("user-7").await.unwrap(), None, "the build's answer did not undo the delete");
    f.store.close().unwrap();
}

// contract: KV-17
#[tokio::test]
async fn a_build_that_panics_holds_off_the_next_as_one_that_fails() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let books = held_books(&store, &held);
    books.run("user-7", || async { Ok::<_, Error>("book 1".to_owned()) }).await.unwrap();
    f.clock.advance(MINUTE);
    let panics = || async {
        let answered: Result<String> = Err(Error::invalid("never answered"));
        assert!(answered.is_ok(), "the cabinet's client panicked");
        answered
    };
    assert!(books.fetch("user-7", panics).await.unwrap().stale);
    let build = held.lock().unwrap().pop().expect("a build spawned");
    assert!(tokio::spawn(build).await.unwrap_err().is_panic(), "the panic reaches the runtime");

    let after = books.fetch("user-7", || async { Ok::<_, Error>("book 2".to_owned()) }).await.unwrap();
    assert!(after.stale);
    assert!(held.lock().unwrap().is_empty(), "no build within the pause");
    f.store.close().unwrap();
}

#[tokio::test]
async fn awaited_answers_opened_with_fresh_need_spawn() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let refused = store.once::<String>("books").fresh(MINUTE).keep(HOUR).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("kv once books"), "{refused}");
    f.store.close().unwrap();
}
