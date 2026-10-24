use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use super::*;
use crate::ErrorKind;
use crate::kv::fixture::{Fixture, eventually, fixture};

fn awaited(f: &Fixture) -> Store {
    Store::from(f.store.clone())
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// A charge that counts how often it ran.
async fn charge(runs: &AtomicUsize) -> Result<String> {
    let run = runs.fetch_add(1, Ordering::SeqCst) + 1;
    Ok(format!("receipt {run}"))
}

// contract: KV-11
#[tokio::test]
async fn a_run_keeps_its_answer_as_a_blocking_run_and_reads_the_blocking_ones() {
    let f = fixture();
    let store = awaited(&f);
    let charges = store.once::<String>("charges").open().unwrap();
    let runs = AtomicUsize::new(0);
    assert_eq!(charges.run("req-7", || charge(&runs)).await.unwrap(), "receipt 1");
    assert_eq!(charges.run("req-7", || charge(&runs)).await.unwrap(), "receipt 1", "kept: run once");
    assert_eq!(charges.blocking().run("req-7", || Ok::<_, Error>("never".to_owned())).unwrap(), "receipt 1");
    assert_eq!(charges.blocking().run("req-8", || Ok::<_, Error>("blocking".to_owned())).unwrap(), "blocking");
    assert_eq!(charges.run("req-8", || charge(&runs)).await.unwrap(), "blocking");
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    assert_eq!(charges.get("req-7").await.unwrap(), charges.blocking().get("req-7").unwrap());
    assert!(charges.delete("req-7").await.unwrap());
    assert!(!charges.delete("req-7").await.unwrap());
    assert_eq!(charges.run("req-7", || charge(&runs)).await.unwrap(), "receipt 2", "forgotten: it runs again");
    assert_eq!(charges.under("eu").get("req-8").await.unwrap(), None, "a branch keeps its own answers");
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_run_takes_the_callers_own_error_and_keeps_nothing_for_it() {
    #[derive(Debug)]
    enum Payment {
        Declined,
        Store(Error),
    }
    impl From<Error> for Payment {
        fn from(error: Error) -> Self {
            Payment::Store(error)
        }
    }
    let f = fixture();
    let store = awaited(&f);
    let charges = store.once::<String>("charges").open().unwrap();
    let declined = charges.run("req-1", || async { Err(Payment::Declined) }).await;
    assert!(matches!(declined, Err(Payment::Declined)));
    assert_eq!(charges.get("req-1").await.unwrap(), None, "an error keeps nothing");
    let refused = charges.under("").run("req-1", || async { Ok::<_, Payment>("never".to_owned()) }).await;
    assert!(matches!(refused, Err(Payment::Store(error)) if error.kind() == ErrorKind::Invalid));
    f.store.close().unwrap();
}

// contract: KV-11, STORE-10
#[tokio::test]
async fn a_run_waits_for_the_one_under_way_on_no_thread_and_takes_its_answer() {
    let f = fixture();
    let store = awaited(&f);
    let charges = store.once::<String>("charges").open().unwrap();
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let (charged, charging) = tokio::sync::oneshot::channel::<()>();
    // One thread runs both tasks: a wait that parked it would never let the first run end.
    let first = tokio::spawn({
        let (charges, runs) = (charges.clone(), std::sync::Arc::clone(&runs));
        async move {
            let ran = &runs;
            charges
                .run("req-7", move || async move {
                    charging.await.unwrap();
                    charge(ran).await
                })
                .await
        }
    });
    eventually_in_task(|| under_way(&charges) == (1, 0)).await;
    let second = tokio::spawn({
        let (charges, runs) = (charges.clone(), std::sync::Arc::clone(&runs));
        async move { charges.run("req-7", || charge(&runs)).await }
    });
    eventually_in_task(|| under_way(&charges) == (1, 1)).await;
    charged.send(()).unwrap();
    assert_eq!(first.await.unwrap().unwrap(), "receipt 1");
    assert_eq!(second.await.unwrap().unwrap(), "receipt 1", "it waited for the first run's answer");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    f.store.close().unwrap();
}

// contract: KV-11
#[tokio::test]
async fn a_run_dropped_while_its_function_runs_lets_the_next_run_run() {
    let f = fixture();
    let store = awaited(&f);
    let charges = store.once::<String>("charges").open().unwrap();
    assert_eq!(charges.get("req-0").await.unwrap(), None, "a read leaves its reader idle");
    {
        let mut abandoned = pin!(charges.run("req-7", std::future::pending::<Result<String>>));
        assert!(poll_once(abandoned.as_mut()).is_pending(), "claimed, its function under way");
    }
    let runs = AtomicUsize::new(0);
    assert_eq!(charges.run("req-7", || charge(&runs)).await.unwrap(), "receipt 1", "the key was let go");
    f.store.close().unwrap();
}

// contract: KV-11, DUR-11
#[test]
fn a_run_dropped_once_its_function_answered_still_keeps_its_answer() {
    let f = fixture();
    let store = awaited(&f);
    let charges = store.once::<String>("charges").open().unwrap();
    assert_eq!(charges.blocking().get("req-0").unwrap(), None, "a read leaves its reader idle");
    {
        let _writer = f.hold_writer();
        let mut kept = pin!(charges.run("req-7", || async { Ok::<_, Error>("receipt 1".to_owned()) }));
        assert!(poll_once(kept.as_mut()).is_pending(), "its keep is queued at the writer");
    }
    eventually("the answer dropped after its function was kept", || {
        charges.blocking().get("req-7").unwrap().as_deref() == Some("receipt 1")
    });
    let again = charges.blocking().run("req-7", || Ok::<_, Error>("never".to_owned())).unwrap();
    assert_eq!(again, "receipt 1", "kept, and the key let go after");
    f.store.close().unwrap();
}

// contract: KV-11
#[tokio::test]
async fn an_answer_too_large_to_keep_still_reaches_the_runs_that_waited() {
    let f = fixture();
    let store = awaited(&f);
    let reports = store.once::<String>("reports").open().unwrap();
    let large = "x".repeat(2 << 20);
    let (made, making) = tokio::sync::oneshot::channel::<()>();
    let first = tokio::spawn({
        let (reports, large) = (reports.clone(), large.clone());
        async move {
            reports
                .run("q3", move || async move {
                    making.await.unwrap();
                    Ok::<_, Error>(large)
                })
                .await
        }
    });
    eventually_in_task(|| under_way(&reports) == (1, 0)).await;
    let second = tokio::spawn({
        let reports = reports.clone();
        async move { reports.run("q3", || async { Ok::<_, Error>("ran again".to_owned()) }).await }
    });
    eventually_in_task(|| under_way(&reports) == (1, 1)).await;
    made.send(()).unwrap();
    assert_eq!(first.await.unwrap().unwrap().len(), large.len());
    assert_eq!(second.await.unwrap().unwrap().len(), large.len(), "it got the answer no row kept");
    assert_eq!(reports.get("q3").await.unwrap(), None);
    f.store.close().unwrap();
}

/// The keys whose run is under way in the engine of `once`, and the runs
/// waiting for them.
fn under_way<V: Value>(once: &Once<V>) -> (usize, usize) {
    once.blocking().answers().scope.kv.runs().under_way()
}

/// Yields to the runtime's other tasks until `holds`.
async fn eventually_in_task(mut holds: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !holds() {
        assert!(std::time::Instant::now() < deadline, "not within ten seconds");
        tokio::task::yield_now().await;
    }
}

// contract: DUR-07
#[tokio::test]
async fn a_run_of_a_key_inside_its_own_run_fails_rather_than_waits() {
    let f = fixture();
    let store = awaited(&f);
    let imports = store.once::<i64>("imports").open().unwrap();
    let inner = imports.clone();
    let nested =
        imports.run("file-1", || async move { inner.run("file-1", || async { Ok::<_, Error>(1) }).await }).await;
    let error = nested.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");

    let blocking = imports.blocking().clone();
    let nested = imports.run("file-1", || async move { blocking.run("file-1", || Ok::<_, Error>(1)) }).await;
    assert_eq!(nested.unwrap_err().kind(), ErrorKind::Invalid, "a blocking run inside an awaited one");

    let other = imports.run("file-2", || async { Ok::<_, Error>(2) });
    let unrelated = imports.run("file-1", || other).await;
    assert_eq!(unrelated.unwrap(), 2, "a run of another key inside a run goes on");
    assert_eq!(
        imports.run("file-1", || async { Ok::<_, Error>(3) }).await.unwrap(),
        2,
        "kept by the run that answered"
    );
    f.store.close().unwrap();
}

// contract: DUR-07
#[test]
fn an_awaited_run_inside_a_blocking_run_of_its_key_fails_rather_than_waits() {
    let f = fixture();
    let store = awaited(&f);
    let imports = store.once::<i64>("imports").open().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let nested =
        imports.blocking().run("file-1", || runtime.block_on(imports.run("file-1", || async { Ok::<_, Error>(1) })));
    assert_eq!(nested.unwrap_err().kind(), ErrorKind::Invalid);
    f.store.close().unwrap();
}

#[tokio::test]
async fn a_key_of_one_store_runs_inside_a_run_of_the_same_key_of_another() {
    let (f, g) = (fixture(), fixture());
    let (here, there) = (awaited(&f), awaited(&g));
    let (imports, elsewhere) =
        (here.once::<i64>("imports").open().unwrap(), there.once::<i64>("imports").open().unwrap());
    let inner = elsewhere.run("file-1", || async { Ok::<_, Error>(1) });
    assert_eq!(imports.run("file-1", || inner).await.unwrap(), 1);
    f.store.close().unwrap();
    g.store.close().unwrap();
}
