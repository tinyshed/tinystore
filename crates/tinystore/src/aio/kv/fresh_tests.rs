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
    let first = books.answer("user-7", || async { Ok::<_, Error>("book 1".to_owned()) }).await.unwrap();
    assert_eq!((first.value.as_str(), first.age, first.stale), ("book 1", Duration::ZERO, false));

    f.clock.advance(MINUTE);
    let stale = books.answer("user-7", || async { Ok::<_, Error>("book 2".to_owned()) }).await.unwrap();
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

#[tokio::test]
async fn awaited_answers_opened_with_fresh_need_spawn() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let refused = store.once::<String>("books").fresh(MINUTE).keep(HOUR).open().unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("kv once books"), "{refused}");
    f.store.close().unwrap();
}
