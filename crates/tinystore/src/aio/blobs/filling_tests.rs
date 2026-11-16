use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::aio::Store;
use crate::aio::blobs::Files;
use crate::blobs::fixture::{Fixture, fixture};
use crate::{Error, ErrorKind};

fn awaited(f: &Fixture) -> Files {
    Store::from(f.store.clone()).files("pages").open().unwrap()
}

/// Waits until the store's blobs have `runs` runs under way and `waiters`
/// calls waiting, yielding to the tasks that make them.
async fn until_under_way(f: &Fixture, runs: usize, waiters: usize) {
    let started = Instant::now();
    while f.blobs().fills.under_way() != (runs, waiters) {
        assert!(started.elapsed() < Duration::from_secs(10), "under way: {:?}", f.blobs().fills.under_way());
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// A blocking run in a thread of its own whose function waits for `go`.
fn held_run(f: &Fixture, answer: &'static str) -> (mpsc::Sender<()>, thread::JoinHandle<Vec<u8>>) {
    let files = f.files("pages");
    let (started, has_started) = mpsc::channel();
    let (go, wait_for_go) = mpsc::channel::<()>();
    let run = thread::spawn(move || {
        let page = files.key("a.pdf").get_or_create(|| {
            let _ = started.send(());
            let _ = wait_for_go.recv();
            Ok::<_, Error>(answer)
        });
        page.unwrap().read_all().unwrap()
    });
    has_started.recv().unwrap();
    (go, run)
}

#[tokio::test]
async fn an_awaited_miss_runs_its_future_once_and_a_blocking_call_reads_its_file() {
    let f = fixture();
    let pages = awaited(&f);
    let runs = AtomicUsize::new(0);
    let convert = || async {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok::<_, Error>(b"%PDF".to_vec())
    };
    let page = pages.key("a.pdf").content_type("application/pdf").get_or_create(convert).await.unwrap();
    assert_eq!(page.info().content_type, "application/pdf");
    assert_eq!(page.read_all().await.unwrap(), b"%PDF");
    let again = pages.key("a.pdf").get_or_create(convert).await.unwrap();
    assert_eq!(again.info(), page.info());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let blocking = pages.blocking().key("a.pdf").get_or_create(|| Ok::<_, Error>("never")).unwrap();
    assert_eq!(blocking.info(), page.info());
}

// contract: BLOB-21
#[tokio::test]
async fn an_awaited_call_waits_for_a_blocking_run_on_no_thread_and_gets_its_file() {
    let f = fixture();
    let pages = awaited(&f);
    let (go, run) = held_run(&f, "blocking");
    let waiting = tokio::spawn({
        let pages = pages.clone();
        async move { pages.key("a.pdf").get_or_create(|| async { Ok::<_, Error>("awaited") }).await }
    });
    until_under_way(&f, 1, 1).await;
    go.send(()).unwrap();
    let page = waiting.await.unwrap().unwrap();
    assert_eq!(page.read_all().await.unwrap(), b"blocking");
    assert_eq!(run.join().unwrap(), b"blocking");
}

#[tokio::test]
async fn a_call_dropped_while_it_waits_leaves_and_the_run_goes_on() {
    let f = fixture();
    let pages = awaited(&f);
    let (go, run) = held_run(&f, "made");
    let waiting = tokio::spawn({
        let pages = pages.clone();
        async move { pages.key("a.pdf").get_or_create(|| async { Ok::<_, Error>("never") }).await }
    });
    until_under_way(&f, 1, 1).await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap(), b"made");
    assert_eq!(f.blobs().fills.under_way(), (0, 0));
}

#[tokio::test]
async fn a_call_dropped_while_its_future_runs_lets_the_claim_go_and_writes_nothing() {
    let f = fixture();
    let pages = awaited(&f);
    let (started, has_started) = tokio::sync::oneshot::channel();
    let mut started = Some(started);
    let running = tokio::spawn({
        let pages = pages.clone();
        async move {
            let call = pages.key("a.pdf").get_or_create(move || {
                if let Some(started) = started.take() {
                    let _ = started.send(());
                }
                std::future::pending::<Result<&'static str, Error>>()
            });
            call.await
        }
    });
    has_started.await.unwrap();
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_eq!(f.blobs().fills.under_way(), (0, 0));
    let page = pages.key("a.pdf").get_or_create(|| async { Ok::<_, Error>("the next") }).await.unwrap();
    assert_eq!(page.read_all().await.unwrap(), b"the next");
}

// contract: BLOB-22
#[tokio::test]
async fn an_awaited_run_s_failure_is_the_answer_of_a_blocking_call_that_waited() {
    let f = fixture();
    let pages = awaited(&f);
    let (go, wait_for_go) = tokio::sync::oneshot::channel::<()>();
    let mut wait_for_go = Some(wait_for_go);
    let failing = tokio::spawn({
        let pages = pages.clone();
        async move {
            let call = pages.key("bad.pdf").get_or_create(move || {
                let wait_for_go = wait_for_go.take();
                async move {
                    if let Some(go) = wait_for_go {
                        let _ = go.await;
                    }
                    Err::<&str, _>(Error::new(ErrorKind::Invalid, "the document does not parse"))
                }
            });
            call.await
        }
    });
    until_under_way(&f, 1, 0).await;
    let blocking = pages.blocking().clone();
    let waited = thread::spawn(move || blocking.key("bad.pdf").get_or_create(|| Ok::<_, Error>("never")).map(drop));
    until_under_way(&f, 1, 1).await;
    go.send(()).unwrap();
    assert_eq!(failing.await.unwrap().unwrap_err().kind(), ErrorKind::Invalid);
    let error = waited.join().unwrap().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "a store error as it is: {error}");
    assert!(error.to_string().contains("the document does not parse"), "{error}");
}
