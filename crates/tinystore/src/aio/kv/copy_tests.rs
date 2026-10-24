//! kv's awaited calls on a node that copies kv.db from its home: each passes
//! on to the home from a thread of its own, and none waits for the home's
//! answer on the thread that polls it or on the store's pool.

use std::path::Path;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::aio::Store;
use crate::kv::fixture::SECOND;
use crate::replication::Node;
use crate::{Error, Options};

fn node(dir: &Path, node: Node) -> Store {
    let options = Options { node: Some(node), background: false, ..Options::default() };
    Store::open(dir, options).unwrap()
}

/// How long work on another node may take to reach a point, on a machine as
/// busy as a CI runner.
async fn eventually(what: &str, mut holds: impl AsyncFnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !holds().await {
        assert!(std::time::Instant::now() < deadline, "{what}: not within thirty seconds");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn poll_once<F: std::future::Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

// contract: STORE-10
#[tokio::test]
async fn on_a_copy_each_call_passes_on_to_its_home_and_answers_as_it_does_there() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let from = vec![home.blocking().node().unwrap().address()];
    let copy = node(copy_dir.path(), Node { from, ..Node::default() });
    let at_home = home.bucket::<String>("sessions").open().unwrap();
    at_home.set("ready", &"made at home".to_owned()).await.unwrap();
    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    eventually("the copy holds the home's key", async || sessions.get("ready").await.unwrap().is_some()).await;

    {
        // a write's poll passes nothing on itself: it hands the call to another thread
        let mut set = pin!(sessions.set("token", &"signed in".to_owned()));
        let _ = poll_once(set.as_mut());
    }
    sessions.set("code", &"481-205".to_owned()).await.unwrap();
    assert_eq!(at_home.get("code").await.unwrap().as_deref(), Some("481-205"), "written at the home");
    assert_eq!(sessions.take("code").await.unwrap().as_deref(), Some("481-205"));
    assert!(!sessions.delete("code").await.unwrap(), "taken at the home");
    eventually("the dropped set reached the home", async || at_home.has("token").await.unwrap()).await;

    let attempts = copy.counters("attempts").open().unwrap();
    assert_eq!(attempts.add("10.0.0.7", 2).await.unwrap(), 2, "queued here, answered with what the queue holds");
    assert_eq!(attempts.get("10.0.0.7").await.unwrap(), 2);
    // a request takes half a minute of the rate: none comes back while the test runs
    let api = copy.rate_limit("api").rate(2, 60 * SECOND).open().unwrap();
    assert!(api.allow("42").await.unwrap().ok);
    let at_its_home = home.rate_limit("api").rate(2, 60 * SECOND).open().unwrap();
    assert!(at_its_home.allow("42").await.unwrap().ok, "the second of the burst, at the home");
    assert!(!api.allow("42").await.unwrap().ok, "one limit for every node, decided at its home");
    let messages = copy.quota("messages").window("daily", 3, 24 * 60 * 60 * SECOND).open().unwrap();
    assert_eq!(messages.allow(42).await.unwrap().left, 2);

    let charges = copy.once::<String>("charges").open().unwrap();
    {
        // a run abandoned on the copy gives its claim back to the home
        let mut abandoned = pin!(charges.run("req-6", std::future::pending::<Result<String, Error>>));
        let _ = poll_once(abandoned.as_mut());
    }
    let receipt = charges.run("req-7", || async { Ok::<_, Error>("receipt 1".to_owned()) }).await.unwrap();
    assert_eq!(receipt, "receipt 1");
    let kept = home.once::<String>("charges").open().unwrap();
    assert_eq!(kept.get("req-7").await.unwrap().as_deref(), Some("receipt 1"), "kept at the home");
    let again = charges.run("req-7", || async { Ok::<_, Error>("never".to_owned()) }).await.unwrap();
    assert_eq!(again, "receipt 1");
    eventually("the abandoned run's key is free at the home", async || {
        let ran = kept.run("req-6", || async { Ok::<_, Error>("at home".to_owned()) });
        tokio::time::timeout(Duration::from_millis(200), ran).await.is_ok_and(|ran| ran.is_ok())
    })
    .await;

    drop((at_home, sessions, attempts, api, messages, charges, kept));
    copy.close().await.unwrap();
    home.close().await.unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn on_a_copy_runs_that_wait_at_the_home_hold_none_of_the_pools_threads() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let from = vec![home.blocking().node().unwrap().address()];
    let copy = node(copy_dir.path(), Node { from, ..Node::default() });
    home.bucket::<String>("sessions").open().unwrap().set("ready", &"made at home".to_owned()).await.unwrap();
    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    eventually("the copy holds the home's key", async || sessions.get("ready").await.unwrap().is_some()).await;

    let (at_home, charges) =
        (home.once::<String>("charges").open().unwrap(), copy.once::<String>("charges").open().unwrap());
    let ((started, charging), (release, released)) = (tokio::sync::oneshot::channel(), tokio::sync::oneshot::channel());
    let charged = tokio::spawn(at_home.run("req-9", move || async move {
        started.send(()).unwrap();
        let _ = released.await;
        Ok::<_, Error>("receipt 1".to_owned())
    }));
    charging.await.unwrap();
    // one more than the pool has threads: on the pool, the last would never reach the home
    let waiting: Vec<_> = (0..=crate::pool::MOST)
        .map(|_| tokio::spawn(charges.run("req-9", || async { Ok::<_, Error>("never".to_owned()) })))
        .collect();
    // Within less than the 25 s the home waits for a run under way: a claim
    // the pool held back would reach the home once those before it gave up.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while at_home.blocking().answers().scope.kv.runs().under_way().1 <= crate::pool::MOST {
        assert!(std::time::Instant::now() < deadline, "a run on the copy waited for a thread of its pool");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let listed = tokio::time::timeout(Duration::from_secs(10), sessions.list(10, None)).await;
    assert!(listed.is_ok(), "a read on the copy waited for a thread of its pool");

    release.send(()).unwrap();
    assert_eq!(charged.await.unwrap().unwrap(), "receipt 1");
    for run in waiting {
        assert_eq!(run.await.unwrap().unwrap(), "receipt 1", "each took the answer kept at the home");
    }
    drop((sessions, at_home, charges));
    copy.close().await.unwrap();
    home.close().await.unwrap();
}
