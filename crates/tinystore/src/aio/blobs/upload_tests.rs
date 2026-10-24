use std::pin::pin;
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use crate::ErrorKind;
use crate::aio::Store;
use crate::aio::blobs::Files;
use crate::blobs::fixture::{Fixture, bytes, fixture};

fn awaited(f: &Fixture) -> Files {
    Store::from(f.store.clone()).files("reports").open().unwrap()
}

/// Waits until `holds`, ten seconds at most: what the pool does after a
/// future went shows up a moment after.
fn eventually(what: &str, holds: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !holds() {
        assert!(Instant::now() < deadline, "{what}");
        thread::sleep(Duration::from_millis(1));
    }
}

// contract: BLOB-01
#[tokio::test]
async fn an_upload_appears_whole_at_its_commit_and_never_before() {
    let f = fixture();
    let reports = awaited(&f);
    let body = bytes(3 << 20);
    let mut upload = reports.key("2026-10/report.csv").content_type("text/csv").upload().await.unwrap();
    for piece in body.chunks(100 << 10) {
        upload.write(piece).await.unwrap();
    }
    assert_eq!(reports.head("2026-10/report.csv").await.unwrap(), None);
    assert_eq!(f.uploads().len(), 1);
    let info = upload.commit().await.unwrap();
    assert_eq!((info.size, info.content_type.as_str()), (body.len() as u64, "text/csv"));
    assert_eq!(reports.blocking().head("2026-10/report.csv").unwrap(), Some(info));
    assert_eq!(reports.get("2026-10/report.csv").await.unwrap().unwrap().read_all().await.unwrap(), body);
    assert!(f.uploads().is_empty());
}

#[tokio::test]
async fn a_small_upload_stays_inline_and_writes_in_its_polls() {
    let f = fixture();
    let reports = awaited(&f);
    reports.blocking().put("first", b"takes a block of ids").unwrap();
    let mut upload = reports.upload("a").await.unwrap();
    for piece in bytes(10 << 10).chunks(1000) {
        let mut write = pin!(upload.write(piece));
        assert!(write.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_ready(), "kept in memory");
    }
    assert_eq!(upload.commit().await.unwrap().size, 10 << 10);
    assert!(f.objects().is_empty() && f.uploads().is_empty());
    assert_eq!(f.rows("bodies"), 2);
}

// contract: BLOB-01
#[tokio::test]
async fn an_upload_aborted_or_dropped_leaves_nothing() {
    let f = fixture();
    let reports = awaited(&f);
    let mut aborted = reports.upload("a").await.unwrap();
    aborted.write(&bytes(1 << 20)).await.unwrap();
    aborted.abort();
    {
        let mut dropped = reports.upload("b").await.unwrap();
        dropped.write(&bytes(1 << 20)).await.unwrap();
    }
    let unpolled = reports.upload("c").await.unwrap();
    drop(unpolled.commit());
    eventually("an upload's file outlived it", || f.uploads().is_empty());
    assert!(f.objects().is_empty());
    let heads = (reports.head("a").await.unwrap(), reports.head("b").await.unwrap(), reports.head("c").await.unwrap());
    assert_eq!(heads, (None, None, None));
}

// contract: DUR-11
#[test]
fn a_write_dropped_after_its_poll_still_writes_its_piece_before_the_next() {
    let f = fixture();
    let reports = awaited(&f);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut upload = runtime.block_on(reports.upload("a")).unwrap();
    let (first, second) = (bytes(1 << 20), bytes(64 << 10));
    {
        let mut write = pin!(upload.write(&first));
        assert!(
            write.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending(),
            "a file's piece is the pool's"
        );
    }
    runtime.block_on(upload.write(&second)).unwrap();
    let info = runtime.block_on(upload.commit()).unwrap();
    assert_eq!(info.size, (first.len() + second.len()) as u64);
    let read = runtime.block_on(async { reports.get("a").await.unwrap().unwrap().read_all().await.unwrap() });
    assert_eq!(read, [first, second].concat(), "the pieces landed whole, in the order they were given");
}

// contract: DUR-11
#[test]
fn a_commit_polled_once_commits_though_its_future_is_dropped() {
    let f = fixture();
    let reports = awaited(&f);
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    for (path, size) in [("small", 100), ("large", 1 << 20)] {
        let mut upload = runtime.block_on(reports.upload(path)).unwrap();
        runtime.block_on(upload.write(&bytes(size))).unwrap();
        let mut commit = pin!(upload.commit());
        let _ = commit.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    eventually("a commit dropped after its poll never committed", || {
        let blocking = reports.blocking();
        blocking.head("small").unwrap().is_some() && blocking.head("large").unwrap().is_some()
    });
    assert_eq!(reports.blocking().get("large").unwrap().unwrap().read_all().unwrap(), bytes(1 << 20));
}

#[tokio::test]
async fn an_upload_whose_store_closed_fails_closed_naming_its_file_and_the_call() {
    let f = fixture();
    let store = Store::from(f.store.clone());
    let reports = store.files("reports").open().unwrap();
    let mut upload = reports.folder("2026-10").upload("report.csv").await.unwrap();
    upload.write(&bytes(1 << 20)).await.unwrap();
    store.close().await.unwrap();
    let refused = upload.write(&bytes(1 << 20)).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Closed, "{refused}");
    assert!(refused.to_string().starts_with(r#"files reports: "2026-10/report.csv": a write"#), "{refused}");
    let refused = upload.commit().await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Closed, "{refused}");
    assert!(refused.to_string().starts_with(r#"files reports: "2026-10/report.csv": a commit"#), "{refused}");
}

#[tokio::test]
async fn a_write_borrows_its_bytes_and_a_commit_can_be_spawned() {
    fn sendable<T: Future + Send>(_: &T) {}
    fn spawnable<T: Future + Send + 'static>(_: &T) {}
    let f = fixture();
    let mut upload = awaited(&f).upload("a").await.unwrap();
    sendable(&upload.write(b"a piece"));
    spawnable(&upload.commit());
}

// contract: BLOB-01
#[tokio::test]
async fn an_upload_that_disagrees_with_its_size_leaves_nothing() {
    let f = fixture();
    let reports = awaited(&f);
    let mut short = reports.key("a").size(100).upload().await.unwrap();
    short.write(&bytes(99)).await.unwrap();
    assert_eq!(short.commit().await.unwrap_err().kind(), ErrorKind::Invalid);
    let mut long = reports.key("b").size(20 << 10).upload().await.unwrap();
    let refused = long.write(&bytes(30 << 10)).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
    let ended = long.write(b"more").await.unwrap_err();
    assert_eq!(ended.kind(), ErrorKind::Closed, "an error ends the upload: {ended}");
    drop(long);
    eventually("an upload's file outlived it", || f.uploads().is_empty());
    assert_eq!((reports.head("a").await.unwrap(), reports.head("b").await.unwrap()), (None, None));
}
