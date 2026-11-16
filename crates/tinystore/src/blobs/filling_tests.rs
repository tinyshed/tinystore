use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::ErrorKind;
use crate::blobs::fixture::{Fixture, bytes, fixture};
use crate::blobs::upload::INLINE;

/// A program's own error, which a store's error becomes too.
#[derive(Debug)]
struct Parse(String);

impl From<Error> for Parse {
    fn from(error: Error) -> Parse {
        Parse(error.to_string())
    }
}

impl std::fmt::Display for Parse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text of the file a call got.
fn text(mut file: StoredFile) -> String {
    String::from_utf8(file.read_all().unwrap()).unwrap()
}

/// Waits until `blobs` has `runs` runs under way and `waiters` calls waiting.
fn until_under_way(f: &Fixture, runs: usize, waiters: usize) {
    let started = Instant::now();
    while f.blobs().fills.under_way() != (runs, waiters) {
        assert!(started.elapsed() < Duration::from_secs(10), "under way: {:?}", f.blobs().fills.under_way());
        thread::sleep(Duration::from_millis(1));
    }
}

/// A run in a thread of its own whose function says it started and waits for
/// `go`, then gives `answer`; joined, it gives what its call got.
fn held_run(
    f: &Fixture,
    path: &'static str,
    answer: &'static str,
) -> (mpsc::Sender<()>, thread::JoinHandle<Result<String>>) {
    let files = f.files("pages");
    let (started, has_started) = mpsc::channel();
    let (go, wait_for_go) = mpsc::channel::<()>();
    let run = thread::spawn(move || {
        let mut wait_for_go = Some(wait_for_go);
        let file = files.key(path).get_or_create(|| {
            let _ = started.send(());
            if let Some(go) = wait_for_go.take() {
                let _ = go.recv();
            }
            Ok::<_, Error>(answer)
        })?;
        Ok(text(file))
    });
    has_started.recv().unwrap();
    (go, run)
}

/// A call of `path` in a thread of its own whose function, should it run,
/// gives `answer` at once.
fn waiting_call(f: &Fixture, path: &'static str, answer: &'static str) -> thread::JoinHandle<Result<String>> {
    let files = f.files("pages");
    thread::spawn(move || files.key(path).get_or_create(|| Ok::<_, Error>(answer)).map(text))
}

#[test]
fn a_miss_runs_the_function_and_writes_its_file_which_the_next_call_reads() {
    let f = fixture();
    let pages = f.files("pages");
    let runs = AtomicUsize::new(0);
    let convert = || {
        runs.fetch_add(1, Ordering::SeqCst);
        Ok::<_, Error>(b"%PDF-1.7".to_vec())
    };
    let page = pages.key("7f3a.pdf").content_type("application/pdf").meta("doc", "42").get_or_create(convert).unwrap();
    assert_eq!((page.info().content_type.as_str(), page.info().meta["doc"].as_str()), ("application/pdf", "42"));
    let again = pages.key("7f3a.pdf").get_or_create(convert).unwrap();
    assert_eq!(again.info(), page.info());
    assert_eq!(text(again), "%PDF-1.7");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the second call read the file");
    assert_eq!(f.rows("filling"), 0, "a run that landed leaves no claim");
    assert_eq!(f.blobs().fills.under_way(), (0, 0));

    pages.put("there.txt", b"put").unwrap();
    let there = pages.key("there.txt").get_or_create(|| -> Result<&str> { panic!("a hit runs nothing") }).unwrap();
    assert_eq!(text(there), "put");
}

#[test]
fn a_function_gives_bytes_a_reader_or_a_file_with_its_own_type_and_meta() {
    let f = fixture();
    let pages = f.files("pages");
    let large = bytes(3 * INLINE + 5);
    let source = f.dir.path().join("source.bin");
    std::fs::write(&source, &large).unwrap();

    let read = pages.key("read.bin").content_type("text/plain");
    let read = read.get_or_create(|| Ok::<_, Error>(Fill::reader(std::io::Cursor::new(large.clone())))).unwrap();
    assert_eq!((read.info().size, read.info().content_type.as_str()), (large.len() as u64, "text/plain"));
    let from_file = pages
        .key("file.bin")
        .content_type("text/plain")
        .meta("from", "call")
        .get_or_create(|| Ok::<_, Error>(Fill::file(&source).content_type("application/pdf").meta("from", "fill")))
        .unwrap();
    assert_eq!(from_file.info().content_type, "application/pdf", "the function's type wins");
    assert_eq!(from_file.info().meta["from"], "fill");
    assert_eq!(from_file.info().etag, read.info().etag);
    let mut whole = pages.get("file.bin").unwrap().unwrap();
    assert_eq!(whole.read_all().unwrap(), large);

    let missing = f.dir.path().join("missing.bin");
    let error = pages.key("missing.bin").get_or_create(|| Ok::<_, Error>(Fill::file(&missing))).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io, "{error}");
    assert!(pages.head("missing.bin").unwrap().is_none());
}

// contract: BLOB-21
#[test]
fn concurrent_calls_of_one_path_run_the_function_once_and_all_get_its_file() {
    let f = fixture();
    let (go, run) = held_run(&f, "a.pdf", "made once");
    let waiting: Vec<_> = (0..8).map(|_| waiting_call(&f, "a.pdf", "made twice")).collect();
    until_under_way(&f, 1, 8);
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "made once");
    for call in waiting {
        assert_eq!(call.join().unwrap().unwrap(), "made once");
    }
    assert_eq!(f.blobs().fills.under_way(), (0, 0));
}

// contract: BLOB-22
#[test]
fn a_run_whose_lease_ended_lands_nothing_and_its_callers_wait_for_the_run_that_holds_the_path() {
    let f = fixture();
    let files = f.files("pages");
    let (go, stale) = {
        // a claim renewed for no time: its lease ends 30 s on, whatever renews
        let (started, has_started) = mpsc::channel();
        let (go, wait_for_go) = mpsc::channel::<()>();
        let files = files.clone();
        let run = thread::spawn(move || {
            let mut wait_for_go = Some(wait_for_go);
            let call = files.key("a.pdf").timeout(Duration::ZERO);
            call.get_or_create(|| {
                let _ = started.send(());
                if let Some(go) = wait_for_go.take() {
                    let _ = go.recv();
                }
                Ok::<_, Error>("stale")
            })
            .map(text)
        });
        has_started.recv().unwrap();
        (go, run)
    };
    let joined = waiting_call(&f, "a.pdf", "the joined call's");
    until_under_way(&f, 1, 1);
    f.blobs().renew_claims().unwrap();
    f.clock.advance(LEASE);
    let (go_b, b) = held_run(&f, "a.pdf", "current");
    until_under_way(&f, 1, 1);
    go.send(()).unwrap();
    // the stale run's result is dropped, and its call now waits for the current run
    until_under_way(&f, 1, 2);
    assert!(files.head("a.pdf").unwrap().is_none(), "the stale run wrote nothing onto the empty path");
    go_b.send(()).unwrap();
    assert_eq!(b.join().unwrap().unwrap(), "current");
    assert_eq!(stale.join().unwrap().unwrap(), "current");
    assert_eq!(joined.join().unwrap().unwrap(), "current");
}

// contract: BLOB-22
#[test]
fn a_put_during_a_run_wins_and_every_call_of_the_run_gets_the_put_file() {
    let f = fixture();
    let files = f.files("pages");
    let (go, run) = held_run(&f, "a.pdf", "made");
    let joined = waiting_call(&f, "a.pdf", "made again");
    until_under_way(&f, 1, 1);
    files.put("a.pdf", b"put").unwrap();
    assert_eq!(joined.join().unwrap().unwrap(), "put", "the put ended the claim, and the call read its file");
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "put", "the run's result was dropped");
    assert_eq!(text(files.get("a.pdf").unwrap().unwrap()), "put");
    assert_eq!(f.rows("filling"), 0);
}

// contract: BLOB-22
#[test]
fn a_delete_during_a_run_ends_its_claim_and_one_of_its_calls_runs_again() {
    let f = fixture();
    let files = f.files("pages");
    let (go, run) = held_run(&f, "a.pdf", "made before the delete");
    let joined = waiting_call(&f, "a.pdf", "made after the delete");
    until_under_way(&f, 1, 1);
    files.delete("a.pdf").unwrap();
    assert_eq!(joined.join().unwrap().unwrap(), "made after the delete", "the waiting call ran it again");
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "made after the delete", "the stale result was dropped");
}

// contract: BLOB-22
#[test]
fn a_clear_of_its_folder_during_a_run_ends_its_claim_as_a_delete_does() {
    let f = fixture();
    let files = f.files("pages");
    let (go, run) = held_run(&f, "lectures/a.pdf", "made before the clear");
    let joined = waiting_call(&f, "lectures/a.pdf", "made after the clear");
    until_under_way(&f, 1, 1);
    files.folder("lectures").clear().unwrap();
    assert_eq!(joined.join().unwrap().unwrap(), "made after the clear", "the waiting call ran it again");
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "made after the clear", "the stale result was dropped");
}

#[test]
fn an_expiry_during_a_run_changes_nothing_since_no_file_is_there_yet() {
    let f = fixture();
    let files = f.files("pages");
    let (go, run) = held_run(&f, "a.pdf", "made");
    let joined = waiting_call(&f, "a.pdf", "made again");
    until_under_way(&f, 1, 1);
    assert!(!files.expire("a.pdf", Duration::ZERO).unwrap());
    crate::blobs::maintain(&f.store).unwrap();
    assert_eq!(f.blobs().fills.under_way(), (1, 1));
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "made");
    assert_eq!(joined.join().unwrap().unwrap(), "made");
}

// contract: BLOB-22
#[test]
fn a_failure_answers_every_call_that_waited_and_the_next_call_runs_again() {
    let f = fixture();
    let files = f.files("pages");
    let (started, has_started) = mpsc::channel();
    let (go, wait_for_go) = mpsc::channel::<()>();
    let failing = {
        let files = files.clone();
        thread::spawn(move || {
            let mut wait_for_go = Some(wait_for_go);
            files.key("bad.pdf").get_or_create(|| -> std::result::Result<&str, Parse> {
                let _ = started.send(());
                if let Some(go) = wait_for_go.take() {
                    let _ = go.recv();
                }
                Err(Parse("the document does not parse".to_owned()))
            })
        })
    };
    has_started.recv().unwrap();
    let joined = waiting_call(&f, "bad.pdf", "never");
    let second = {
        let files = files.clone();
        thread::spawn(move || files.key("bad.pdf").get_or_create(|| Ok::<_, Error>("never")).map(text))
    };
    until_under_way(&f, 1, 2);
    go.send(()).unwrap();
    let own = failing.join().unwrap().unwrap_err();
    assert_eq!(own.0, "the document does not parse", "the runner's own error");
    for call in [joined, second] {
        let error = call.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
        assert!(error.to_string().contains("the document does not parse"), "{error}");
        assert!(error.to_string().contains("files pages: \"bad.pdf\""), "{error}");
    }
    let again = files.key("bad.pdf").get_or_create(|| Ok::<_, Error>("parsed")).unwrap();
    assert_eq!(text(again), "parsed", "the next call ran it again");
}

#[test]
fn a_store_error_of_the_function_or_of_its_write_is_every_waiting_call_s_as_it_is() {
    let f = fixture();
    let files = f.store.files("pages").max_file_size(4).open().unwrap();
    let (started, has_started) = mpsc::channel();
    let (go, wait_for_go) = mpsc::channel::<()>();
    let large = {
        let files = files.clone();
        thread::spawn(move || {
            let mut wait_for_go = Some(wait_for_go);
            files.key("big.pdf").get_or_create(|| {
                let _ = started.send(());
                if let Some(go) = wait_for_go.take() {
                    let _ = go.recv();
                }
                Ok::<_, Error>("past the bound")
            })
        })
    };
    has_started.recv().unwrap();
    let joined = {
        let files = files.clone();
        thread::spawn(move || files.key("big.pdf").get_or_create(|| Ok::<_, Error>("ok")))
    };
    until_under_way(&f, 1, 1);
    go.send(()).unwrap();
    for call in [large, joined] {
        let error = call.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    }
    assert!(files.head("big.pdf").unwrap().is_none(), "the bytes were dropped");
    assert!(f.uploads().is_empty());

    let refused =
        files.key("found.pdf").get_or_create(|| Err::<&str, _>(Error::not_found("the document"))).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::NotFound, "{refused}");
}

#[test]
fn a_claim_is_renewed_while_its_function_runs_up_to_its_timeout() {
    let f = fixture();
    let (go, run) = held_run(&f, "a.pdf", "slow");
    for _ in 0..4 {
        f.clock.advance(RENEW_EVERY);
        f.blobs().renew_claims().unwrap();
    }
    // 40 s on: past one lease, held by its renewals
    let joined = waiting_call(&f, "a.pdf", "never");
    until_under_way(&f, 1, 1);
    go.send(()).unwrap();
    assert_eq!(run.join().unwrap().unwrap(), "slow");
    assert_eq!(joined.join().unwrap().unwrap(), "slow");

    let files = f.files("pages");
    let (started, has_started) = mpsc::channel();
    let (go, wait_for_go) = mpsc::channel::<()>();
    let hung = thread::spawn(move || {
        let mut wait_for_go = Some(wait_for_go);
        files.key("b.pdf").timeout(Duration::from_secs(15)).get_or_create(|| {
            let _ = started.send(());
            if let Some(go) = wait_for_go.take() {
                let _ = go.recv();
            }
            Ok::<_, Error>("hung")
        })
    });
    has_started.recv().unwrap();
    f.clock.advance(Duration::from_secs(20));
    f.blobs().renew_claims().unwrap();
    f.clock.advance(Duration::from_secs(20));
    f.blobs().renew_claims().unwrap();
    assert_eq!(f.rows("filling"), 0, "a lease that ended goes");
    let next = f.files("pages").key("b.pdf").get_or_create(|| Ok::<_, Error>("next")).unwrap();
    assert_eq!(text(next), "next", "past its timeout the claim lapsed, and the next call ran its own");
    go.send(()).unwrap();
    assert_eq!(text(hung.join().unwrap().unwrap()), "next", "only the run holding the claim landed");
}

#[test]
fn a_claim_a_process_that_died_left_is_taken_over_and_an_open_forgets_every_claim() {
    let f = fixture();
    let files = f.files("pages");
    let set = files.set;
    let now = f.blobs().now();
    let left = move |tx: &Tx<'_>| {
        tx.execute(UPSERT_CLAIM, params![set, "a.pdf", 42_i64, now + 30_000])
            .map(drop)
            .map_err(|error| sql_error("", error))
    };
    f.blobs().file.write(0, left).unwrap();
    let page = files.key("a.pdf").get_or_create(|| Ok::<_, Error>("taken over")).unwrap();
    assert_eq!(text(page), "taken over", "no run of this process held it");

    f.blobs()
        .file
        .write(0, move |tx: &Tx<'_>| {
            tx.execute(UPSERT_CLAIM, params![set, "b.pdf", 42_i64, now + 30_000])
                .map(drop)
                .map_err(|error| sql_error("", error))
        })
        .unwrap();
    assert_eq!(f.rows("filling"), 1);
    let f = f.reopen();
    assert_eq!(f.rows("filling"), 0, "the open forgot what a process that died left");
}

#[test]
fn a_call_of_the_path_inside_its_own_run_is_invalid() {
    let f = fixture();
    let pages = f.files("pages");
    let inner = pages.clone();
    let error = pages
        .key("a.pdf")
        .get_or_create(|| inner.key("a.pdf").get_or_create(|| Ok::<_, Error>("inner")).map(|_| "outer"))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert_eq!(f.blobs().fills.under_way(), (0, 0));
    let other = pages.key("b.pdf").get_or_create(|| {
        inner.key("c.pdf").get_or_create(|| Ok::<_, Error>("c"))?;
        Ok::<_, Error>("b")
    });
    assert_eq!(text(other.unwrap()), "b", "another path inside a run is a call of its own");
}

#[cfg(feature = "sql")]
#[test]
fn a_database_s_files_make_a_miss_once_and_refuse_it_inside_a_transaction() {
    let f = fixture();
    let db = f.store.database("cache").open().unwrap();
    let pages = db.files("pages").open().unwrap();
    let page = pages.key("a.pdf").get_or_create(|| Ok::<_, Error>("in the database")).unwrap();
    assert_eq!(text(page), "in the database");
    let error = db
        .tx(|tx| -> Result<()> { tx.with(&pages).key("b.pdf").get_or_create(|| Ok::<_, Error>("inside")).map(drop) })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[cfg(feature = "metrics")]
#[test]
fn every_call_counts_its_outcome_under_its_set() {
    use crate::metrics::SelfMetrics;
    let dir = tempfile::tempdir().unwrap();
    let options =
        crate::Options { self_metrics: Some(SelfMetrics::default()), background: false, ..Default::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let pages = store.files("pages").open().unwrap();
    pages.key("a").get_or_create(|| Ok::<_, Error>("a")).unwrap();
    pages.key("a").get_or_create(|| Ok::<_, Error>("a")).unwrap();
    pages.key("a").get_or_create(|| Ok::<_, Error>("a")).unwrap();
    pages.key("b").get_or_create(|| Err::<&str, _>(Error::not_found("b"))).unwrap_err();
    let metrics = store.metrics().unwrap();
    metrics.flush().unwrap();
    let counted = |outcome: &str| {
        let query = metrics.query().name("tinystore_files_get_or_create_total").filter("set", "pages");
        let latest = query.filter("outcome", outcome).since(Duration::from_secs(3600)).latest().unwrap();
        latest.first().map(|latest| latest.value)
    };
    assert_eq!((counted("hit"), counted("run"), counted("failed")), (Some(2.0), Some(1.0), Some(1.0)));
    store.close().unwrap();
}

#[test]
fn a_result_a_refusing_ceiling_cannot_take_fails_limit_and_lands_nothing() {
    let f = fixture();
    let quota = f.store.files("uploads").max_size(100).when_full(crate::blobs::WhenFull::Refuse).open().unwrap();
    quota.put("a", bytes(80)).unwrap();
    let error = quota.key("b").get_or_create(|| Ok::<_, crate::Error>(bytes(50))).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
    assert!(quota.get("b").unwrap().is_none());
    assert_eq!(quota.usage().unwrap().size, 80);
}
