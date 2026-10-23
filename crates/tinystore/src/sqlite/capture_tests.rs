use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;

use super::*;
use crate::sqlite::{Config, File, Migration, sql_error};

/// An observer that keeps what it is handed in memory, and refuses commits
/// while told to.
#[derive(Default)]
struct Kept {
    last: Mutex<Option<Position>>,
    begun: Mutex<Vec<Position>>,
    commits: Mutex<Vec<Handed>>,
    gaps: Mutex<Vec<Position>>,
    syncs: AtomicUsize,
    refusing: AtomicBool,
    unsynced: AtomicBool,
}

struct Handed {
    position: Position,
    page_size: u32,
    size: u32,
    pages: Vec<(u32, Vec<u8>)>,
}

impl Observe for Kept {
    fn last(&self) -> Option<Position> {
        *self.last.lock().unwrap()
    }

    fn begin(&self, position: Position, _size: u32) -> Result<()> {
        self.begun.lock().unwrap().push(position);
        *self.last.lock().unwrap() = Some(position);
        Ok(())
    }

    fn committed(&self, commit: &Commit<'_>) -> Result<()> {
        if self.refusing.load(Ordering::SeqCst) {
            return Err(Error::io("append", io::Error::other("the disk is full")));
        }
        let pages = commit.pages.iter().map(|(number, page)| (*number, page.to_vec())).collect();
        let handed = Handed { position: commit.position, page_size: commit.page_size, size: commit.size, pages };
        self.commits.lock().unwrap().push(handed);
        *self.last.lock().unwrap() = Some(commit.position);
        Ok(())
    }

    fn gap(&self, position: Position) -> Result<()> {
        self.gaps.lock().unwrap().push(position);
        *self.last.lock().unwrap() = Some(position);
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        if self.unsynced.load(Ordering::SeqCst) {
            return Err(Error::io("sync", io::Error::other("the disk is gone")));
        }
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Kept {
    /// The file the commits handed make, from an empty one.
    fn rebuilt(&self) -> Vec<u8> {
        let mut image = Vec::new();
        for commit in self.commits.lock().unwrap().iter() {
            let page_size = commit.page_size as usize;
            image.resize(commit.size as usize * page_size, 0);
            for (number, page) in &commit.pages {
                let at = (*number as usize - 1) * page_size;
                if at < image.len() {
                    image[at..at + page_size].copy_from_slice(page);
                }
            }
        }
        image
    }

    fn handed(&self) -> usize {
        self.commits.lock().unwrap().len()
    }
}

const TABLE: &[Migration] = &[Migration {
    version: 1,
    name: std::borrow::Cow::Borrowed("0001_t.sql"),
    sql: std::borrow::Cow::Borrowed("create table t (id integer primary key, v text not null unique);"),
}];

fn open(path: &Path, kept: &Arc<Kept>) -> File {
    let observer: Arc<dyn Observe> = Arc::clone(kept) as Arc<dyn Observe>;
    File::open_observed(path, Config::default(), Some(observer)).unwrap()
}

fn insert(file: &File, value: &str) -> Result<()> {
    let value = value.to_owned();
    file.write(value.len(), move |tx| {
        tx.execute("insert into t (v) values (?1)", [&value]).map(drop).map_err(|error| sql_error("insert", error))
    })
}

fn the_file_after_close(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

// contract: REPL-01
#[test]
fn every_commit_of_every_kind_is_handed_once_and_rebuilds_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    for i in 0..50 {
        insert(&file, &format!("grouped {i}")).unwrap();
    }
    let (sender, received) = mpsc::channel();
    for i in 0..20 {
        let sender = sender.clone();
        let value = format!("submitted {i}");
        file.submit(
            value.len(),
            move |tx| {
                tx.execute("insert into t (v) values (?1)", [&value])
                    .map(drop)
                    .map_err(|error| sql_error("insert", error))
            },
            move |done| sender.send(done).unwrap(),
        );
    }
    for _ in 0..20 {
        received.recv().unwrap().unwrap();
    }
    file.transaction(|tx| {
        tx.execute("delete from t where id % 3 = 0", []).map_err(|error| sql_error("delete", error))?;
        tx.execute("update t set v = v || ' again' where id % 3 = 1", []).map_err(|error| sql_error("update", error))
    })
    .unwrap();
    let mark = file.mark().unwrap();
    let begun = file.begin(&mark).unwrap();
    begun.tx().execute("insert into t (v) values ('begun')", []).unwrap();
    begun.commit().unwrap();
    drop(mark);
    file.read(|_| Ok(())).unwrap();
    let before = kept.handed();
    file.write(0, |_| Ok(())).unwrap();
    assert_eq!(kept.handed(), before, "a commit that wrote nothing hands nothing");
    file.close().unwrap();

    assert_eq!(kept.begun.lock().unwrap().len(), 1, "a file kept nowhere before begins once");
    assert!(kept.gaps.lock().unwrap().is_empty());
    let commits = kept.commits.lock().unwrap();
    assert!(
        commits.windows(2).all(|pair| pair[0].position.frame < pair[1].position.frame),
        "in the order they committed"
    );
    drop(commits);
    assert_eq!(kept.rebuilt(), the_file_after_close(&path));
}

// contract: DUR-05, REPL-02
#[test]
fn a_commit_its_observer_refuses_is_outcome_unknown_and_the_next_open_hands_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    insert(&file, "before").unwrap();

    kept.refusing.store(true, Ordering::SeqCst);
    let refused = insert(&file, "unlogged").unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::OutcomeUnknown, "{refused}");
    kept.refusing.store(false, Ordering::SeqCst);
    let after = insert(&file, "after").unwrap_err();
    assert_eq!(after.kind(), ErrorKind::Unavailable, "{after}");
    let transaction = file.transaction(|_| Ok::<(), Error>(())).unwrap_err();
    assert_eq!(transaction.kind(), ErrorKind::Unavailable);
    std::mem::forget(file);

    let reopened = open(&path, &kept);
    let count: i64 = reopened
        .read(|c| c.query_row("select count(*) from t", [], |row| row.get(0)).map_err(|e| sql_error("count", e)))
        .unwrap();
    assert_eq!(count, 2, "the refused commit is in the file");
    insert(&reopened, "again").unwrap();
    reopened.close().unwrap();
    assert_eq!(kept.rebuilt(), the_file_after_close(&path), "the reopened file handed what its observer lacked");
}

// contract: REPL-03
#[test]
fn a_process_that_died_leaves_the_commits_its_observer_lost_to_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    for i in 0..30 {
        insert(&file, &format!("row {i}")).unwrap();
    }
    // The observer loses its last ten commits, as a log does whose unsynced
    // tail a crash took; the process dies without closing the file.
    let lost = {
        let mut commits = kept.commits.lock().unwrap();
        let kept_back = commits.len() - 10;
        let lost: Vec<Handed> = commits.drain(kept_back..).collect();
        *kept.last.lock().unwrap() = Some(commits.last().unwrap().position);
        lost
    };
    let died = tempfile::tempdir().unwrap();
    for name in ["test.db", "test.db-wal"] {
        std::fs::copy(dir.path().join(name), died.path().join(name)).unwrap();
    }
    std::mem::forget(file);

    let path = died.path().join("test.db");
    let reopened = open(&path, &kept);
    let commits = kept.commits.lock().unwrap();
    let handed_again: Vec<Position> = commits[commits.len() - 10..].iter().map(|c| c.position).collect();
    drop(commits);
    assert_eq!(handed_again, lost.iter().map(|c| c.position).collect::<Vec<_>>());
    insert(&reopened, "after").unwrap();
    reopened.close().unwrap();
    assert_eq!(kept.rebuilt(), the_file_after_close(&path));
}

#[test]
fn the_wal_is_checkpointed_only_after_its_commits_are_synced_and_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    let syncs = kept.syncs.load(Ordering::SeqCst);
    for i in 0..3000 {
        insert(&file, &format!("row {i:05} {}", "x".repeat(200))).unwrap();
    }
    assert!(kept.syncs.load(Ordering::SeqCst) > syncs, "a long WAL synced the log before its checkpoint");
    let generations: std::collections::HashSet<_> =
        kept.commits.lock().unwrap().iter().map(|c| (c.position.checkpoints, c.position.salts)).collect();
    assert!(generations.len() > 1, "the WAL started over after a checkpoint");
    let largest = kept.commits.lock().unwrap().iter().map(|c| c.position.frame).max().unwrap();
    assert!(largest < 2 * CHECKPOINT_AT, "the WAL stays near the checkpoint's length: {largest} frames");
    assert!(kept.gaps.lock().unwrap().is_empty(), "the capture's own checkpoints leave no gap");
    file.close().unwrap();
    assert_eq!(kept.rebuilt(), the_file_after_close(&path));
}

#[test]
fn a_wal_started_over_beside_the_store_is_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    insert(&file, "ours").unwrap();
    {
        let beside = rusqlite::Connection::open(&path).unwrap();
        beside.execute("insert into t (v) values ('beside')", []).unwrap();
        beside.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())).unwrap();
        beside.execute("insert into t (v) values ('beside again')", []).unwrap();
        beside.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())).unwrap();
    }
    insert(&file, "ours again").unwrap();
    assert_eq!(kept.gaps.lock().unwrap().len(), 1, "the capture said the WAL started over beside it");
    file.close().unwrap();
}

#[test]
fn a_wal_header_and_frames_read_as_sqlite_recovers_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = open(&path, &kept);
    file.migrate_checked("test", TABLE).unwrap();
    for i in 0..5 {
        insert(&file, &format!("row {i}")).unwrap();
    }
    let bytes = std::fs::read(wal_of(&path)).unwrap();
    let scanned = |bytes: &[u8]| {
        let copy = dir.path().join("scanned-wal");
        std::fs::write(&copy, bytes).unwrap();
        WalCommits::scan(&mut std::fs::File::open(&copy).unwrap()).unwrap()
    };
    let wal = scanned(&bytes).expect("a WAL with commits");
    assert_eq!(wal.end, kept.commits.lock().unwrap().last().unwrap().position.frame);
    let mut torn = bytes.clone();
    let last = torn.len() - 1;
    torn[last] ^= 0xff;
    let recovered = scanned(&torn).expect("the commits before the torn frame");
    assert!(recovered.end < wal.end, "a frame whose checksum breaks ends what is recovered");
    std::mem::forget(file);
}

#[cfg(feature = "replication")]
#[test]
fn a_file_open_becomes_a_copy_and_a_home_again_its_commits_handed_only_as_a_home() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Arc::new(Kept::default());
    let file = File::open_observed(dir.path().join("moving.db"), Config::default(), Some(kept.clone())).unwrap();
    let insert = |file: &File, value: &'static str| {
        file.write(value.len(), move |tx| {
            tx.execute("insert into t (v) values (?1)", [value]).map(drop).map_err(|error| sql_error("insert", error))
        })
    };
    let count = |file: &File| {
        file.read(|connection| {
            connection
                .query_row("select count(*) from t", [], |row| row.get::<_, i64>(0))
                .map_err(|e| sql_error("count", e))
        })
        .unwrap()
    };
    file.transaction(|tx| tx.execute_batch("create table t (v text)").map_err(|error| sql_error("t", error))).unwrap();
    insert(&file, "as a home").unwrap();
    let handed = kept.handed();

    file.become_copy().unwrap();
    assert!(file.is_copy());
    assert!(insert(&file, "as a copy").is_err(), "a copy takes no write");
    assert_eq!((kept.handed(), count(&file)), (handed, 1), "and hands nothing");

    file.become_home(kept.clone()).unwrap();
    assert!(!file.is_copy());
    insert(&file, "a home again").unwrap();
    assert_eq!((kept.handed(), count(&file)), (handed + 1, 2), "a home again hands its commits");
    file.close().unwrap();
}

#[cfg(feature = "replication")]
#[test]
fn a_log_that_fails_to_close_keeps_the_file_a_logged_home() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Arc::new(Kept::default());
    let file = File::open_observed(dir.path().join("stuck.db"), Config::default(), Some(kept.clone())).unwrap();
    let insert = |value: &'static str| {
        file.write(value.len(), move |tx| {
            tx.execute("insert into t (v) values (?1)", [value]).map(drop).map_err(|error| sql_error("insert", error))
        })
    };
    file.transaction(|tx| tx.execute_batch("create table t (v text)").map_err(|error| sql_error("t", error))).unwrap();

    kept.unsynced.store(true, Ordering::SeqCst);
    assert!(file.become_copy().is_err());
    kept.unsynced.store(false, Ordering::SeqCst);

    assert!(!file.is_copy(), "a failed change of home leaves a home");
    let handed = kept.handed();
    insert("still a home").unwrap();
    assert_eq!(kept.handed(), handed + 1, "whose commits are still logged");
    file.close().unwrap();
}

/// Reads on four threads that never stop, each `read` holding its
/// snapshot a moment, until the flag returned is cleared.
fn reading_always(read: impl Fn() + Send + Sync + 'static) -> (Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>) {
    let (reading, read) = (Arc::new(AtomicBool::new(true)), Arc::new(read));
    let threads = (0..4)
        .map(|_| {
            let (reading, read) = (Arc::clone(&reading), Arc::clone(&read));
            std::thread::spawn(move || {
                while reading.load(Ordering::SeqCst) {
                    read();
                }
            })
        })
        .collect();
    (reading, threads)
}

/// A read of `t` whose first statement takes its snapshot, held longer than
/// a commit takes to reach its log, so that one read or another always holds
/// a snapshot of before the last commit when its checkpoint runs.
fn a_moments_snapshot(connection: &Connection) -> Result<()> {
    let one = "select count(*) from t where id = 1";
    connection.query_row(one, [], |row| row.get::<_, i64>(0)).map_err(|error| sql_error("read", error))?;
    std::thread::sleep(std::time::Duration::from_millis(20));
    Ok(())
}

/// A file of 512-byte pages whose commits `kept` keeps, with a table `t`
/// of rows about a page long.
fn small_pages(path: &Path, kept: &Arc<Kept>) -> Arc<File> {
    let config = Config { page_size: 512, durability: crate::sqlite::Durability::Os, ..Config::default() };
    let observer: Arc<dyn Observe> = Arc::clone(kept) as Arc<dyn Observe>;
    let file = File::open_observed(path, config, Some(observer)).unwrap();
    let table = "create table t (id integer primary key, v blob not null)";
    file.transaction(|tx| tx.execute_batch(table).map_err(|error| sql_error("t", error))).unwrap();
    Arc::new(file)
}

/// Writes a thousand rows of `t` in one commit, as many times as take the
/// WAL past where its writer restarts it.
fn fill_past_the_bound(file: &File) {
    for _ in 0..(HARD / 1000 + 16) {
        file.write(0, |tx| {
            for _ in 0..1000 {
                tx.execute("insert into t (v) values (zeroblob(412))", []).map_err(|error| sql_error("fill", error))?;
            }
            Ok(())
        })
        .unwrap();
    }
}

/// Writes a thousand rows of `t`, then writes each of them again in one
/// commit, as many times as take the WAL past where its writer restarts it:
/// commits that leave the file as long, which write no page 1.
fn rewrite_past_the_bound(file: &File) {
    file.write(0, |tx| {
        for _ in 0..1000 {
            tx.execute("insert into t (v) values (zeroblob(412))", []).map_err(|error| sql_error("fill", error))?;
        }
        Ok(())
    })
    .unwrap();
    for _ in 0..(HARD / 1000 + 16) {
        // bytes of their own, since a row written again as it was writes no page
        let again = |tx: &crate::sqlite::Tx<'_>| tx.execute("update t set v = randomblob(412)", []).map(drop);
        file.write(0, move |tx| again(tx).map_err(|error| sql_error("rewrite", error))).unwrap();
    }
}

/// The generations of the WAL that `positions` stood in, and the longest
/// it grew to.
fn generations_and_longest(positions: &[Position]) -> (usize, u32) {
    let generations: std::collections::HashSet<_> = positions.iter().map(|at| (at.checkpoints, at.salts)).collect();
    (generations.len(), positions.iter().map(|at| at.frame).max().unwrap_or(0))
}

#[test]
fn reads_that_never_stop_leave_a_logged_wal_no_longer_than_its_bound_and_lose_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let kept = Arc::new(Kept::default());
    let file = small_pages(&path, &kept);
    let reader = Arc::clone(&file);
    let (reading, readers) = reading_always(move || reader.read(a_moments_snapshot).unwrap());
    fill_past_the_bound(&file);
    reading.store(false, Ordering::SeqCst);
    readers.into_iter().for_each(|reader| reader.join().unwrap());

    let positions: Vec<Position> = kept.commits.lock().unwrap().iter().map(|commit| commit.position).collect();
    let (generations, longest) = generations_and_longest(&positions);
    assert!(longest < HARD + 10_000, "a WAL {longest} frames long, past {HARD} by more than a few tries");
    assert!(generations > 1, "the WAL started over though reads never stopped");
    assert!(kept.gaps.lock().unwrap().is_empty(), "the capture's own restarts leave no gap");
    file.close().unwrap();
    assert_eq!(kept.rebuilt(), the_file_after_close(&path));
}

#[test]
fn reads_that_never_stop_leave_a_copys_wal_no_longer_than_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Arc::new(Kept::default());
    let home = small_pages(&dir.path().join("home.db"), &kept);
    rewrite_past_the_bound(&home);
    home.close().unwrap();

    let copy = dir.path().join("copy.db");
    let applier = crate::sqlite::Applier::new(&copy, crate::sqlite::Durability::Os);
    let readers = Arc::new(crate::sqlite::Readers::new(copy.clone(), Arc::new(Config::default())));
    let (mut positions, mut reads) = (Vec::new(), None);
    for (at, commit) in kept.commits.lock().unwrap().iter().enumerate() {
        let pages: Vec<(u32, &[u8])> = commit.pages.iter().map(|(number, page)| (*number, page.as_slice())).collect();
        positions.push(applier.apply(commit.page_size, commit.size, &pages).unwrap());
        if applier.is_long() {
            applier.checkpoint().unwrap();
        }
        // once the copy holds `t`
        if at == 0 {
            let readers = Arc::clone(&readers);
            reads = Some(reading_always(move || readers.read(a_moments_snapshot).unwrap()));
        }
    }
    let (reading, threads) = reads.unwrap();
    reading.store(false, Ordering::SeqCst);
    threads.into_iter().for_each(|reader| reader.join().unwrap());
    applier.close();
    readers.close();

    let (generations, longest) = generations_and_longest(&positions);
    assert!(longest < HARD + 10_000, "a copy's WAL {longest} frames long, past {HARD} by more than a few tries");
    assert!(generations > 1, "the copy's WAL started over though reads never stopped");
}
