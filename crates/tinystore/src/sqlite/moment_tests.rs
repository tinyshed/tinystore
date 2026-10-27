//! Files copied at one moment of them all: a row of one that follows a row of
//! another is copied with it, whichever is named first, a transaction that
//! holds one writer and waits for another's goes on through the moment, as
//! one that waits for another thread's commit does, and each copy is a file
//! of one piece with no free page.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use super::*;
use crate::sqlite::{File, read_copy};

fn with_a_table(dir: &Path, name: &str, gate: &Arc<Gate>) -> File {
    let config = Config { gate: Some(Arc::clone(gate)), ..Config::default() };
    let file = File::open(dir.join(name), config).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table t (id integer primary key, v blob)").map_err(|error| sql_error("t", error))
    })
    .unwrap();
    file
}

fn insert(file: &File, id: i64) {
    let insert = move |tx: &crate::sqlite::Tx<'_>| {
        tx.execute("insert into t (id) values (?1)", [id]).map(drop).map_err(|error| sql_error("insert", error))
    };
    file.write(8, insert).unwrap();
}

/// The highest id a copy holds, -1 for none.
fn newest(copy: &Path) -> i64 {
    read_copy(copy, |connection| connection.query_row("select coalesce(max(id), -1) from t", [], |row| row.get(0)))
        .unwrap()
}

fn within_a_minute<T: Send + 'static>(scenario: impl FnOnce() -> T + Send + 'static) -> T {
    let (sent, answered) = mpsc::channel();
    thread::spawn(move || sent.send(scenario()));
    answered.recv_timeout(Duration::from_secs(60)).expect("the scenario ended within a minute")
}

#[test]
fn a_row_that_follows_one_of_another_file_is_copied_with_it_whichever_is_named_first() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let (first, then) = (with_a_table(dir.path(), "first.db", &gate), with_a_table(dir.path(), "then.db", &gate));
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            for id in 0.. {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                insert(&first, id);
                insert(&then, id);
            }
        });
        for n in 0..10 {
            thread::sleep(Duration::from_millis(10));
            let into = dir.path().join(format!("copy-{n}"));
            fs::create_dir(&into).unwrap();
            let mut files =
                vec![(first.source().unwrap(), into.join("first.db")), (then.source().unwrap(), into.join("then.db"))];
            if n % 2 == 1 {
                files.reverse();
            }
            copy_at_one_moment(&gate, &files, || ()).unwrap();
            let (first, then) = (newest(&into.join("first.db")), newest(&into.join("then.db")));
            assert!(first == then || first == then + 1, "copy {n}: first.db up to {first}, then.db up to {then}");
        }
        stop.store(true, Ordering::Relaxed);
    });
}

#[test]
fn a_transaction_that_waits_for_another_files_writer_goes_on_through_the_moment() {
    within_a_minute(|| {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new(Gate::default());
        let held = Arc::new(with_a_table(dir.path(), "held.db", &gate));
        let wanted = Arc::new(with_a_table(dir.path(), "wanted.db", &gate));
        let (began, beginning) = mpsc::channel();
        let transaction = {
            let (held, wanted) = (Arc::clone(&held), Arc::clone(&wanted));
            thread::spawn(move || {
                held.transaction(|tx| {
                    tx.execute("insert into t (id) values (1)", []).map_err(|error| sql_error("insert", error))?;
                    began.send(()).unwrap();
                    // the moment shuts the gate and tries both writers meanwhile
                    thread::sleep(Duration::from_millis(100));
                    insert(&wanted, 1);
                    Ok::<_, crate::Error>(())
                })
            })
        };
        beginning.recv().unwrap();
        let into = dir.path().join("copy");
        fs::create_dir(&into).unwrap();
        // the wanted file first, so that the moment holds it while it waits
        let files =
            [(wanted.source().unwrap(), into.join("wanted.db")), (held.source().unwrap(), into.join("held.db"))];
        copy_at_one_moment(&gate, &files, || ()).unwrap();
        transaction.join().unwrap().unwrap();
        assert_eq!((newest(&into.join("held.db")), newest(&into.join("wanted.db"))), (1, 1), "after the transaction");
    });
}

#[test]
fn a_transaction_that_waits_for_a_commit_the_gate_holds_back_goes_on_through_the_moment() {
    within_a_minute(|| {
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new(Gate::default());
        let held = Arc::new(with_a_table(dir.path(), "held.db", &gate));
        let wanted = Arc::new(with_a_table(dir.path(), "wanted.db", &gate));
        let (began, beginning) = mpsc::channel();
        let transaction = {
            let (held, wanted, gate) = (Arc::clone(&held), Arc::clone(&wanted), Arc::clone(&gate));
            thread::spawn(move || {
                held.transaction(|tx| {
                    tx.execute("insert into t (id) values (1)", []).map_err(|error| sql_error("insert", error))?;
                    began.send(()).unwrap();
                    while gate.shut.load(Ordering::Acquire) == 0 {
                        thread::sleep(Duration::from_millis(1));
                    }
                    // as an SDK's transaction awaits a write to another file, which another thread commits
                    thread::spawn(move || insert(&wanted, 1)).join().unwrap();
                    Ok::<_, crate::Error>(())
                })
            })
        };
        beginning.recv().unwrap();
        let into = dir.path().join("copy");
        fs::create_dir(&into).unwrap();
        let files =
            [(held.source().unwrap(), into.join("held.db")), (wanted.source().unwrap(), into.join("wanted.db"))];
        copy_at_one_moment(&gate, &files, || ()).unwrap();
        transaction.join().unwrap().unwrap();
        assert_eq!((newest(&into.join("held.db")), newest(&into.join("wanted.db"))), (1, 1), "after the transaction");
    });
}

#[test]
fn a_copy_is_a_file_of_one_piece_with_no_free_page() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let file = with_a_table(dir.path(), "test.db", &gate);
    file.write(0, |tx| {
        let fill = "with recursive n(i) as (select 1 union all select i + 1 from n where i < 2000) \
                    insert into t (id, v) select i, randomblob(500) from n";
        tx.execute_batch(fill).map_err(|error| sql_error("fill", error))?;
        tx.execute_batch("delete from t where id > 10").map_err(|error| sql_error("empty", error))
    })
    .unwrap();
    let target = dir.path().join("copy.db");
    copy_at_one_moment(&gate, &[(file.source().unwrap(), target.clone())], || ()).unwrap();

    let header = fs::read(&target).unwrap();
    assert_eq!((header[18], header[19]), (1, 1), "a file of one piece, not a WAL's");
    let free: i64 = read_copy(&target, |c| c.query_row("pragma freelist_count", [], |row| row.get(0))).unwrap();
    assert_eq!((free, newest(&target)), (0, 10));
    let left: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert!(!left.iter().any(|name| name.to_string_lossy().starts_with("copy.db.")), "nothing beside it: {left:?}");
}
