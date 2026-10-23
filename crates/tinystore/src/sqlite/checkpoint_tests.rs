use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;

use super::*;
use crate::Result;
use crate::sqlite::{Durability, File, sql_error, wal_end};

/// A file of pages `page_size` long, its rows `page_size - 100` bytes, so
/// that a row is a page.
fn open(page_size: u32) -> (tempfile::TempDir, Arc<File>) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config { durability: Durability::Os, page_size, ..Config::default() };
    let file = File::open(dir.path().join("test.db"), config).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table t (id integer primary key, v blob not null)")
            .map_err(|error| sql_error("create t", error))
    })
    .unwrap();
    (dir, Arc::new(file))
}

/// One grouped write of `pages` rows, each about a page long, so that the
/// commit leaves the WAL that many frames longer and a few more.
fn fill(file: &File, pages: u32) -> Result<()> {
    file.write(0, move |tx| {
        let row = tx
            .query_row("pragma page_size", [], |row| row.get::<_, i64>(0))
            .map_err(|error| sql_error("page size", error))?
            - 100;
        for _ in 0..pages {
            tx.execute("insert into t (v) values (zeroblob(?1))", [row]).map_err(|error| sql_error("fill", error))?;
        }
        Ok(())
    })
}

/// The frame the WAL's last commit ends at, and its generation, which each
/// start of it over moves.
fn wal(file: &File) -> (u32, u32) {
    let at = wal_end(file.path()).unwrap();
    (at.frame, at.checkpoints)
}

#[test]
fn a_wal_long_enough_is_checkpointed_by_the_call_that_made_it_so_once_answered() {
    let (dir, file) = open(4096);
    fill(&file, SOFT / 2).unwrap();
    assert!(wal(&file).0 < SOFT);
    let generation = wal(&file).1;
    fill(&file, SOFT / 2 + 10).unwrap();
    assert!(!file.checkpoints().due.load(Ordering::SeqCst), "the call ran the checkpoint its commit asked for");

    fill(&file, 1).unwrap();
    let (frame, started_over) = wal(&file);
    assert_ne!(started_over, generation, "it copied the whole WAL: the next write starts it over");
    assert!(frame < 10, "a WAL {frame} frames long");

    file.close().unwrap();
    drop(file);
    // the connection the checkpoint ran on holds the file no longer, which Windows would refuse
    std::fs::remove_dir_all(dir.path()).unwrap();
}

#[test]
fn a_checkpoint_off_the_writers_turn_runs_while_a_commit_holds_it() {
    let (_dir, file) = open(4096);
    fill(&file, SOFT / 2).unwrap();
    let generation = wal(&file).1;
    let held = file.hold_writer();
    file.checkpoints().due.store(true, Ordering::SeqCst);
    let checkpointing = {
        let file = Arc::clone(&file);
        thread::spawn(move || file.checkpoints().run_due())
    };
    checkpointing.join().unwrap();
    drop(held);
    fill(&file, 1).unwrap();
    assert_ne!(wal(&file).1, generation, "it copied the whole WAL, the writer held: the next write starts it over");
}

#[test]
fn writes_that_no_checkpoint_catches_up_with_leave_the_wal_no_longer_than_its_bound() {
    // small pages, so that the bound's frames are a few megabytes
    let (_dir, file) = open(512);
    // as a checkpoint that never ends would: no call runs one off the writer's turn
    let running = file.checkpoints().connection.lock().unwrap();
    let (mut longest, generation) = (0, wal(&file).1);
    for _ in 0..(HARD / 1000 + 4) {
        fill(&file, 1000).unwrap();
        longest = longest.max(wal(&file).0);
    }
    drop(running);
    assert!(longest <= HARD + 1100, "a WAL {longest} frames long, past {HARD} and one write");
    assert_ne!(wal(&file).1, generation, "the writer checkpointed it, and the next write started it over");
}

#[test]
fn a_copys_checkpoints_never_run() {
    let (dir, file) = open(4096);
    fill(&file, 10).unwrap();
    file.close().unwrap();
    let copy = File::open_copy(dir.path().join("test.db"), Config::default()).unwrap();
    copy.checkpoints().due.store(true, Ordering::SeqCst);
    copy.checkpoints().run_due();
    assert!(copy.checkpoints().connection.lock().unwrap().is_none(), "no connection opened to run one");
}
