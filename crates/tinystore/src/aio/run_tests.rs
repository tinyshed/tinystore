use std::pin::pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use super::*;
use crate::Options;
use crate::sqlite::{Config, File, Tx, sql_error};

/// A store's pool, and a file of one table beside it.
fn pool_and_file(dir: &tempfile::TempDir) -> (crate::Store, Arc<Pool>, Arc<File>) {
    let store = crate::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let pool = Pool::of_or_stopped(&store);
    let file = File::open(dir.path().join("test.db"), Config::default()).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table t (id integer primary key, v text not null)")
            .map_err(|error| sql_error("create t", error))
    })
    .unwrap();
    (store, pool, Arc::new(file))
}

/// Where each part of a write ran: its submit, its commit and its answer.
type Noted = Arc<Mutex<Vec<(&'static str, ThreadId, Option<String>)>>>;

fn note(noted: &Noted, part: &'static str) {
    let here = thread::current();
    noted.lock().unwrap().push((part, here.id(), here.name().map(str::to_owned)));
}

/// An insert of `value` submitted to `file`, which notes where its commit
/// and its answer ran.
fn insert(file: &Arc<File>, value: &str, noted: &Noted) -> impl FnOnce(Answer<i64>) + Send + 'static {
    let (file, value) = (Arc::clone(file), value.to_owned());
    let (committing, answering) = (Arc::clone(noted), Arc::clone(noted));
    move |answer| {
        let write = move |tx: &Tx<'_>| {
            note(&committing, "commit");
            tx.execute("insert into t (v) values (?1)", [&value]).map_err(|error| sql_error("insert", error))?;
            Ok(tx.last_insert_rowid())
        };
        file.submit(1, write, move |id| {
            note(&answering, "answer");
            answer.send(id);
        });
    }
}

fn values(file: &File) -> Vec<String> {
    file.read(|connection| {
        let mut select =
            connection.prepare("select v from t order by id").map_err(|error| sql_error("select", error))?;
        let rows = select.query_map([], |row| row.get(0)).map_err(|error| sql_error("select", error))?;
        rows.collect::<rusqlite::Result<Vec<String>>>().map_err(|error| sql_error("select", error))
    })
    .unwrap()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap()
}

/// A statement that counts for ever: only an interrupt ends it.
const FOREVER: &str = "with recursive n(x) as (select 1 union all select x + 1 from n) select count(*) from n";

/// A read of `file` that says when its count starts, handing over what
/// interrupts it, and how it ended: the count that never ends.
fn counting(
    file: &Arc<File>,
    started: std::sync::mpsc::Sender<Interrupt>,
    ended: std::sync::mpsc::Sender<crate::ErrorKind>,
) -> impl FnOnce() -> Result<i64> + Send + 'static {
    let file = Arc::clone(file);
    move || {
        let counted = file.read(|connection| {
            started.send(Interrupt(connection.get_interrupt_handle())).unwrap();
            connection.query_row(FOREVER, [], |row| row.get(0)).map_err(|error| sql_error("a count", error))
        });
        if let Err(error) = &counted {
            ended.send(error.kind()).unwrap();
        }
        counted
    }
}

/// Interrupts a count as its test ends, its asserts failed or not: a count a
/// cancel missed would run on, and the store's close would wait for it. One
/// that lands after the count ended is lost harmlessly, as cancel.rs says.
struct Interrupt(rusqlite::InterruptHandle);

impl Drop for Interrupt {
    fn drop(&mut self) {
        self.0.interrupt();
    }
}

/// A file whose one reader a test can tell is in use or back.
fn pool_and_file_of_one_reader(dir: &tempfile::TempDir) -> (crate::Store, Arc<Pool>, Arc<File>) {
    let store = crate::Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let pool = Pool::of_or_stopped(&store);
    let config = Config { readers: 1, reader_patience: Duration::from_secs(30), ..Config::default() };
    let file = File::open(dir.path().join("test.db"), config).unwrap();
    file.transaction(|tx| tx.execute_batch("create table t (v text)").map_err(|error| sql_error("create t", error)))
        .unwrap();
    (store, pool, Arc::new(file))
}

/// Says that `file`'s one reader is back: a read waiting for it would wait
/// out its thirty seconds of patience.
fn its_reader_is_back(file: &File) {
    let started = Instant::now();
    let count: i64 = file
        .read(|connection| {
            connection
                .query_row("select count(*) from t", [], |row| row.get(0))
                .map_err(|error| sql_error("count", error))
        })
        .unwrap();
    assert_eq!(count, 0);
    assert!(started.elapsed() < Duration::from_secs(5), "the read waited for the reader");
    assert_eq!(file.readers_open(), 1);
}

// contract: STORE-09
#[test]
fn a_read_whose_future_is_dropped_stops_its_statement_and_gives_its_reader_back() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file_of_one_reader(&dir);
    let ((started, began), (ended, finished)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel());
    let _interrupt = {
        let mut count = pin!(read(&pool, "test.db: a count".to_owned(), counting(&file, started, ended)));
        assert!(count.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        let interrupt = began.recv_timeout(Duration::from_secs(10)).expect("the read started on the pool");
        thread::sleep(Duration::from_millis(20));
        interrupt
    };
    let stopped = finished.recv_timeout(Duration::from_secs(10)).expect("the dropped read never stopped");
    assert_eq!(stopped, crate::ErrorKind::Cancelled);
    its_reader_is_back(&file);
    drop(file);
    store.close().unwrap();
}

// contract: STORE-09
#[test]
fn a_read_a_runtimes_timeout_drops_stops_as_a_dropped_one_does() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file_of_one_reader(&dir);
    let ((started, began), (ended, finished)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel());
    let count = read(&pool, "test.db: a count".to_owned(), counting(&file, started, ended));
    let timed = runtime().block_on(async { tokio::time::timeout(Duration::from_millis(300), count).await });
    assert!(timed.is_err(), "the count never ends: its timeout drops it");
    // a read the pool took after its future went never starts, which is a stop as well
    if let Ok(_interrupt) = began.try_recv() {
        let stopped = finished.recv_timeout(Duration::from_secs(10)).expect("the timed out read never stopped");
        assert_eq!(stopped, crate::ErrorKind::Cancelled);
    }
    its_reader_is_back(&file);
    drop(file);
    store.close().unwrap();
}

// contract: STORE-09
#[test]
fn a_token_around_the_call_that_made_the_future_stops_its_read_on_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file_of_one_reader(&dir);
    let ((started, began), (ended, _finished)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel());
    let token = CancellationToken::new();
    let count = token.run(|| read(&pool, "test.db: a count".to_owned(), counting(&file, started, ended)));
    let cancelling = {
        let token = token.clone();
        thread::spawn(move || {
            let interrupt = began.recv_timeout(Duration::from_secs(10)).unwrap();
            token.cancel();
            // kept a while: a cancel that reached nothing leaves the count to this interrupt
            thread::sleep(Duration::from_secs(3));
            drop(interrupt);
        })
    };
    let refused = runtime().block_on(count).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Cancelled, "{refused}");
    assert!(!cancelling.is_finished(), "the token stopped the count, not the interrupt after it");
    cancelling.join().unwrap();
    its_reader_is_back(&file);
    drop(file);
    store.close().unwrap();
}

/// A point read of `file`'s values, which notes where it ran.
fn point_read(file: &Arc<File>, noted: &Noted) -> impl Fn() -> Result<Vec<String>> + Send + 'static {
    let (file, noted) = (Arc::clone(file), Arc::clone(noted));
    move || {
        note(&noted, "read");
        file.read(|connection| {
            let mut select = connection.prepare("select v from t").map_err(|error| sql_error("select", error))?;
            let rows = select.query_map([], |row| row.get(0)).map_err(|error| sql_error("select", error))?;
            rows.collect::<rusqlite::Result<Vec<String>>>().map_err(|error| sql_error("select", error))
        })
    }
}

// contract: STORE-10
#[test]
fn a_point_read_answers_in_its_first_poll_where_a_reader_is_idle() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    values(&file); // a read leaves its reader idle
    let noted = Noted::default();
    let mut read = pin!(point(&pool, "test.db: a read".to_owned(), point_read(&file, &noted)));
    let polled = read.as_mut().poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(polled, Poll::Ready(Ok(ref values)) if values.is_empty()), "{polled:?}");
    let noted = noted.lock().unwrap().clone();
    assert_eq!(noted.len(), 1);
    assert_eq!(noted[0].1, thread::current().id(), "it ran in the poll");
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_point_read_with_no_reader_idle_waits_for_none_and_runs_on_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    // no read opened a reader yet: opening one is the pool's
    let noted = Noted::default();
    let mut read = pin!(point(&pool, "test.db: a read".to_owned(), point_read(&file, &noted)));
    assert!(read.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
    assert!(runtime().block_on(read.as_mut()).unwrap().is_empty());
    let noted = noted.lock().unwrap().clone();
    let polling = thread::current().id();
    assert_eq!(noted.iter().map(|(_, ran_on, _)| *ran_on == polling).collect::<Vec<_>>(), [true, false]);
    assert_eq!(noted[1].2.as_deref(), Some(crate::pool::THREAD), "it ran again on the store's pool");
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_point_read_that_would_write_writes_once_and_on_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    values(&file);
    let (writing, noted) = (Arc::clone(&file), Noted::default());
    let noting = Arc::clone(&noted);
    let renewal = move || {
        note(&noting, "write");
        let insert = |tx: &Tx<'_>| {
            tx.execute("insert into t (v) values ('renewed')", []).map_err(|error| sql_error("insert", error))
        };
        writing.write(1, insert)
    };
    assert_eq!(runtime().block_on(point(&pool, "test.db: a renewal".to_owned(), renewal)).unwrap(), 1);
    assert_eq!(values(&file), ["renewed"], "the write refused in the poll wrote nothing there");
    let names: Vec<_> = noted.lock().unwrap().iter().map(|(_, _, name)| name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert_eq!(names[1].as_deref(), Some(crate::pool::THREAD));
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_point_read_that_submits_a_write_submits_it_once_and_on_the_pool() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    values(&file);
    let (writing, noted) = (Arc::clone(&file), Noted::default());
    let noting = Arc::clone(&noted);
    // A submit queued in the poll, before a wait it is refused, would be
    // queued again as the read runs again on the pool.
    let renewal = move || {
        note(&noting, "submit");
        let submitted = |tx: &Tx<'_>| {
            tx.execute("insert into t (v) values ('submitted')", []).map_err(|error| sql_error("insert", error))
        };
        writing.submit(1, submitted, drop);
        let written = |tx: &Tx<'_>| {
            tx.execute("insert into t (v) values ('written')", []).map_err(|error| sql_error("insert", error))
        };
        writing.write(1, written)
    };
    assert_eq!(runtime().block_on(point(&pool, "test.db: a renewal".to_owned(), renewal)).unwrap(), 1);
    assert_eq!(values(&file), ["submitted", "written"], "each write was made once, on the pool");
    let noted = noted.lock().unwrap().clone();
    assert_eq!(noted.len(), 2);
    assert_eq!(noted[1].2.as_deref(), Some(crate::pool::THREAD));
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn a_writes_submit_that_reads_or_waits_fails_internal_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    values(&file); // a reader idle: a submit's read is refused all the same, since it may run long
    let (submitting, memory) = (Arc::clone(&file), Arc::clone(store.memory()));
    let submit = move |answer: Answer<Vec<Option<crate::ErrorKind>>>| {
        let count = |connection: &rusqlite::Connection| {
            connection.query_row("select count(*) from t", [], |row| row.get::<_, i64>(0))
        };
        let read = submitting.read(|connection| count(connection).map_err(|error| sql_error("count", error)));
        let insert = |tx: &Tx<'_>| {
            tx.execute("insert into t (v) values ('waited')", []).map_err(|error| sql_error("insert", error))
        };
        let written = submitting.write(1, insert);
        let transaction = submitting.transaction(insert);
        let reserved = memory.reserve(1, "test.db: a reservation", Duration::from_secs(1));
        let outcomes = [read.map(drop), written.map(drop), transaction.map(drop), reserved.map(drop)];
        answer.send(Ok(outcomes.into_iter().map(|outcome| outcome.err().map(|error| error.kind())).collect()));
    };
    let refused = write(&pool, "test.db: a submit that waits".to_owned(), submit).await.unwrap();
    assert_eq!(refused, [Some(crate::ErrorKind::Internal); 4], "a read, a write, a transaction, a reservation");
    assert!(values(&file).is_empty(), "nothing was written");
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[tokio::test]
async fn an_awaited_writes_commit_runs_on_the_stores_pool_never_on_the_thread_that_polls() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    let noted = Noted::default();
    let polling = thread::current().id();
    let (submit, submitted) = (insert(&file, "a", &noted), Arc::clone(&noted));
    let id = write(&pool, "test.db: an insert".to_owned(), move |answer| {
        note(&submitted, "submit");
        submit(answer);
    })
    .await
    .unwrap();
    assert_eq!(id, 1);
    let noted = noted.lock().unwrap().clone();
    assert_eq!(noted.iter().map(|(part, _, _)| *part).collect::<Vec<_>>(), ["submit", "commit", "answer"]);
    assert_eq!(noted[0].1, polling, "the submit runs in the poll");
    for (part, ran_on, name) in &noted[1..] {
        assert_ne!(*ran_on, polling, "the {part} ran on the thread that polled");
        assert_eq!(name.as_deref(), Some(crate::pool::THREAD), "the {part} ran on the store's pool");
    }
    drop(file);
    store.close().unwrap();
}

// contract: DUR-11
#[test]
fn a_write_polled_once_commits_though_its_future_is_dropped_unawaited() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    // Held on a thread of its own, for a second at most: a poll that led the
    // commit itself would wait for it, then answer, rather than hang the test.
    let (release, held_until) = std::sync::mpsc::channel::<()>();
    let (holding, held) = std::sync::mpsc::channel();
    let holder = {
        let file = Arc::clone(&file);
        thread::spawn(move || {
            let _writer = file.hold_writer();
            holding.send(()).unwrap();
            let _ = held_until.recv_timeout(Duration::from_secs(1));
        })
    };
    held.recv().unwrap();
    {
        let mut written = pin!(write(&pool, "test.db: an insert".to_owned(), insert(&file, "a", &Noted::default())));
        let polled = written.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        assert!(polled.is_pending(), "the writer is held, so its commit cannot have ended");
    }
    release.send(()).unwrap();
    holder.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while values(&file).is_empty() {
        assert!(Instant::now() < deadline, "the write dropped after its poll never committed");
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(values(&file), ["a"]);
    drop(file);
    store.close().unwrap();
}

// contract: DUR-11
#[test]
fn a_write_whose_future_is_never_polled_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    let unpolled = write(&pool, "test.db: an insert".to_owned(), insert(&file, "never", &Noted::default()));
    drop(unpolled);
    let after = write(&pool, "test.db: an insert".to_owned(), insert(&file, "after", &Noted::default()));
    assert_eq!(runtime().block_on(after).unwrap(), 1);
    assert_eq!(values(&file), ["after"], "only the write that was polled is there");
    drop(file);
    store.close().unwrap();
}

// contract: STORE-10
#[test]
fn a_write_whose_pool_stopped_commits_on_a_thread_of_its_own_never_on_the_thread_that_polls() {
    let dir = tempfile::tempdir().unwrap();
    let (store, pool, file) = pool_and_file(&dir);
    store.close().unwrap();
    // The file is no engine's, so it is open still and only the pool stopped,
    // as between a store's stop of its engines and its close of their files.
    let noted = Noted::default();
    let written = write(&pool, "test.db: an insert".to_owned(), insert(&file, "a", &noted));
    assert_eq!(runtime().block_on(written).unwrap(), 1, "the writes behind a lead the pool refused still commit");
    assert_eq!(values(&file), ["a"]);
    let polling = thread::current().id();
    for (part, ran_on, name) in noted.lock().unwrap().iter() {
        assert_ne!(*ran_on, polling, "the {part} ran on the thread that polled");
        assert_eq!(name.as_deref(), Some(crate::pool::ALONE), "the {part} ran on a thread of its own");
    }
}

// contract: STORE-10
#[tokio::test]
async fn an_awaited_reservation_parks_no_thread_while_it_waits_for_memory() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { background: false, memory: Some(100), ..Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let held = store.memory().try_reserve(100, "a read").unwrap();
    // One thread runs both: a reservation that parked it would never let the
    // task that gives the bytes back run, and would fail its patience.
    let giving = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(held);
    });
    let started = Instant::now();
    let reserved = reserve(store.memory(), 60, "an append".to_owned(), Duration::from_secs(5)).await.unwrap();
    assert_eq!(reserved.bytes(), 60);
    assert!(started.elapsed() < Duration::from_secs(5));
    giving.await.unwrap();
    drop(reserved);
    assert_eq!(store.memory().used(), 0);
    store.close().unwrap();
}

#[tokio::test]
async fn an_awaited_reservation_its_store_closes_under_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options { background: false, memory: Some(100), ..Options::default() };
    let store = crate::Store::open(dir.path(), options).unwrap();
    let _held = store.memory().try_reserve(100, "a read").unwrap();
    let waiting = tokio::spawn(reserve(store.memory(), 60, "an append".to_owned(), Duration::from_secs(30)));
    tokio::task::yield_now().await;
    store.close().unwrap();
    let closed = waiting.await.unwrap().unwrap_err();
    assert_eq!(closed.kind(), crate::ErrorKind::Closed, "{closed}");
}

#[test]
fn a_pending_answer_polled_before_it_is_sent_waits_and_is_ready_after() {
    let (answer, answered) = pending::<u32>("a test's call");
    let mut answered = pin!(answered);
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(answered.as_mut().poll(&mut context), Poll::Pending));
    answer.send(Ok(3));
    assert!(matches!(answered.as_mut().poll(&mut context), Poll::Ready(Ok(3))));
}
