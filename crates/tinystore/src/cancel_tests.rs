//! A token's reads on a file's readers: each stops where it is, gives its
//! reader back and fails `cancelled`, and no other read stops with it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::CancellationToken;
use crate::sqlite::{Config, File, sql_error};
use crate::{ErrorKind, Result};

/// Counts to a billion, which takes minutes: a read only a cancel ends.
const SLOW: &str = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000)
    select count(*) from c";
/// Counts to five thousand, a millisecond or two: long enough that a stray
/// interrupt finds it running.
const SHORT: &str = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 5000)
    select count(*) from c";

fn open(config: Config) -> (tempfile::TempDir, Arc<File>) {
    let dir = tempfile::tempdir().unwrap();
    let file = File::open(dir.path().join("test.db"), config).unwrap();
    (dir, Arc::new(file))
}

fn count(file: &File, sql: &str) -> Result<i64> {
    file.read(|connection| connection.query_row(sql, [], |row| row.get(0)).map_err(|error| sql_error("count", error)))
}

/// Starts `read` on a thread of its own under `token`; its answer comes on the
/// channel, so that a read that never stops fails the test rather than hang it.
fn reading(
    file: &Arc<File>,
    token: &CancellationToken,
    read: impl FnOnce(&File) -> Result<i64> + Send + 'static,
) -> mpsc::Receiver<Result<i64>> {
    let (answer, answered) = mpsc::channel();
    let (file, token) = (Arc::clone(file), token.clone());
    thread::spawn(move || answer.send(token.run(|| read(&file))));
    answered
}

fn within<T>(answered: &mpsc::Receiver<T>, at_most: Duration) -> T {
    answered.recv_timeout(at_most).expect("the read stopped in time")
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "the condition never held");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_cancelled_read_stops_its_statement_and_gives_its_reader_back() {
    // contract: STORE-09
    let config = Config { readers: 1, reader_patience: Duration::from_millis(500), ..Config::default() };
    let (_dir, file) = open(config);
    let token = CancellationToken::new();
    let answered = reading(&file, &token, |file| count(file, SLOW));
    thread::sleep(Duration::from_millis(50));
    let cancelled = Instant::now();
    token.cancel();
    let error = within(&answered, Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
    assert!(error.to_string().ends_with("test.db: a read: cancelled"), "{error}");
    assert!(
        cancelled.elapsed() < Duration::from_secs(1),
        "the read stopped {:?} after its cancel",
        cancelled.elapsed()
    );
    // the one reader is back: a read now takes it inside its patience
    assert_eq!(count(&file, "select 1").unwrap(), 1);
    assert_eq!(file.readers_open(), 1);
}

#[test]
fn a_read_past_its_timeout_stops() {
    // contract: STORE-09
    let (_dir, file) = open(Config::default());
    let token = CancellationToken::timeout(Duration::from_millis(100));
    let error = within(&reading(&file, &token, |file| count(file, SLOW)), Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
    assert!(error.to_string().ends_with("a read past its timeout of 100ms: cancelled"), "{error}");
    assert!(token.is_cancelled());
}

#[test]
fn a_read_waiting_for_a_reader_stops_waiting() {
    // contract: STORE-09
    let config = Config { readers: 1, reader_patience: Duration::from_secs(30), ..Config::default() };
    let (_dir, file) = open(config);
    let (hold, holding) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let holder = {
        let (file, hold, holding) = (Arc::clone(&file), Arc::clone(&hold), Arc::clone(&holding));
        thread::spawn(move || {
            file.read(|_| {
                holding.wait();
                hold.wait();
                Ok(())
            })
        })
    };
    holding.wait();
    let token = CancellationToken::new();
    let answered = reading(&file, &token, |file| count(file, "select 1"));
    wait_until(|| file.readers_waiting() == 1);
    token.cancel();
    let error = within(&answered, Duration::from_secs(5)).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
    assert_eq!(file.readers_waiting(), 0, "the read left the queue");
    hold.wait();
    holder.join().unwrap().unwrap();
    assert_eq!(count(&file, "select 1").unwrap(), 1);
}

#[test]
fn a_read_cancelled_before_it_starts_runs_nothing() {
    // contract: STORE-09
    let (_dir, file) = open(Config::default());
    let token = CancellationToken::new();
    token.cancel();
    let ran = token.run(|| file.read(|_| -> Result<()> { panic!("a cancelled read ran") }));
    assert_eq!(ran.unwrap_err().kind(), ErrorKind::Cancelled);
    assert_eq!(file.readers_open(), 0, "it took no reader");
}

#[test]
fn a_read_stops_when_any_token_around_it_does() {
    let (_dir, file) = open(Config::default());
    let (outer, inner) = (CancellationToken::new(), CancellationToken::timeout(Duration::from_secs(60)));
    let answered = reading(&file, &outer, {
        let inner = inner.clone();
        move |file| inner.run(|| count(file, SLOW))
    });
    thread::sleep(Duration::from_millis(50));
    outer.cancel();
    assert_eq!(within(&answered, Duration::from_secs(5)).unwrap_err().kind(), ErrorKind::Cancelled);
    assert!(!inner.is_cancelled());
}

#[test]
fn a_cancel_stops_no_read_but_its_own() {
    // contract: STORE-09
    // One reader for two threads. Half the cancels of one thread's reads come
    // while the read waits or runs, half once it ended, as the other thread's
    // read takes the reader: an interrupt that outlived its read stops that one.
    let config = Config { readers: 1, reader_patience: Duration::from_secs(30), ..Config::default() };
    let (_dir, file) = open(config);
    let (tokens, cancelling) = mpsc::channel::<CancellationToken>();
    let canceller = thread::spawn(move || cancelling.into_iter().for_each(|token| token.cancel()));
    let cancelled = Arc::new(AtomicUsize::new(0));
    let own = {
        let (file, cancelled) = (Arc::clone(&file), Arc::clone(&cancelled));
        thread::spawn(move || {
            for read in 0..500 {
                let token = CancellationToken::new();
                let after = read % 2 == 1;
                if !after {
                    tokens.send(token.clone()).unwrap();
                }
                let answered = token.run(|| count(&file, SHORT));
                if after {
                    tokens.send(token.clone()).unwrap();
                }
                match answered {
                    Ok(counted) => assert_eq!(counted, 5000),
                    Err(error) => {
                        assert_eq!(error.kind(), ErrorKind::Cancelled, "{error}");
                        cancelled.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        })
    };
    let others = {
        let file = Arc::clone(&file);
        thread::spawn(move || {
            for _ in 0..500 {
                assert_eq!(count(&file, SHORT).unwrap(), 5000, "a read without a token answers whole");
            }
        })
    };
    own.join().unwrap();
    others.join().unwrap();
    canceller.join().unwrap();
    assert!(cancelled.load(Ordering::Relaxed) > 0, "some of the reads were cancelled");
}

#[test]
fn a_progress_handler_stops_only_the_thread_whose_call_was_cancelled() {
    // contract: STORE-09
    let (_dir, file) = open(Config { readers: 2, ..Config::default() });
    let (stopped, kept) = (CancellationToken::new(), CancellationToken::timeout(Duration::from_secs(60)));
    let first = reading(&file, &stopped, |file| count(file, SLOW));
    let second = reading(&file, &kept, |file| count(file, SLOW));
    wait_until(|| file.readers_open() == 2);
    thread::sleep(Duration::from_millis(50));
    stopped.cancel();
    assert_eq!(within(&first, Duration::from_secs(5)).unwrap_err().kind(), ErrorKind::Cancelled);
    assert!(second.recv_timeout(Duration::from_millis(200)).is_err(), "the other read goes on");
    kept.cancel();
    assert_eq!(within(&second, Duration::from_secs(5)).unwrap_err().kind(), ErrorKind::Cancelled);
}

#[test]
fn a_cancel_interrupts_a_statement_between_the_progress_handlers_looks() {
    // contract: STORE-09
    // The read takes its reader's progress handler away, as a statement that
    // SQLite runs in one step never calls it: only the interrupt stops it.
    let (_dir, file) = open(Config { readers: 1, ..Config::default() });
    let token = CancellationToken::new();
    let (started, starting) = mpsc::channel();
    let answered = reading(&file, &token, move |file| {
        file.read(|connection| {
            connection.progress_handler(0, None::<fn() -> bool>).map_err(|error| sql_error("no handler", error))?;
            started.send(()).unwrap();
            connection.query_row(SLOW, [], |row| row.get(0)).map_err(|error| sql_error("count", error))
        })
    });
    // an interrupt before the statement starts is cleared as it starts
    starting.recv().unwrap();
    thread::sleep(Duration::from_millis(100));
    token.cancel();
    assert_eq!(within(&answered, Duration::from_secs(5)).unwrap_err().kind(), ErrorKind::Cancelled);
}

/// What a reader's progress handler costs a long read, in turns: without the
/// handler, with it and no token, under a token, under a token's timeout.
///
/// `cargo test --release -p tinystore --lib cancel::tests::what_the_progress_handler_costs -- --ignored --nocapture`
#[test]
#[ignore = "a timing, run by hand"]
fn what_the_progress_handler_costs() {
    let (_dir, file) = open(Config { readers: 1, ..Config::default() });
    file.transaction(|tx| {
        tx.execute_batch(
            "create table t (id integer primary key, v text not null);
             insert into t (v) select printf('%08d', value) from
                 (with recursive c(value) as (select 1 union all select value + 1 from c where value < 500000)
                  select value from c)",
        )
        .map_err(|error| sql_error("fill t", error))
    })
    .unwrap();
    let reads = [
        (
            "a count to 2M",
            "with recursive c(x) as (select 1 union all select x + 1 from c where x < 2000000) select count(*) from c",
        ),
        ("a scan of 500k rows", "select sum(length(v)) from t where v like '%7%'"),
    ];
    for (named, sql) in reads {
        let without = |file: &File| {
            file.read(|connection| {
                let handler = |on: bool| {
                    let installed = if on {
                        connection.progress_handler(super::STEPS, Some(super::stopped))
                    } else {
                        connection.progress_handler(0, None::<fn() -> bool>)
                    };
                    installed.map_err(|error| sql_error("handler", error))
                };
                handler(false)?;
                let counted = connection.query_row(sql, [], |row| row.get::<_, i64>(0));
                handler(true)?;
                counted.map_err(|error| sql_error("count", error))
            })
        };
        let token = CancellationToken::new();
        let timeout = CancellationToken::timeout(Duration::from_secs(3600));
        let ways: [(&str, &dyn Fn() -> Result<i64>); 4] = [
            ("no handler", &|| without(&file)),
            ("handler, no token", &|| count(&file, sql)),
            ("handler, a token", &|| token.run(|| count(&file, sql))),
            ("handler, a timeout", &|| timeout.run(|| count(&file, sql))),
        ];
        let mut took: Vec<Vec<Duration>> = vec![Vec::new(); ways.len()];
        for _pass in 0..11 {
            for (at, (_, read)) in ways.iter().enumerate() {
                let started = Instant::now();
                read().unwrap();
                took[at].push(started.elapsed());
            }
        }
        for (at, (way, _)) in ways.iter().enumerate() {
            took[at].sort();
            let (median, low, high) = (took[at][5], took[at][0], took[at][10]);
            println!("{named}: {way}: median {median:?}, from {low:?} to {high:?} over 11 passes");
        }
    }
}
