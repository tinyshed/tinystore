use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::sqlite::{Config, Durability, GroupLimits, Migration};

/// A call a test makes on the file from a thread of its own.
type Call<T> = Box<dyn FnOnce(&File) -> Result<T> + Send>;

fn open(config: Config) -> (tempfile::TempDir, Arc<File>) {
    let dir = tempfile::tempdir().unwrap();
    let file = File::open(dir.path().join("test.db"), config).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table t (id integer primary key, v text not null unique)")
            .map_err(|error| sql_error("create t", error))
    })
    .unwrap();
    (dir, Arc::new(file))
}

fn insert(file: &File, value: &str) -> Result<i64> {
    let value = value.to_owned();
    file.write(value.len(), move |tx| {
        tx.prepare_cached("insert into t (v) values (?1)")
            .and_then(|mut insert| insert.execute([&value]))
            .map_err(|error| sql_error("insert", error))?;
        Ok(tx.last_insert_rowid())
    })
}

fn count(file: &File) -> i64 {
    file.read(|connection| {
        connection.query_row("select count(*) from t", [], |row| row.get(0)).map_err(|error| sql_error("count", error))
    })
    .unwrap()
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "the condition never held");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Queues `writes` behind a held writer, so that all of them meet in the
/// group, then lets the writer go and returns each one's answer.
fn queue_behind_the_writer<T: Send + 'static>(file: &Arc<File>, writes: Vec<Call<T>>) -> Vec<Result<T>> {
    let held = lock(&file.writer);
    let count = writes.len();
    let threads: Vec<_> = writes
        .into_iter()
        .map(|write| {
            let file = Arc::clone(file);
            thread::spawn(move || write(&file))
        })
        .collect();
    wait_until(|| file.group.waiting() == count);
    drop(held);
    threads.into_iter().map(|thread| thread.join().unwrap()).collect()
}

#[test]
fn writes_from_many_threads_share_their_commits() {
    let (_dir, file) = open(Config::default());
    let start = Arc::new(Barrier::new(32));
    let threads: Vec<_> = (0..32)
        .map(|thread| {
            let file = Arc::clone(&file);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                for write in 0..20 {
                    insert(&file, &format!("{thread}-{write}")).unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(count(&file), 640);
    assert!(file.commits() < 640, "{} commits for 640 writes", file.commits());
}

#[test]
fn a_failing_write_rolls_back_alone() {
    let (_dir, file) = open(Config::default());
    insert(&file, "taken").unwrap();
    let before = file.commits();
    let answers = queue_behind_the_writer(
        &file,
        vec![
            Box::new(|file| insert(file, "a")),
            Box::new(|file| insert(file, "taken")),
            Box::new(|file| insert(file, "b")),
        ],
    );
    assert_eq!(file.commits() - before, 1, "the three writes share one commit");
    assert!(answers[0].is_ok() && answers[2].is_ok());
    assert_eq!(answers[1].as_ref().unwrap_err().kind(), ErrorKind::Conflict);
    assert_eq!(count(&file), 3);
}

#[test]
fn a_panicking_write_fails_alone_and_the_writer_goes_on() {
    let (_dir, file) = open(Config::default());
    let answers = queue_behind_the_writer(
        &file,
        vec![
            Box::new(|file| insert(file, "a")),
            Box::new(|file| file.write(0, |_| -> Result<i64> { panic!("a bug in an engine") })),
        ],
    );
    assert!(answers[0].is_ok());
    assert_eq!(answers[1].as_ref().unwrap_err().kind(), ErrorKind::Internal);
    insert(&file, "b").unwrap();
    assert_eq!(count(&file), 2);
}

#[test]
fn a_write_heavier_than_the_group_commits_alone() {
    let config = Config { group: GroupLimits { writes: 1024, bytes: 100 }, ..Config::default() };
    let (_dir, file) = open(config);
    let before = file.commits();
    let heavy = |value: &'static str| -> Call<i64> {
        Box::new(move |file| {
            let value = value.to_owned();
            file.write(60, move |tx| {
                tx.execute("insert into t (v) values (?1)", [&value]).map_err(|error| sql_error("insert", error))?;
                Ok(tx.last_insert_rowid())
            })
        })
    };
    let answers = queue_behind_the_writer(&file, vec![heavy("a"), heavy("b"), heavy("c")]);
    assert!(answers.iter().all(Result::is_ok));
    assert_eq!(file.commits() - before, 3);
}

#[test]
fn closing_answers_the_queued_writes_and_refuses_new_ones() {
    let (_dir, file) = open(Config::default());
    let held = lock(&file.writer);
    let writers: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|value| {
            let file = Arc::clone(&file);
            thread::spawn(move || insert(&file, value))
        })
        .collect();
    wait_until(|| file.group.waiting() == 3);
    let closer = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.close())
    };
    wait_until(|| file.group.waiting() == 0);
    drop(held);
    for writer in writers {
        assert_eq!(writer.join().unwrap().unwrap_err().kind(), ErrorKind::Closed);
    }
    closer.join().unwrap().unwrap();
    assert_eq!(insert(&file, "d").unwrap_err().kind(), ErrorKind::Closed);
}

#[test]
fn a_transaction_keeps_its_writes_only_when_it_succeeds() {
    let (_dir, file) = open(Config::default());
    let failed = file.transaction(|tx| {
        tx.execute("insert into t (v) values ('a')", []).map_err(|error| sql_error("insert", error))?;
        Err::<(), _>(Error::invalid("changed its mind"))
    });
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Invalid);

    let panicked = catch_unwind(AssertUnwindSafe(|| {
        file.transaction(|tx| -> Result<()> {
            tx.execute("insert into t (v) values ('b')", []).unwrap();
            panic!("a bug in an engine")
        })
    }));
    assert!(panicked.is_err());

    file.transaction(|tx| tx.execute("insert into t (v) values ('c')", []).map_err(|error| sql_error("insert", error)))
        .unwrap();
    assert_eq!(count(&file), 1);
}

#[test]
fn a_read_sees_one_snapshot_while_a_write_commits_beside_it() {
    let (_dir, file) = open(Config::default());
    insert(&file, "a").unwrap();
    let writer = Arc::clone(&file);
    let seen = file
        .read(move |connection| {
            let count = |connection: &rusqlite::Connection| -> Result<i64> {
                connection
                    .query_row("select count(*) from t", [], |row| row.get(0))
                    .map_err(|error| sql_error("count", error))
            };
            let before = count(connection)?;
            thread::spawn(move || insert(&writer, "b")).join().unwrap()?;
            Ok((before, count(connection)?))
        })
        .unwrap();
    assert_eq!(seen, (1, 1));
    assert_eq!(count(&file), 2);
}

#[test]
fn a_reader_cannot_write() {
    let (_dir, file) = open(Config::default());
    let error = file
        .read(|connection| {
            connection.execute("insert into t (v) values ('a')", []).map_err(|error| sql_error("insert", error))
        })
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
}

#[test]
fn idle_readers_close_but_one() {
    let config = Config { reader_idle: Duration::ZERO, ..Config::default() };
    let (_dir, file) = open(config);
    let together = Arc::new(Barrier::new(3));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let file = Arc::clone(&file);
            let together = Arc::clone(&together);
            thread::spawn(move || {
                file.read(move |_| {
                    together.wait();
                    Ok(())
                })
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap().unwrap();
    }
    assert_eq!(file.readers_open(), 3);
    assert_eq!(file.sweep(), 2);
    assert_eq!(file.readers_open(), 1);
    assert_eq!(count(&file), 0);
}

#[test]
fn a_read_waits_for_a_busy_reader_and_then_gives_up() {
    let config = Config { readers: 1, reader_patience: Duration::from_millis(50), ..Config::default() };
    let (_dir, file) = open(config);
    let (reading, release) = (mpsc::channel(), mpsc::channel::<()>());
    let holder = {
        let file = Arc::clone(&file);
        let (started, held) = (reading.0, release.1);
        thread::spawn(move || {
            file.read(move |_| {
                started.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
        })
    };
    reading.1.recv().unwrap();
    let error = file.read(|_| Ok(())).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unavailable, "{error}");
    release.0.send(()).unwrap();
    holder.join().unwrap().unwrap();
    file.read(|_| Ok(())).unwrap();
}

const STEP_ONE: Migration = Migration { version: 1, sql: "create table a (x integer)" };
const STEP_TWO: Migration = Migration { version: 2, sql: "create table b (y integer)" };

#[test]
fn migrations_apply_once_in_order() {
    let (_dir, file) = open(Config::default());
    file.migrate("test", &[STEP_ONE]).unwrap();
    file.migrate("test", &[STEP_ONE]).unwrap();
    file.migrate("test", &[STEP_ONE, STEP_TWO]).unwrap();
    let tables: i64 = file
        .read(|connection| {
            connection
                .query_row("select count(*) from sqlite_schema where name in ('a', 'b')", [], |row| row.get(0))
                .map_err(|error| sql_error("tables", error))
        })
        .unwrap();
    assert_eq!(tables, 2);
}

#[test]
fn a_migration_the_program_does_not_have_or_has_edited_is_refused() {
    let (_dir, file) = open(Config::default());
    file.migrate("test", &[STEP_ONE, STEP_TWO]).unwrap();

    let older = file.migrate("test", &[STEP_ONE]).unwrap_err();
    assert!(older.to_string().contains("a newer program"), "{older}");

    let edited = Migration { version: 1, sql: "create table a (x text)" };
    let error = file.migrate("test", &[edited, STEP_TWO]).unwrap_err();
    assert!(error.to_string().contains("was edited"), "{error}");

    let gap = file.migrate("other", &[STEP_TWO]).unwrap_err();
    assert_eq!(gap.kind(), ErrorKind::Invalid);
}

#[test]
fn durability_sets_how_far_a_commit_goes() {
    for (durability, synchronous) in [(Durability::Full, 2), (Durability::Os, 1)] {
        let config = Config { durability, ..Config::default() };
        let (_dir, file) = open(config);
        let (mode, level) = file
            .transaction(|tx| -> Result<(String, i64)> {
                let mode: String = tx
                    .pragma_query_value(None, "journal_mode", |row| row.get(0))
                    .map_err(|error| sql_error("journal_mode", error))?;
                let level: i64 = tx
                    .pragma_query_value(None, "synchronous", |row| row.get(0))
                    .map_err(|error| sql_error("synchronous", error))?;
                Ok((mode, level))
            })
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(level, synchronous, "{durability:?}");
    }
}

#[test]
fn sqlite_counts_the_memory_it_holds() {
    let (_dir, file) = open(Config::default());
    insert(&file, "a").unwrap();
    assert!(crate::sqlite::memory_used() > 0);
}

#[test]
fn writes_submitted_while_a_commit_runs_return_at_once_and_share_the_next() {
    let (_dir, file) = open(Config::default());
    let before = file.commits();
    let (answers, answered) = mpsc::channel();
    let submit = |file: &File, n: usize, answers: mpsc::Sender<Result<()>>| {
        let value = format!("v{n}");
        let write = move |tx: &Tx<'_>| {
            tx.execute("insert into t (v) values (?1)", [&value]).map_err(|error| sql_error("insert", error))
        };
        file.submit(4, write, move |answer| answers.send(answer.map(|_| ())).unwrap());
    };
    let held = lock(&file.writer);
    let leader = {
        let (file, answers) = (Arc::clone(&file), answers.clone());
        // with no commit running, the first submission leads, and waits for the writer
        thread::spawn(move || submit(&file, 0, answers))
    };
    wait_until(|| file.group.waiting() == 1);
    for n in 1..500 {
        submit(&file, n, answers.clone());
    }
    assert_eq!(file.group.waiting(), 500, "a submission returns at once while a leader runs");
    drop(held);
    leader.join().unwrap();
    for _ in 0..500 {
        answered.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    }
    assert_eq!(count(&file), 500);
    assert!(file.commits() - before <= 2, "{} commits for 500 writes", file.commits() - before);
}
