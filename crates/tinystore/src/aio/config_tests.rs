//! An awaited config as a program changes and watches one, beside the
//! blocking handle of the same store.

use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Wake;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::*;
use crate::Options;
use crate::aio::Store;
use crate::config::{Live, Origin};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    port: u16,
    rps: Live<u32>,
    burst: Live<u32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { port: 8080, rps: Live::new(100), burst: Live::new(10) }
    }
}

fn opened(dir: &tempfile::TempDir) -> Store {
    Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap()
}

/// The awaited handle of `chat`, and a blocking one of the same config.
fn handles(store: &Store) -> (Config<Settings>, crate::config::Config<Settings>) {
    let awaited = store.config("chat", Settings::default()).open().unwrap();
    let blocking = store.blocking().config("chat", Settings::default()).open().unwrap();
    (awaited, blocking)
}

/// A waker that says whether it was woken.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn watched() -> (Arc<Woken>, Waker) {
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    (Arc::clone(&woken), Waker::from(woken))
}

fn polled<S: Stream + Unpin>(stream: &mut S, waker: &Waker) -> Poll<Option<S::Item>> {
    Pin::new(stream).poll_next(&mut Context::from_waker(waker))
}

async fn next<S: Stream + Unpin>(stream: &mut S) -> Option<S::Item> {
    let next = std::future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context));
    tokio::time::timeout(Duration::from_secs(10), next).await.expect("the stream answered within ten seconds")
}

/// Holds kv.db's writer in a blocking transaction on a thread of its own
/// until told, two seconds at most: a commit led on the thread that polls
/// would wait for it there, and one led on the pool waits for it there.
fn writer_held(store: &Store) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (blocking, held) = (store.blocking().clone(), store.blocking().bucket::<u32>("held").open().unwrap());
    let (holds, holding) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let tx = blocking.tx(|tx| -> Result<()> {
            tx.with(&held).set("writer", &1)?;
            holds.send(()).unwrap();
            let _ = released.recv_timeout(Duration::from_secs(2));
            Ok(())
        });
        tx.unwrap();
    });
    holding.recv().unwrap();
    (release, holder)
}

/// Waits for `seen` to hold, ten seconds at most.
fn eventually(what: &str, seen: impl Fn() -> bool) {
    let started = Instant::now();
    while !seen() {
        assert!(started.elapsed() < Duration::from_secs(10), "{what}: not within ten seconds");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// contract: CFG-04
#[tokio::test]
async fn an_awaited_update_answers_as_the_blocking_one_and_reaches_every_handle_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);

    config.update(|s| *s.rps = 200).await.unwrap();
    assert_eq!(*blocking.get().rps, 200, "the blocking handle saw the change before the future answered");
    blocking.update(|s| *s.burst = 20).unwrap();
    let (awaited, other) = (config.get(), blocking.get());
    assert_eq!((*awaited.rps, *awaited.burst, awaited.version()), (*other.rps, *other.burst, other.version()));

    let refused = config.update(|s| s.port = 9000).await.unwrap_err();
    let blocking_refused = blocking.update(|s| s.port = 9000).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocking_refused.kind(), blocking_refused.to_string()));
    assert_eq!(refused.kind(), ErrorKind::Invalid, "port is not live: {refused}");

    config.update(|_| {}).await.unwrap();
    assert_eq!(config.get().version(), other.version(), "a change of nothing writes nothing");
    store.close().await.unwrap();
}

// contract: CFG-04
#[tokio::test]
async fn if_version_refuses_an_awaited_change_made_since_as_the_blocking_one_does() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    let shown = config.get().version();
    blocking.update(|s| *s.rps = 150).unwrap();

    let conflict = config.if_version(shown).update(|s| *s.rps = 300).await.unwrap_err();
    let blocking_conflict = blocking.if_version(shown).update(|s| *s.rps = 300).unwrap_err();
    assert_eq!(conflict.kind(), ErrorKind::Conflict, "{conflict}");
    assert_eq!(conflict.to_string(), blocking_conflict.to_string());
    assert_eq!(*config.get().rps, 150, "the conflict published what came first");

    let now = config.get().version();
    config.if_version(now).update(|s| *s.rps = 300).await.unwrap();
    assert_eq!(*blocking.get().rps, 300);
    store.close().await.unwrap();
}

#[tokio::test]
async fn an_awaited_reset_drops_what_is_kept_and_refuses_what_the_blocking_one_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    config.update(|s| (*s.rps, *s.burst) = (200, 20)).await.unwrap();

    config.reset("rps").await.unwrap();
    assert_eq!((*blocking.get().rps, *blocking.get().burst), (100, 20), "the layers give rps again");
    config.reset("rps").await.unwrap();
    let unknown = config.reset("rsp").await.unwrap_err();
    let blocking_unknown = blocking.reset("rsp").unwrap_err();
    assert_eq!((unknown.kind(), unknown.to_string()), (blocking_unknown.kind(), blocking_unknown.to_string()));

    config.reset_all().await.unwrap();
    assert_eq!(*blocking.get().burst, 10);
    store.close().await.unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn an_update_runs_its_closure_in_the_poll_and_its_commit_off_the_thread_that_polls() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    let here = std::thread::current().id();
    let ran_on = Arc::new(Mutex::new(None));
    let noted = Arc::clone(&ran_on);
    let mut update = pin!(config.update(move |s| {
        *noted.lock().unwrap() = Some(std::thread::current().id());
        *s.rps = 200;
    }));

    let (release, holder) = writer_held(&store);
    let mut context = Context::from_waker(Waker::noop());
    assert!(update.as_mut().poll(&mut context).is_pending(), "the commit waits for the writer on the pool, not here");
    assert_eq!(*ran_on.lock().unwrap(), Some(here), "the closure ran in the poll");
    assert_eq!(*blocking.get().rps, 100, "nothing published before its commit");
    release.send(()).unwrap();
    holder.join().unwrap();
    update.await.unwrap();
    assert_eq!(*blocking.get().rps, 200);
    store.close().await.unwrap();
}

// contract: DUR-11
#[tokio::test]
async fn an_update_polled_once_commits_and_reaches_every_handle_though_its_future_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    let (release, holder) = writer_held(&store);
    {
        let mut update = pin!(config.update(|s| *s.rps = 200));
        let mut context = Context::from_waker(Waker::noop());
        assert!(update.as_mut().poll(&mut context).is_pending(), "submitted, behind the writer held");
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    eventually("the dropped change published", || *blocking.get().rps == 200);
    store.close().await.unwrap();

    let store = opened(&dir);
    let (config, _) = handles(&store);
    assert_eq!(*config.get().rps, 200, "the change was kept in kv.db");
    store.close().await.unwrap();
}

// contract: DUR-04, DUR-11
#[tokio::test]
async fn an_update_never_polled_runs_nothing_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, _) = handles(&store);
    let ran = Arc::new(Mutex::new(false));
    let noted = Arc::clone(&ran);
    drop(config.update(move |s| {
        *noted.lock().unwrap() = true;
        *s.rps = 200;
    }));
    drop(config.reset_all());
    assert!(!*ran.lock().unwrap(), "the closure never ran");
    store.close().await.unwrap();

    let store = opened(&dir);
    let (config, _) = handles(&store);
    assert_eq!(*config.get().rps, 100);
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_watch_gives_the_settings_now_then_the_newest_after_changes_it_fell_behind() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    let mut watching = config.watch();
    blocking.update(|s| *s.rps = 150).unwrap(); // made before the first poll: the watch started at watch()

    let first = next(&mut watching).await.unwrap().unwrap();
    assert_eq!(*first.rps, 150, "it fell behind its first item, and gets the newest");
    config.update(|s| *s.rps = 200).await.unwrap();
    config.update(|s| *s.rps = 250).await.unwrap();
    let newest = next(&mut watching).await.unwrap().unwrap();
    assert_eq!((*newest.rps, newest.version()), (250, config.get().version()), "never every version between");

    let (woken, waker) = watched();
    assert!(polled(&mut watching, &waker).is_pending(), "nothing changed since: it waits on no thread");
    blocking.update(|s| *s.rps = 300).unwrap();
    assert!(woken.0.load(Ordering::SeqCst), "a change through another handle woke it");
    let Poll::Ready(Some(Ok(changed))) = polled(&mut watching, &waker) else { panic!("the change, at the next poll") };
    assert_eq!(*changed.rps, 300);

    config.update(|s| *s.rps = 350).await.unwrap();
    // spawned, so that the stream is Send, as a program's runtime asks
    let spawned = tokio::spawn(async move { next(&mut watching).await.map(|item| item.map(|s| *s.rps)) });
    assert_eq!(spawned.await.unwrap().unwrap().unwrap(), 350);
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_watch_ends_with_closed_once_its_store_closes() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, _) = handles(&store);
    let mut watching = config.watch();
    assert!(next(&mut watching).await.unwrap().is_ok());

    let (woken, waker) = watched();
    assert!(polled(&mut watching, &waker).is_pending());
    store.close().await.unwrap();
    assert!(woken.0.load(Ordering::SeqCst), "the store's close woke it");
    let Poll::Ready(Some(Err(closed))) = polled(&mut watching, &waker) else { panic!("Closed, at the next poll") };
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(next(&mut watching).await.is_none(), "the stream ends after Closed");

    let mut after = config.watch();
    assert!(next(&mut after).await.unwrap().is_ok(), "a watch made after the close has the settings");
    assert_eq!(next(&mut after).await.unwrap().unwrap_err().kind(), ErrorKind::Closed);
    assert!(next(&mut after).await.is_none());
}

#[tokio::test]
async fn a_watch_outlives_the_handle_it_was_made_from_and_keeps_no_store_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let mut watching = store.config("chat", Settings::default()).open().unwrap().watch(); // the handle dropped
    let blocking = store.blocking().config("chat", Settings::default()).open().unwrap();
    assert_eq!(*next(&mut watching).await.unwrap().unwrap().rps, 100);
    blocking.update(|s| *s.rps = 150).unwrap();
    let changed = next(&mut watching).await.unwrap().unwrap();
    assert_eq!(*changed.rps, 150, "a change through another handle reached it");
    store.close().await.unwrap();
    assert_eq!(next(&mut watching).await.unwrap().unwrap_err().kind(), ErrorKind::Closed);
    assert!(next(&mut watching).await.is_none(), "the stream ends after Closed");

    let store = opened(&dir);
    let mut watching = store.config("chat", Settings::default()).open().unwrap().watch();
    drop(store); // its last handle: the store closes here, though the watch is kept
    let again = opened(&dir);
    assert_eq!(*next(&mut watching).await.unwrap().unwrap().rps, 150);
    assert_eq!(next(&mut watching).await.unwrap().unwrap_err().kind(), ErrorKind::Closed);
    again.close().await.unwrap();
}

// contract: CFG-09
#[tokio::test]
async fn an_awaited_config_explains_where_each_value_came_from_as_the_blocking_one_does() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let (config, blocking) = handles(&store);
    config.update(|s| *s.rps = 200).await.unwrap();
    let explained = config.explain();
    let rps = explained.settings.iter().find(|line| line.path == "rps").unwrap();
    let froms: Vec<&Origin> = rps.given.iter().map(|given| &given.from).collect();
    assert!(matches!(froms[..], [Origin::Change { version: 1, .. }, Origin::Declaration]), "{:?}", rps.given);
    assert_eq!(explained, blocking.explain());
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_blocking_config_crosses_to_the_async_api_and_back_as_the_same_config() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir);
    let blocking = store.blocking().config("chat", Settings::default()).open().unwrap();
    let config = Config::from(blocking.clone());
    config.update(|s| *s.rps = 200).await.unwrap();
    assert_eq!(*blocking.get().rps, 200);
    assert_eq!(config.blocking().get().version(), blocking.get().version());
    assert_eq!(config.name(), "chat");
    assert_eq!(config.explain().settings.len(), blocking.explain().settings.len());

    // spawned, so that each future is Send and 'static, as a program's runtime asks
    tokio::spawn(config.update(|s| *s.burst = 20)).await.unwrap().unwrap();
    tokio::spawn(config.reset("burst")).await.unwrap().unwrap();
    store.close().await.unwrap();
    let closed = config.update(|s| *s.rps = 300).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}

// contract: CFG-10
#[tokio::test]
async fn an_awaited_config_loaded_without_a_store_answers_as_the_blocking_one() {
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Agent {
        retries: u32,
    }
    let loaded = crate::config::load("agent", Agent::default()).open().unwrap();
    let awaited = Config::from(loaded);
    let refused = awaited.update(|agent| agent.retries = 3).await.unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid, "{refused}");
    awaited.reset_all().await.unwrap();
    assert_eq!(awaited.get().retries, 0);
}
