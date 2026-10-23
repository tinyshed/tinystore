use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::sqlite::{Config, File};

fn incremental() -> Config {
    Config { incremental_vacuum: true, ..Config::default() }
}

/// When the tests' windows begin.
fn at(minutes: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000) + Duration::from_mins(minutes)
}

/// A file of `rows` rows of 600 bytes, six a page, its first half then
/// removed: the free pages lie before those in use, which a give back moves.
fn half_free(path: &Path, config: Config, rows: u32) -> File {
    let file = File::open(path, config).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table b (id integer primary key, body blob not null)")
            .and_then(|()| {
                tx.execute(
                    "with recursive n(i) as (select 1 union all select i + 1 from n where i < ?1)
                     insert into b (id, body) select i, randomblob(600) from n",
                    [rows],
                )
            })
            .and_then(|_| tx.execute("delete from b where id <= ?1", [rows / 2]))
            .map_err(|error| sql_error("fill b", error))
    })
    .unwrap();
    file
}

fn occupancy(file: &File) -> Occupancy {
    file.read(Occupancy::read).unwrap()
}

/// What the rows hold, which giving pages back must not change.
fn held(file: &File) -> (i64, i64, Vec<u8>) {
    file.read(|connection| {
        let read = || -> rusqlite::Result<(i64, i64, Vec<u8>)> {
            let sums = "select count(*), sum(id) from b";
            let (count, sum) = connection.query_row(sums, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut select = connection.prepare("select body from b order by id")?;
            let bodies = select.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
            Ok((count, sum, bodies.collect::<rusqlite::Result<Vec<_>>>()?.concat()))
        };
        read().map_err(|error| sql_error("read b", error))
    })
    .unwrap()
}

// contract: DUR-09
#[test]
fn a_file_made_incremental_gives_back_its_free_pages_as_many_as_a_call_may() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let file = half_free(&path, incremental(), 6000);
    let before = occupancy(&file);
    let rows = held(&file);
    assert!(before.incremental && before.free > 400, "{before:?}");

    let most = 100 * before.page_size as usize;
    assert_eq!(file.give_back(most, at(0)).unwrap(), 0, "a window of free pages begins");
    assert_eq!(file.give_back(most, at(10)).unwrap(), 100);
    let after = occupancy(&file);
    assert_eq!(after.free, before.free - 100);
    assert!(after.pages <= before.pages - 100, "{after:?}");

    assert_eq!(file.give_back(usize::MAX, at(11)).unwrap(), before.free - 100);
    let after = occupancy(&file);
    // a page of the pointer map, which only a file that gives pages back has, may go with them
    assert!(after.free == 0 && after.pages <= before.pages - before.free, "{after:?}");
    assert_eq!(held(&file), rows, "the rows as they were");
    assert_eq!(file.give_back(usize::MAX, at(30)).unwrap(), 0, "nothing is left to give back");

    file.close().unwrap();
    let on_disk = std::fs::metadata(&path).unwrap().len();
    assert_eq!(on_disk, after.pages * after.page_size, "the file cut to its pages");
}

#[test]
fn a_file_with_nothing_free_commits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = File::open(dir.path().join("test.db"), incremental()).unwrap();
    file.transaction(|tx| {
        tx.execute_batch("create table b (id integer primary key, body blob not null)")
            .map_err(|error| sql_error("create b", error))
    })
    .unwrap();
    let commits = file.commits();
    assert_eq!(file.give_back(usize::MAX, at(0)).unwrap(), 0);
    assert_eq!(file.give_back(usize::MAX, at(10)).unwrap(), 0);
    assert_eq!(file.commits(), commits);
}

#[test]
fn free_pages_go_back_once_they_stayed_free_a_whole_window() {
    let mut unneeded = Unneeded::default();
    assert_eq!(unneeded.note(at(0), 0), 0);
    assert_eq!(unneeded.note(at(5), 100), 0);
    assert_eq!(unneeded.note(at(10), 100), 0, "the window held none free at its start");
    assert_eq!(unneeded.note(at(15), 100), 0);
    assert_eq!(unneeded.note(at(20), 100), 100, "free through the whole of the next");
    unneeded.given(40);
    assert_eq!(unneeded.note(at(21), 60), 60, "what the window found and is not given back yet");
    unneeded.given(60);
    assert_eq!(unneeded.note(at(22), 0), 0);

    let mut taken_back = Unneeded::default();
    assert_eq!(taken_back.note(at(0), 100), 0);
    assert_eq!(taken_back.note(at(5), 3), 0, "the writes took them back");
    assert_eq!(taken_back.note(at(9), 100), 0);
    assert_eq!(taken_back.note(at(10), 100), 3, "only what no write took");

    let mut moved_back = Unneeded::default();
    assert_eq!(moved_back.note(at(30), 100), 0);
    assert_eq!(moved_back.note(at(0), 100), 0, "a clock moved back begins the window again");
    assert_eq!(moved_back.note(at(10), 100), 100);
}

// contract: DUR-09
#[test]
fn a_file_made_before_files_could_give_pages_back_is_vacuumed_once_while_small() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let made = half_free(&path, Config::default(), 6000);
    let rows = held(&made);
    made.close().unwrap();
    drop(made);

    // the setting a file opens with comes too late for a file with tables
    let file = File::open(&path, incremental()).unwrap();
    let before = occupancy(&file);
    assert!(!before.incremental && before.free > 400, "{before:?}");
    let given = file.give_back(4096, at(0)).unwrap();
    let after = occupancy(&file);
    assert!(after.incremental && after.free == 0, "{after:?}");
    assert_eq!(after.pages, before.pages - given);
    // the vacuum packs every page full again, and adds a page of the pointer map
    assert!(after.pages <= before.pages - before.free + 1, "{before:?} {after:?}");
    assert_eq!(held(&file), rows, "the rows as they were");
    let wal = std::fs::metadata(crate::sqlite::wal_of(&path)).map_or(0, |found| found.len());
    assert_eq!(wal, 0, "the WAL that held the whole file is emptied");

    let delete = |tx: &crate::sqlite::Tx<'_>| tx.execute("delete from b where id < 4000", []);
    file.transaction(|tx| delete(tx).map_err(|error| sql_error("delete", error))).unwrap();
    let freed = occupancy(&file).free;
    assert!(freed > 0);
    assert_eq!(file.give_back(usize::MAX, at(1)).unwrap(), 0);
    assert_eq!(file.give_back(usize::MAX, at(11)).unwrap(), freed, "and from then on gives pages back");
}

#[test]
fn a_file_made_before_and_larger_than_a_vacuum_may_rewrite_keeps_its_free_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    // the 15,000 rows left take 2,500 pages of 4 KiB, more than 8 MiB
    half_free(&path, Config::default(), 30_000).close().unwrap();
    let file = File::open(&path, incremental()).unwrap();
    let before = occupancy(&file);
    assert!(before.in_use() > VACUUM_UP_TO && before.free > 0, "{before:?}");
    assert_eq!(file.give_back(usize::MAX, at(0)).unwrap(), 0);
    assert_eq!(file.give_back(usize::MAX, at(10)).unwrap(), 0);
    assert_eq!(occupancy(&file), before);
}

#[test]
fn a_file_its_engine_keeps_as_it_was_made_is_never_vacuumed() {
    let dir = tempfile::tempdir().unwrap();
    let file = half_free(&dir.path().join("test.db"), Config::default(), 6000);
    let before = occupancy(&file);
    assert!(!before.incremental && before.free > 0, "{before:?}");
    assert_eq!(file.give_back(usize::MAX, at(0)).unwrap(), 0);
    assert_eq!(file.give_back(usize::MAX, at(10)).unwrap(), 0);
    assert_eq!(occupancy(&file), before);
}

#[test]
fn a_copy_gives_back_nothing_its_home_did_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    half_free(&path, incremental(), 6000).close().unwrap();
    let copy = File::open_copy(&path, incremental()).unwrap();
    let before = occupancy(&copy);
    assert!(before.free > 0, "{before:?}");
    assert_eq!(copy.give_back(usize::MAX, at(0)).unwrap(), 0);
    assert_eq!(copy.give_back(usize::MAX, at(10)).unwrap(), 0);
    assert_eq!(occupancy(&copy), before);
}
