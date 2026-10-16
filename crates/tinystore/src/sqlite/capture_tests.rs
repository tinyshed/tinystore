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
    let wal = Committed::read(&bytes).expect("a WAL with commits");
    assert_eq!(wal.end, kept.commits.lock().unwrap().last().unwrap().position.frame);
    let mut torn = bytes.clone();
    let last = torn.len() - 1;
    torn[last] ^= 0xff;
    let recovered = Committed::read(&torn).expect("the commits before the torn frame");
    assert!(recovered.end < wal.end, "a frame whose checksum breaks ends what is recovered");
    std::mem::forget(file);
}
