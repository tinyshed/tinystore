use std::fs;
use std::io;
use std::pin::pin;
use std::sync::Mutex;
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::aio::Store;
use crate::{ErrorKind, Options};

/// A store in `data` of `dir`, a file of the program's in it, and with kv a
/// session written.
fn with_notes(dir: &tempfile::TempDir) -> Store {
    let store = Store::open(dir.path().join("data"), Options { background: false, ..Options::default() }).unwrap();
    fs::write(store.dir().join("notes.txt"), "a program's own file").unwrap();
    #[cfg(feature = "kv")]
    store.blocking().bucket::<String>("sessions").open().unwrap().set("42", &"ada".to_owned()).unwrap();
    store
}

/// Says that a store restored into `dir` holds what `with_notes` wrote.
fn holds_the_notes(dir: &Path) {
    assert_eq!(fs::read_to_string(dir.join("notes.txt")).unwrap(), "a program's own file");
    #[cfg(feature = "kv")]
    {
        let store = crate::Store::open(dir, Options { background: false, ..Options::default() }).unwrap();
        let sessions = store.bucket::<String>("sessions").open().unwrap();
        assert_eq!(sessions.get("42").unwrap().as_deref(), Some("ada"));
        store.close().unwrap();
    }
}

fn wait_until(holds: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !holds() {
        assert!(Instant::now() < deadline, "{what} never came");
        thread::sleep(Duration::from_millis(1));
    }
}

#[tokio::test]
async fn an_awaited_backup_holds_what_the_blocking_one_holds() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_notes(&dir);
    let (awaited_zip, blocking_zip) = (dir.path().join("awaited.zip"), dir.path().join("blocking.zip"));
    let awaited = store.backup().include("notes.txt").write(&awaited_zip).await.unwrap();
    let blocking = store.blocking().backup().include("notes.txt").write(&blocking_zip).unwrap();
    assert_eq!(awaited, blocking);
    store.close().await.unwrap();
    for (zip, into) in [(&awaited_zip, "from-awaited"), (&blocking_zip, "from-blocking")] {
        assert_eq!(crate::backup::restore(zip, dir.path().join(into)).unwrap(), awaited);
        holds_the_notes(&dir.path().join(into));
    }
}

/// What a backup sent to a writer: its bytes, and the names of the threads
/// that wrote them.
#[derive(Clone, Default)]
struct Sent(Arc<Mutex<Received>>);

#[derive(Clone, Default)]
struct Received {
    bytes: Vec<u8>,
    written_on: Vec<Option<String>>,
}

impl Write for Sent {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut sent = self.0.lock().unwrap();
        sent.bytes.extend_from_slice(bytes);
        sent.written_on.push(thread::current().name().map(str::to_owned));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// contract: STORE-10
#[tokio::test]
async fn a_backup_sent_to_a_writer_is_made_on_the_stores_pool_never_on_the_thread_that_polls() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_notes(&dir);
    let sent = Sent::default();
    // spawned, so that the future is Send and 'static, as a program's runtime asks
    let backup = tokio::spawn(store.backup().include("notes.txt").write_to(sent.clone())).await.unwrap().unwrap();
    let Received { bytes, written_on } = sent.0.lock().unwrap().clone();
    assert!(!written_on.is_empty());
    assert!(written_on.iter().all(|name| name.as_deref() == Some(crate::pool::THREAD)), "{written_on:?}");
    store.close().await.unwrap();
    fs::write(dir.path().join("sent.zip"), bytes).unwrap();
    assert_eq!(crate::backup::restore(dir.path().join("sent.zip"), dir.path().join("restored")).unwrap(), backup);
    holds_the_notes(&dir.path().join("restored"));
}

#[test]
fn a_backup_polled_once_runs_to_its_end_though_its_future_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_notes(&dir);
    let (zip, never) = (dir.path().join("dropped.zip"), dir.path().join("never.zip"));
    drop(store.backup().include("notes.txt").write(&never));
    {
        let mut written = pin!(store.backup().include("notes.txt").write(&zip));
        let _ = written.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    }
    wait_until(|| zip.exists(), "the zip of a backup dropped after its first poll");
    assert!(!never.exists(), "a backup never polled wrote nothing");
    store.blocking().close().unwrap();
    crate::backup::restore(&zip, dir.path().join("restored")).unwrap();
    holds_the_notes(&dir.path().join("restored"));
}

// contract: BAK-04
#[tokio::test]
async fn an_awaited_backup_that_fails_leaves_no_zip_and_no_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_notes(&dir);
    let zip = dir.path().join("failed.zip");
    let failed = store.backup().include("missing.txt").write(&zip).await.unwrap_err();
    let twin = store.blocking().backup().include("missing.txt").write(&zip).unwrap_err();
    assert_eq!((failed.kind(), failed.to_string()), (twin.kind(), twin.to_string()));
    let names = |dir: &Path| -> Vec<String> {
        fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect()
    };
    assert!(!names(dir.path()).iter().any(|name| name.starts_with("failed.zip")), "{:?}", names(dir.path()));
    assert!(!names(store.dir()).iter().any(|name| name.starts_with(".snapshot-")), "{:?}", names(store.dir()));
    store.close().await.unwrap();
}

#[tokio::test]
async fn a_backup_of_a_store_that_closed_fails_closed_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_notes(&dir);
    let later = store.clone();
    store.close().await.unwrap();
    let zip = dir.path().join("late.zip");
    let closed = later.backup().write(&zip).await.unwrap_err();
    assert_eq!(closed.kind(), ErrorKind::Closed, "{closed}");
    assert!(!zip.exists());
}
