use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::blobs::fixture::{Fixture, HOUR, bytes, fixture};
use crate::{ErrorKind, Options};

/// The fixture's store, its calls awaited, and a set of files in it.
fn awaited(f: &Fixture, name: &str) -> (Store, Files) {
    let store = Store::from(f.store.clone());
    let files = store.files(name).open().unwrap();
    (store, files)
}

/// Holds blobs.db's writer on a thread of its own until `release` is sent,
/// or for a second at most: a poll that led a commit itself would wait for
/// it there, then answer, rather than hang the test.
fn hold_the_writer(files: &Files) -> (mpsc::Sender<()>, thread::JoinHandle<()>) {
    let (release, held_until) = mpsc::channel::<()>();
    let (holding, held) = mpsc::channel();
    let blobs = Arc::clone(&files.blocking.blobs);
    let holder = thread::spawn(move || {
        let _writer = blobs.file.hold_writer();
        holding.send(()).unwrap();
        let _ = held_until.recv_timeout(Duration::from_secs(1));
    });
    held.recv().unwrap();
    (release, holder)
}

fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// Waits until `holds`, ten seconds at most: what a write dropped after its
/// poll did on the pool shows up a moment after.
fn eventually(what: &str, holds: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !holds() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn each_call_answers_as_its_blocking_twin_does_on_the_same_store() {
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    let blocking = f.files("docs");
    for (path, size) in [("small", 100), ("large", 3 << 20)] {
        let body = bytes(size);
        let info = docs.key(path).content_type("text/csv").meta("name", "report.csv").put(body.clone()).await.unwrap();
        assert_eq!(blocking.head(path).unwrap(), Some(info.clone()), "{path}");
        assert_eq!(docs.head(path).await.unwrap(), Some(info.clone()));
        let read = docs.get(path).await.unwrap().expect("just written");
        assert_eq!(read.info(), &info);
        assert_eq!(read.read_all().await.unwrap(), body);
    }
    assert!(!docs.create("small", b"again".to_vec()).await.unwrap(), "a create finds the file");
    assert!(docs.create("new", "first").await.unwrap());
    assert_eq!(blocking.get("new").unwrap().unwrap().read_all().unwrap(), b"first");

    let copied = docs.copy("large", "large (copy)").await.unwrap();
    assert_eq!(blocking.head("large (copy)").unwrap(), Some(copied));
    assert_eq!(f.objects().len(), 1, "a copy shares its bytes");
    let refused = docs.copy("small", "large").await.unwrap_err();
    let blocked = blocking.copy("small", "large").unwrap_err();
    assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()));
    let renamed = docs.rename("large (copy)", "moved").await.unwrap();
    assert_eq!((blocking.head("large (copy)").unwrap(), blocking.head("moved").unwrap()), (None, Some(renamed)));

    let stale = docs.key("small").if_match("\"0000\"").delete().await.unwrap_err();
    assert_eq!(stale.kind(), ErrorKind::Conflict, "{stale}");
    assert!(docs.expire("new", HOUR).await.unwrap());
    assert!(blocking.head("new").unwrap().unwrap().expires.is_some());
    assert!(!docs.expire("gone", HOUR).await.unwrap());
    docs.delete("small").await.unwrap();
    docs.delete("small").await.unwrap(); // nothing to delete is not an error
    assert_eq!(blocking.head("small").unwrap(), None);
}

// contract: BLOB-08
#[tokio::test]
async fn a_path_that_is_not_one_is_refused_as_the_blocking_call_refuses_it() {
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    for path in ["", "a//b", "../a", "a/"] {
        let refused = docs.put(path, b"x".to_vec()).await.unwrap_err();
        let blocked = f.files("docs").put(path, b"x").unwrap_err();
        assert_eq!((refused.kind(), refused.to_string()), (blocked.kind(), blocked.to_string()), "{path:?}");
        assert_eq!(docs.head(path).await.unwrap_err().kind(), ErrorKind::Invalid);
        assert_eq!(docs.delete(path).await.unwrap_err().kind(), ErrorKind::Invalid);
    }
    let error = docs.folder("..").upload("a").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[tokio::test]
async fn files_cross_between_the_two_apis_as_one_set() {
    let f = fixture();
    let blocking = f.files("docs");
    let docs = Files::from(blocking.clone());
    docs.folder("users").folder(42).put("a.md", b"written awaited".to_vec()).await.unwrap();
    let read = blocking.folder("users").folder(42).get("a.md").unwrap().unwrap().read_all().unwrap();
    assert_eq!(read, b"written awaited");
    docs.blocking().put("b.md", b"written blocking").unwrap();
    assert!(docs.head("b.md").await.unwrap().is_some());
}

// contract: STORE-10
#[test]
fn an_awaited_write_commits_on_the_pool_never_on_the_thread_that_polls() {
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    f.files("docs").put("a", b"there").unwrap();
    let (release, holder) = hold_the_writer(&docs);
    let mut small = pin!(docs.put("small", bytes(100)));
    let mut large = pin!(docs.put("large", bytes(1 << 20)));
    let mut deleted = pin!(docs.delete("a"));
    let mut cleared = pin!(docs.folder("users").clear());
    assert!(poll_once(small.as_mut()).is_pending(), "the writer is held, so no commit can have ended");
    assert!(poll_once(large.as_mut()).is_pending());
    assert!(poll_once(deleted.as_mut()).is_pending());
    assert!(poll_once(cleared.as_mut()).is_pending());
    release.send(()).unwrap();
    holder.join().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(async {
        assert_eq!(small.await.unwrap().size, 100);
        assert_eq!(large.await.unwrap().size, 1 << 20);
        deleted.await.unwrap();
        cleared.await.unwrap();
    });
    assert_eq!(f.files("docs").head("a").unwrap(), None);
}

// contract: DUR-11, DUR-04
#[test]
fn a_put_polled_once_commits_though_its_future_is_dropped_and_one_never_polled_writes_nothing() {
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    let (release, holder) = hold_the_writer(&docs);
    for (path, size) in [("small", 100), ("large", 1 << 20)] {
        let mut put = pin!(docs.put(path, bytes(size)));
        assert!(poll_once(put.as_mut()).is_pending());
    }
    drop(docs.put("never", bytes(100)));
    drop(docs.key("never either").put(bytes(1 << 20)));
    release.send(()).unwrap();
    holder.join().unwrap();
    let blocking = f.files("docs");
    eventually("the puts dropped after their poll never committed", || {
        blocking.head("small").unwrap().is_some() && blocking.head("large").unwrap().is_some()
    });
    assert_eq!(blocking.get("large").unwrap().unwrap().read_all().unwrap(), bytes(1 << 20));
    assert_eq!((blocking.head("never").unwrap(), blocking.head("never either").unwrap()), (None, None));
    assert!(f.uploads().is_empty());
}

/// A store whose memory holds one upload, and what takes it now: the files
/// in it hold a block of content ids, so that a put inline needs no commit
/// of its own for one.
fn memory_of_one_upload(dir: &tempfile::TempDir) -> (Store, Files, crate::Reservation) {
    let options = Options { background: false, memory: Some(20 << 10), ..Options::default() };
    let store = Store::open(dir.path(), options).unwrap();
    let docs = store.files("docs").open().unwrap();
    docs.blocking().put("first", b"takes a block of content ids").unwrap();
    let held = store.blocking().memory().try_reserve(10 << 10, "a test's read").unwrap();
    (store, docs, held)
}

// contract: DUR-11, STORE-10
#[test]
fn a_put_that_waits_for_memory_parks_no_thread_and_commits_once_it_is_given_though_its_future_went() {
    let dir = tempfile::tempdir().unwrap();
    let (store, docs, held) = memory_of_one_upload(&dir);
    {
        let mut put = pin!(docs.put("a", b"waited for".to_vec()));
        assert!(poll_once(put.as_mut()).is_pending(), "16 KiB do not fit beside the 10 held");
    }
    assert_eq!(docs.blocking().head("a").unwrap(), None);
    // The put goes on where the memory comes back, here: its commit's lead
    // goes to the pool, so that a thread giving memory back, an executor's,
    // never waits for a writer, which is held now.
    let (release, holder) = hold_the_writer(&docs);
    drop(held);
    assert_eq!(docs.blocking().head("a").unwrap(), None, "the writer is held: the commit waits on the pool");
    release.send(()).unwrap();
    holder.join().unwrap();
    eventually("the put never took the memory given back", || docs.blocking().head("a").unwrap().is_some());
    eventually("the put never gave its memory back", || store.blocking().memory().used() == 0);
    store.blocking().close().unwrap();
}

#[test]
fn an_upload_dropped_while_it_waits_for_memory_leaves_the_line_and_holds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, docs, held) = memory_of_one_upload(&dir);
    {
        let mut upload = pin!(docs.upload("a"));
        assert!(poll_once(upload.as_mut()).is_pending());
    }
    drop(held);
    assert_eq!(store.blocking().memory().used(), 0, "the dropped upload took nothing");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let taken = runtime.block_on(async { docs.upload("b").await.unwrap() });
    assert_eq!(store.blocking().memory().used(), 16 << 10);
    drop(taken);
    store.blocking().close().unwrap();
}

#[tokio::test]
async fn the_bytes_a_write_freed_are_gone_once_it_answers() {
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    docs.put("a", bytes(1 << 20)).await.unwrap();
    docs.put("a", bytes(2 << 20)).await.unwrap();
    assert_eq!(f.objects().len(), 1, "the replaced file's bytes went before the put answered");
    docs.delete("a").await.unwrap();
    assert!(f.objects().is_empty(), "the deleted file's bytes went before the delete answered");
}

#[tokio::test]
async fn a_call_on_a_store_that_closed_answers_closed() {
    let f = fixture();
    let (store, docs) = awaited(&f, "docs");
    docs.put("a", b"x".to_vec()).await.unwrap();
    store.close().await.unwrap();
    let errors = [
        docs.put("b", b"x".to_vec()).await.unwrap_err(),
        docs.put("c", bytes(1 << 20)).await.unwrap_err(),
        docs.head("a").await.unwrap_err(),
        docs.get("a").await.unwrap_err(),
        docs.delete("a").await.unwrap_err(),
        docs.usage().await.unwrap_err(),
    ];
    for error in errors {
        assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
    }
}

#[test]
fn every_future_can_be_spawned() {
    fn spawnable<T: Future + Send + 'static>(_: &T) {}
    let f = fixture();
    let (_store, docs) = awaited(&f, "docs");
    let calls = (docs.put("a", Vec::new()), docs.create("a", ""), docs.get("a"), docs.head("a"), docs.delete("a"));
    spawnable(&calls.0);
    spawnable(&calls.1);
    spawnable(&calls.2);
    spawnable(&calls.3);
    spawnable(&calls.4);
    spawnable(&docs.copy("a", "b"));
    spawnable(&docs.rename("a", "b"));
    spawnable(&docs.expire("a", HOUR));
    spawnable(&docs.upload("a"));
    spawnable(&docs.usage());
    spawnable(&docs.clear());
    spawnable(&docs.list().page());
}

// contract: STORE-10
#[cfg(feature = "replication")]
#[tokio::test]
#[expect(clippy::await_holding_lock, reason = "the gate's guard is what keeps the pool's threads held while it awaits")]
async fn on_a_copy_a_write_waits_for_its_home_on_a_thread_of_its_own_while_every_thread_of_the_pool_is_held() {
    use crate::replication::Node;
    let node = |dir: &std::path::Path, node: Node| {
        Store::open(dir, Options { node: Some(node), background: false, ..Options::default() }).unwrap()
    };
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let from = vec![home.blocking().node().unwrap().address()];
    let copy = node(copy_dir.path(), Node { from, ..Node::default() });
    let at_home = home.files("docs").open().unwrap();
    at_home.put("seed", b"planted at the home".to_vec()).await.unwrap();
    // a set opened on a copy that has not taken it yet is refused there
    let started = Instant::now();
    let docs = loop {
        match copy.files("docs").open() {
            Ok(docs) => break docs,
            Err(refused) => assert!(started.elapsed() < Duration::from_secs(30), "the copy never took it: {refused}"),
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    while docs.head("seed").await.unwrap().is_none() {
        assert!(started.elapsed() < Duration::from_secs(30), "the copy never took the home's file");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // A write that waited for the home on the pool would queue behind these,
    // which wait for the gate until it drops, a panic's unwinding too.
    let (pool, gate) = (Arc::clone(copy.pool()), Arc::new(std::sync::RwLock::new(())));
    let closed = gate.write().unwrap();
    for _ in 0..crate::pool::MOST {
        let gate = Arc::clone(&gate);
        pool.run(Box::new(move || drop(gate.read())));
    }
    let within = async |what: &str, write: std::pin::Pin<&mut (dyn Future<Output = Result<()>> + Send)>| {
        let written = tokio::time::timeout(Duration::from_secs(10), write).await;
        written.unwrap_or_else(|_| panic!("{what} on the copy waited for a thread of its pool")).unwrap();
    };
    within("a put", pin!(async { docs.put("far", b"written on a copy".to_vec()).await.map(drop) })).await;
    within("a create", pin!(async { docs.create("new", b"made on a copy".to_vec()).await.map(drop) })).await;
    within("a copy", pin!(async { docs.copy("far", "far (copy)").await.map(drop) })).await;
    within("a rename", pin!(async { docs.rename("far (copy)", "moved").await.map(drop) })).await;
    within("a delete", pin!(docs.delete("new"))).await;
    within("a clear", pin!(docs.folder("drafts").clear())).await;
    drop(closed);

    let mut upload = docs.upload("uploaded").await.unwrap();
    upload.write(&bytes(64 << 10)).await.unwrap();
    upload.commit().await.unwrap();
    for (path, there) in [("far", true), ("moved", true), ("new", false), ("uploaded", true)] {
        assert_eq!(at_home.head(path).await.unwrap().is_some(), there, "{path} at the home");
    }
    drop((at_home, docs));
    copy.close().await.unwrap();
    home.close().await.unwrap();
}
