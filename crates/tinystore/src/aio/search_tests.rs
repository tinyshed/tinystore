use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::*;
use crate::{Durability, ErrorKind, Options};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Article {
    title: String,
    body: String,
    author_id: i64,
}

fn article(title: &str, body: &str, author_id: i64) -> Article {
    Article { title: title.to_owned(), body: body.to_owned(), author_id }
}

/// A store whose maintenance the test runs, as the blocking tests' is, or
/// the store's own background work runs.
fn opened(dir: &tempfile::TempDir, background: bool) -> Store {
    // Commits to the operating system alone: a test's sets wait for no disk.
    let options = Options { background, durability: Some(Durability::Os), ..Options::default() };
    Store::open(dir.path(), options).unwrap()
}

fn articles(store: &Store) -> Index<Article> {
    store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).filters(["author_id"]).open().unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
}

fn polled_once<F: Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// Holds `file`'s writer on a thread of its own until the sender it gives
/// back is dropped or sent to, ten seconds at most.
fn hold_writer(file: &Arc<crate::sqlite::File>) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (release, held_until) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let file = Arc::clone(file);
    let holder = thread::spawn(move || {
        let _writer = file.hold_writer();
        holding.send(()).unwrap();
        let _ = held_until.recv_timeout(Duration::from_secs(10));
    });
    held.recv().unwrap();
    (release, holder)
}

/// Every thread of the store's pool kept busy until this is dropped.
struct BusyPool(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl BusyPool {
    fn occupy(store: &Store) -> BusyPool {
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (started, all_started) = mpsc::channel();
        for _ in 0..crate::pool::MOST {
            let (gate, started) = (Arc::clone(&gate), started.clone());
            store.pool().run(Box::new(move || {
                started.send(()).unwrap();
                let mut open = gate.0.lock().unwrap();
                while !*open {
                    open = gate.1.wait(open).unwrap();
                }
            }));
        }
        for _ in 0..crate::pool::MOST {
            all_started.recv_timeout(Duration::from_secs(10)).expect("a thread of the pool never ran its job");
        }
        BusyPool(gate)
    }
}

impl Drop for BusyPool {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

/// A change of the index's rows on a thread of its own, which says when it
/// holds them and lets them go at once.
fn change_on_a_thread(index: &Index<Article>) -> (mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let (open, file) = (Arc::clone(&index.blocking().open), Arc::clone(&index.blocking().search.file));
    let (changed, change) = mpsc::channel();
    let changer = thread::spawn(move || {
        let held = open.change(&file);
        changed.send(()).unwrap();
        drop(held);
    });
    (change, changer)
}

fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn an_awaited_index_answers_as_the_blocking_one_on_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    let blocking = articles.blocking();
    let bread = article("Sourdough bread", "flour, water and salt", 42);
    // spawned, so that the future is Send and 'static, as a program's runtime asks
    tokio::spawn(articles.set("a-17", &bread)).await.unwrap().unwrap();
    blocking.set("a-18", &article("Rye", "bread from rye flour", 7)).unwrap();
    let (spelt, butter) = (article("Spelt", "an old wheat", 42), article("Replication", "milk and cream", 7));
    articles.set_many([("a-19", &spelt), ("a-20", &butter)]).await.unwrap();

    assert_eq!(articles.get("a-17").await.unwrap(), Some(bread.clone()));
    assert_eq!(articles.get("a-18").await.unwrap(), blocking.get("a-18").unwrap());
    assert_eq!(articles.get("a-99").await.unwrap(), None);
    let filtered = articles.search("bread").filter(eq("author_id", &42)).run().await.unwrap();
    assert_eq!(filtered, blocking.search("bread").filter(eq("author_id", &42)).run().unwrap());
    assert_eq!(filtered.hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(), ["a-17"]);
    let paged = articles.search("bread flour").limit(1).offset(1).run().await.unwrap();
    assert_eq!(paged, blocking.search("bread flour").limit(1).offset(1).run().unwrap());
    let misspelt = articles.search("replicaiton").run().await.unwrap();
    assert_eq!(misspelt.corrected.as_deref(), Some("replication"));
    assert_eq!(misspelt, blocking.search("replicaiton").run().unwrap());
    let typed = articles.search("sourd").prefix().run().await.unwrap();
    assert_eq!(typed.hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(), ["a-17"]);
    assert_eq!(typed, blocking.search("sourd").prefix().run().unwrap());
    assert_eq!(articles.status().await.unwrap(), blocking.status().unwrap());

    assert!(articles.delete("a-17").await.unwrap());
    assert!(!articles.delete("a-17").await.unwrap(), "there is none any more");
    assert_eq!(blocking.get("a-17").unwrap(), None);
    assert!(articles.search("sourdough").run().await.unwrap().hits.is_empty(), "the next search sees the delete");
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_refused_call_answers_the_error_its_blocking_call_does() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    let blocking = articles.blocking();
    let bread = article("Sourdough bread", "salt", 42);
    let same = |awaited: crate::Error, blocked: crate::Error| {
        assert_eq!((awaited.kind(), awaited.to_string()), (blocked.kind(), blocked.to_string()));
    };
    same(articles.set("", &bread).await.unwrap_err(), blocking.set("", &bread).unwrap_err());
    same(articles.get("").await.unwrap_err(), blocking.get("").unwrap_err());
    same(articles.delete("").await.unwrap_err(), blocking.delete("").unwrap_err());
    let vector = [1.0, 0.0];
    same(
        articles.id("a-1").vector(&vector).set(&bread).await.unwrap_err(),
        blocking.id("a-1").vector(&vector).set(&bread).unwrap_err(),
    );
    let one_bad = [("a-1", &bread, Some(&vector[..])), ("a-2", &bread, None)];
    same(articles.set_many(one_bad).await.unwrap_err(), blocking.set_many(one_bad).unwrap_err());
    let too_many: Vec<(String, &Article)> = (0..1001).map(|at| (format!("a-{at}"), &bread)).collect();
    let refused = articles.set_many(too_many.iter().map(|(id, value)| (id, *value))).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit);
    same(refused, blocking.set_many(too_many.iter().map(|(id, value)| (id, *value))).unwrap_err());
    same(
        articles.search("bread").limit(0).run().await.unwrap_err(),
        blocking.search("bread").limit(0).run().unwrap_err(),
    );
    same(
        articles.search("bread").rerank().run().await.unwrap_err(),
        blocking.search("bread").rerank().run().unwrap_err(),
    );
    assert_eq!(blocking.get("a-1").unwrap(), None, "nothing refused was written");
    store.close().await.unwrap();
}

#[tokio::test]
async fn documents_with_vectors_are_set_and_found_as_the_blocking_api_finds_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let docs = store.index::<Article>("docs").fields(["title", "body"]).vectors(3).open().unwrap();
    let (bread, rye) = (article("Bread at home", "flour", 1), article("Rye", "bread from rye", 2));
    docs.id("d-1").vector(&[1.0, 0.0, 0.0]).set(&bread).await.unwrap();
    docs.set_many([("d-2", &rye, &[0.0, 1.0, 0.0][..])]).await.unwrap();
    docs.id("d-3").without_vector().set(&article("Salt", "no vector", 3)).await.unwrap();
    let question = [0.9, 0.1, 0.0];
    let by_vector = docs.search_vector(&question).run().await.unwrap();
    assert_eq!(by_vector, docs.blocking().search_vector(&question).run().unwrap());
    assert_eq!(by_vector.hits[0].id, "d-1");
    let by_both = docs.search("bread").vector(&question).vector_weight(0.7).run().await.unwrap();
    assert_eq!(by_both, docs.blocking().search("bread").vector(&question).vector_weight(0.7).run().unwrap());
    store.close().await.unwrap();
}

// contract: STORE-10
#[test]
fn a_set_or_a_get_polled_while_a_change_holds_the_rows_waits_for_it_on_the_pool_never_in_the_poll() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    let bread = article("Sourdough bread", "salt", 42);
    articles.blocking().set("a-17", &bread).unwrap();
    articles.blocking().get("a-17").unwrap(); // a reader idle: the get is tried in its poll
    let (open, file) = (Arc::clone(&articles.blocking().open), Arc::clone(&articles.blocking().search.file));
    let (release, held_until) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let changer = thread::spawn(move || {
        let _changing = open.change(&file);
        holding.send(()).unwrap();
        let _ = held_until.recv_timeout(Duration::from_secs(10));
    });
    held.recv().unwrap();
    let rye = article("Rye", "flour", 7);
    let (mut set, mut get, mut delete) =
        (pin!(articles.set("a-18", &rye)), pin!(articles.get("a-17")), pin!(articles.delete("a-19")));
    let started = Instant::now();
    assert!(polled_once(set.as_mut()).is_pending(), "the change holds the rows: the set waits for it");
    assert!(polled_once(get.as_mut()).is_pending());
    assert!(polled_once(delete.as_mut()).is_pending());
    assert!(started.elapsed() < Duration::from_secs(5), "a poll waited for the change");
    release.send(()).unwrap();
    changer.join().unwrap();
    let runtime = runtime();
    runtime.block_on(set).unwrap();
    assert_eq!(runtime.block_on(get).unwrap(), Some(bread));
    assert!(!runtime.block_on(delete).unwrap(), "there was no a-19");
    assert_eq!(articles.blocking().get("a-18").unwrap(), Some(rye));
    store.blocking().close().unwrap();
}

// contract: SRCH-06
#[test]
fn searches_answer_while_a_change_waits_for_a_set_submitted_under_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let (release, holder) = hold_writer(&articles.blocking().search.file);
    let mut set = pin!(articles.set("a-18", &article("Rye", "flour", 7)));
    assert!(polled_once(set.as_mut()).is_pending(), "the writer is held, so the set's commit waits for it");
    let (changed, changer) = change_on_a_thread(&articles);
    assert!(changed.recv_timeout(Duration::from_millis(200)).is_err(), "the change waits for the set to commit");
    let runtime = runtime();
    let search = articles.search("bread").run();
    let found = runtime.block_on(async { tokio::time::timeout(Duration::from_secs(10), search).await });
    let found = found.expect("a search waited for the rows, which a change that waits for a set took first");
    assert_eq!(found.unwrap().hits.len(), 1);
    release.send(()).unwrap();
    holder.join().unwrap();
    changed.recv_timeout(Duration::from_secs(10)).expect("the change never went on once the set committed");
    changer.join().unwrap();
    runtime.block_on(set).unwrap();
    store.blocking().close().unwrap();
}

// contract: SRCH-06
#[test]
fn a_change_goes_on_while_the_set_it_waits_for_is_led_behind_searches_on_a_busy_pool() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let busy = BusyPool::occupy(&store);
    // as many searches as the pool has threads, queued before the set's lead
    let mut searches: Vec<_> = (0..crate::pool::MOST).map(|_| Box::pin(articles.search("bread").run())).collect();
    for search in &mut searches {
        assert!(polled_once(search.as_mut()).is_pending());
    }
    let mut set = Box::pin(articles.set("a-18", &article("Rye", "flour", 7)));
    assert!(polled_once(set.as_mut()).is_pending());
    let (changed, changer) = change_on_a_thread(&articles);
    // a change that took the rows first had the searches wait for them on every thread
    thread::sleep(Duration::from_millis(100));
    drop(busy);
    if changed.recv_timeout(Duration::from_secs(10)).is_err() {
        // left open: its close would wait for the pool's threads, which wait for the change
        std::mem::forget((store, articles, searches, set));
        panic!("the change waited for a set whose lead never ran");
    }
    changer.join().unwrap();
    let runtime = runtime();
    runtime.block_on(set).unwrap();
    for search in searches {
        runtime.block_on(search).unwrap();
    }
    store.blocking().close().unwrap();
}

// contract: SRCH-06
#[test]
fn a_change_leads_the_commit_of_the_set_it_waits_for_while_every_thread_of_the_pool_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let busy = BusyPool::occupy(&store);
    let rye = article("Rye", "flour", 7);
    let mut set = pin!(articles.set("a-18", &rye));
    assert!(polled_once(set.as_mut()).is_pending(), "its commit's lead waits for a thread of the pool");
    // As passes of maintenance that wait for the one a switch runs in would keep every thread.
    let (changed, changer) = change_on_a_thread(&articles);
    changed.recv_timeout(Duration::from_secs(10)).expect("the change waited for a lead no thread of the pool took");
    changer.join().unwrap();
    assert_eq!(articles.blocking().get("a-18").unwrap(), Some(rye), "the change led the set's commit");
    drop(busy);
    runtime().block_on(set).unwrap();
    store.blocking().close().unwrap();
}

#[test]
fn a_set_of_many_bytes_makes_its_words_on_the_pool_and_a_small_one_in_its_poll() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    let blocking = articles.blocking();
    let (small, large) = (article("Rye", "flour", 7), article("Rye", &"flour and water, ".repeat(512), 7));
    let busy = BusyPool::occupy(&store);
    // An empty id is refused as a set makes its words: where that runs shows in its first poll.
    let mut small_set = pin!(articles.set("", &small));
    let refused = match polled_once(small_set.as_mut()) {
        std::task::Poll::Ready(refused) => refused.unwrap_err(),
        std::task::Poll::Pending => panic!("a small set's words are made in its first poll"),
    };
    let blocked = blocking.set("", &small).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
    let mut large_set = pin!(articles.set("", &large));
    assert!(polled_once(large_set.as_mut()).is_pending(), "a large set's words are made on the pool, which is busy");
    let many: Vec<(String, &Article)> =
        (0..64).map(|at| (format!("a-{at}"), &small)).chain([(String::new(), &large)]).collect();
    let mut set_many = pin!(articles.set_many(many.iter().map(|(id, value)| (id, *value))));
    assert!(polled_once(set_many.as_mut()).is_pending(), "a set_many of many bytes makes its words on the pool");
    drop(busy);
    let runtime = runtime();
    let (refused, blocked) = (runtime.block_on(large_set).unwrap_err(), blocking.set("", &large).unwrap_err());
    assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
    let refused = runtime.block_on(set_many).unwrap_err();
    let blocked = blocking.set_many(many.iter().map(|(id, value)| (id, *value))).unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
    assert_eq!(blocking.get("a-0").unwrap(), None, "a set_many refused for one document sets none");
    store.blocking().close().unwrap();
}

// contract: STORE-10
// contract: DUR-11
#[test]
fn a_write_polled_once_commits_though_its_future_is_dropped_and_its_poll_waits_for_no_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let (release, holder) = hold_writer(&articles.blocking().search.file);
    let rye = article("Rye", "flour", 7);
    let started = Instant::now();
    {
        let (mut set, mut delete) = (pin!(articles.set("a-18", &rye)), pin!(articles.delete("a-17")));
        assert!(polled_once(set.as_mut()).is_pending(), "the writer is held, so its commit cannot have ended");
        assert!(polled_once(delete.as_mut()).is_pending());
    }
    assert!(started.elapsed() < Duration::from_secs(5), "a poll waited for the commit");
    release.send(()).unwrap();
    holder.join().unwrap();
    eventually("the set dropped after its poll never committed", || {
        articles.blocking().get("a-18").unwrap().is_some() && articles.blocking().get("a-17").unwrap().is_none()
    });
    assert_eq!(articles.blocking().get("a-18").unwrap(), Some(rye));
    store.blocking().close().unwrap();
}

// contract: DUR-04
// contract: DUR-11
#[tokio::test]
async fn a_write_whose_future_is_never_polled_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.set("a-17", &article("Sourdough bread", "salt", 42)).await.unwrap();
    drop(articles.set("a-18", &article("Rye", "flour", 7)));
    drop(articles.set_many([("a-19", &article("Spelt", "wheat", 7))]));
    drop(articles.delete("a-17"));
    articles.set("a-20", &article("Barley", "malt", 7)).await.unwrap();
    assert_eq!(articles.get("a-18").await.unwrap(), None);
    assert_eq!(articles.get("a-19").await.unwrap(), None);
    assert!(articles.get("a-17").await.unwrap().is_some(), "the delete never polled deleted nothing");
    assert!(articles.get("a-20").await.unwrap().is_some());
    store.close().await.unwrap();
}

// contract: STORE-09
#[tokio::test]
async fn a_search_whose_future_is_dropped_stops_waiting_for_a_reader() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    articles.set("a-17", &article("Sourdough bread", "salt", 42)).await.unwrap();
    let file = Arc::clone(&articles.blocking().search.file);
    // every reader of search.db taken, until the test lets them go
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Arc::new(std::sync::Mutex::new(released));
    let (taken, readers_taken) = std::sync::mpsc::channel();
    let holders: Vec<_> = (0..file.readers_at_most())
        .map(|_| {
            let (file, released, taken) = (Arc::clone(&file), Arc::clone(&released), taken.clone());
            thread::spawn(move || {
                file.read(|_| {
                    taken.send(()).unwrap();
                    let _ = released.lock().unwrap().recv_timeout(Duration::from_secs(10));
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect();
    for _ in &holders {
        readers_taken.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    let search = tokio::spawn(articles.search("bread").run());
    let deadline = Instant::now() + Duration::from_secs(10);
    while file.readers_waiting() == 0 {
        assert!(Instant::now() < deadline, "the search never waited for a reader");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    search.abort();
    assert!(search.await.unwrap_err().is_cancelled());
    let deadline = Instant::now() + Duration::from_secs(10);
    while file.readers_waiting() > 0 {
        assert!(Instant::now() < deadline, "the dropped search still waits for a reader");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    drop(release);
    for holder in holders {
        holder.join().unwrap();
    }
    let found = articles.search("bread").run().await.unwrap();
    assert_eq!(found.hits.len(), 1, "a search after it answers");
    store.close().await.unwrap();
}

// contract: STORE-10
#[test]
fn ready_waits_on_no_thread_and_wakes_as_the_rebuild_switches_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, true);
    let titles = store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    titles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let search = Arc::clone(&titles.blocking().search);
    let held = search.hold_maintenance();
    let both = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    let runtime = runtime();
    runtime.block_on(async {
        assert!(matches!(both.status().await.unwrap(), Status::Rebuilding { .. }));
        let waiting = tokio::spawn(both.ready());
        // One thread runs both: a ready that parked it would starve this task.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "nothing moved the rebuild on");
        let (search, open) = (Arc::clone(&both.blocking().search), Arc::clone(&both.blocking().open));
        let switch = thread::spawn(move || search.rebuild(&open));
        waiting.await.unwrap().unwrap();
        switch.join().unwrap().unwrap();
        assert_eq!(both.status().await.unwrap(), Status::Ready);
        assert_eq!(both.search("salt").run().await.unwrap().hits.len(), 1, "the body is read now");
    });
    drop(held);
    runtime.block_on(store.close()).unwrap();
}

#[test]
fn a_dropped_ready_stops_waiting_and_the_rebuild_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, true);
    let titles = store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    titles.blocking().set("a-17", &article("Sourdough bread", "salt", 42)).unwrap();
    let search = Arc::clone(&titles.blocking().search);
    let held = search.hold_maintenance();
    let both = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    let runtime = runtime();
    let timed = runtime.block_on(async { tokio::time::timeout(Duration::from_millis(300), both.ready()).await });
    assert!(timed.is_err(), "the rebuild cannot have switched while maintenance is held");
    drop(held);
    both.blocking().ready().unwrap();
    assert_eq!(runtime.block_on(both.status()).unwrap(), Status::Ready);
    runtime.block_on(store.close()).unwrap();
}

#[tokio::test]
async fn ready_runs_the_maintenance_itself_on_a_store_without_background_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let titles = store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    titles.set("a-17", &article("Sourdough bread", "salt", 42)).await.unwrap();
    let both = store.index::<Article>("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
    assert!(matches!(both.status().await.unwrap(), Status::Rebuilding { .. }));
    assert!(both.search("salt").run().await.unwrap().hits.is_empty(), "the index answers as before until then");
    tokio::spawn(both.ready()).await.unwrap().unwrap();
    assert_eq!(both.status().await.unwrap(), Status::Ready);
    assert_eq!(both.search("salt").run().await.unwrap().hits.len(), 1);
    store.close().await.unwrap();
}

#[test]
fn a_wait_for_the_next_settle_on_a_store_that_closed_answers_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, true);
    let articles = articles(&store);
    store.blocking().close().unwrap();
    let (answer, answered) = mpsc::channel();
    articles.blocking().settled_then(move |settled| answer.send(settled).unwrap());
    let closed = answered.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = runtime().block_on(articles.ready()).unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
}

#[tokio::test]
async fn the_stores_calls_list_and_drop_indexes_as_the_blocking_ones_do() {
    let dir = tempfile::tempdir().unwrap();
    let store = opened(&dir, false);
    let articles = articles(&store);
    let notes = store.index::<String>("notes").open().unwrap();
    articles.set("a-17", &article("Sourdough bread", "salt", 42)).await.unwrap();
    notes.set("n-1", &"Кошки спят на печи".to_owned()).await.unwrap();
    let listed = list(&store).await.unwrap();
    assert_eq!(listed, crate::search::list(store.blocking()).unwrap());
    assert_eq!(listed.iter().map(|index| index.name.as_str()).collect::<Vec<_>>(), ["articles", "notes"]);

    assert!(store.drop_index("notes").await.unwrap());
    assert!(!store.drop_index("notes").await.unwrap(), "there is none any more");
    let refused = notes.get("n-1").await.unwrap_err();
    let blocked = notes.blocking().get("n-1").unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
    assert_eq!(list(&store).await.unwrap().len(), 1);
    store.close().await.unwrap();
}

#[tokio::test]
async fn from_and_blocking_cross_between_the_two_apis_on_one_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let blocking = store.index::<Article>("articles").field("title", 2.0).open().unwrap();
    let awaited = Index::from(blocking.clone());
    awaited.set("a-17", &article("Sourdough bread", "salt", 42)).await.unwrap();
    assert_eq!(blocking.get("a-17").unwrap(), awaited.blocking().get("a-17").unwrap());
    assert_eq!(awaited.name(), "articles");
    store.close().unwrap();
    let closed = awaited.search("bread").run().await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = awaited.set("a-18", &article("Rye", "flour", 7)).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    let closed = awaited.get("a-17").await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    drop(store);
    let gone = Index::from(blocking);
    let closed = gone.status().await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "a handle whose store is gone answers closed: {closed}");
}

#[cfg(feature = "replication")]
mod copy {
    use serde_json::{Value, json};

    use super::*;
    use crate::replication::Node;

    fn node_with(dir: &std::path::Path, node: Node) -> crate::Store {
        crate::Store::open(dir, Options { node: Some(node), background: false, ..Options::default() }).unwrap()
    }

    // contract: STORE-10
    #[tokio::test]
    async fn a_write_awaited_on_a_copy_is_passed_to_its_home_from_the_pool_never_from_the_poll() {
        let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let home = node_with(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
        let address = home.node().unwrap().listening.lock().unwrap().unwrap().to_string();
        let home_docs = home.index::<Value>("docs").field("title", 1.0).open().unwrap();
        home_docs.set("seed", &json!({ "title": "planted at the home" })).unwrap();
        let copy = Store::from(node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() }));
        let docs = copy.index::<Value>("docs").field("title", 1.0).open().unwrap();
        assert_eq!(docs.search("planted").run().await.unwrap().hits[0].id, "seed");
        assert!(docs.blocking().search.file.is_copy());

        // A pass to the home in the poll would fail its submit, Internal.
        docs.set("far", &json!({ "title": "written on a copy" })).await.unwrap();
        assert_eq!(docs.search("copy").run().await.unwrap().hits[0].id, "far", "the copy finds its own set");
        assert_eq!(home_docs.search("copy").run().unwrap().hits[0].id, "far");
        let many = [("a", json!({ "title": "first of many" })), ("b", json!({ "title": "second of many" }))];
        docs.set_many(many.iter().map(|(id, value)| (*id, value))).await.unwrap();
        assert_eq!(docs.search("many").run().await.unwrap().hits.len(), 2);
        assert!(docs.delete("far").await.unwrap());
        assert!(home_docs.get("far").unwrap().is_none());
        assert!(!docs.delete("far").await.unwrap());
        let refused = docs.delete("").await.unwrap_err();
        let blocked = docs.blocking().delete("").unwrap_err();
        assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
        copy.close().await.unwrap();
        home.close().unwrap();
    }
}

#[cfg(feature = "sql")]
mod table {
    use super::*;
    use crate::sql;

    #[derive(Clone, Debug, PartialEq, Deserialize)]
    struct Row {
        id: i64,
        title: String,
        body: String,
    }

    const ARTICLES: &str =
        "create table articles (id integer primary key, title text not null, body text not null default '')";

    // contract: SRCH-07
    #[tokio::test]
    async fn a_tables_index_awaited_answers_as_the_blocking_one_and_searches_in_a_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = opened(&dir, false);
        let db = store.blocking().database("blog").migrations([("0001_articles.sql", ARTICLES)]).open().unwrap();
        db.exec(sql!("insert into articles (id, title, body) values (?, ?, ?)", 1, "Sourdough bread", "salt")).unwrap();
        let builder = TableIndexBuilder::from(db.search_index::<Row>("articles_search"));
        let articles = builder.table("articles").field("title", 2.0).field("body", 1.0).open().unwrap();
        assert!(matches!(articles.status().await.unwrap(), Status::Rebuilding { .. }), "its rows are read first");
        tokio::spawn(articles.ready()).await.unwrap().unwrap();
        assert_eq!(articles.status().await.unwrap(), Status::Ready);

        let found = articles.search("bread").run().await.unwrap();
        assert_eq!(found, articles.blocking().search("bread").run().unwrap());
        assert_eq!(found.hits[0].value, Row { id: 1, title: "Sourdough bread".into(), body: "salt".into() });
        db.exec(sql!("insert into articles (id, title) values (2, 'Rye bread')")).unwrap();
        assert_eq!(articles.search("bread").run().await.unwrap().hits.len(), 2, "the row's commit changed it");

        let seen = db
            .tx(|tx| -> crate::Result<usize> {
                tx.exec(sql!("update articles set title = 'Bread without yeast' where id = 1"))?;
                Ok(tx.with(&articles).search("yeast").run()?.hits.len()) // blocking, as every call in a transaction
            })
            .unwrap();
        assert_eq!(seen, 1, "the transaction's own write");
        let again = TableIndex::from(articles.blocking().clone());
        assert_eq!(again.search("yeast").run().await.unwrap().hits.len(), 1);
        store.close().await.unwrap();
    }
}
