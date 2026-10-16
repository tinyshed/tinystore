use std::fs;
use std::sync::Arc;

use super::entry::Kind;
use super::*;
use crate::sqlite::{Config, File, sql_error};

#[test]
fn a_log_keeps_its_bounds_and_counts_on_past_the_segments_it_removed() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join(DIR).join("test.db");
    let bounds = Bounds { segment: 64 << 10, keep: 256 << 10 };
    let observer: Arc<dyn crate::sqlite::Observe> = Arc::new(Log::open(logs.clone(), bounds).unwrap());
    let file = File::open_observed(dir.path().join("test.db"), Config::default(), Some(observer)).unwrap();
    file.transaction(|tx| tx.execute_batch("create table t (v text)").map_err(|error| sql_error("create", error)))
        .unwrap();
    for i in 0..600 {
        let value = format!("{i:04}{}", "y".repeat(1000));
        file.write(value.len(), move |tx| {
            tx.execute("insert into t (v) values (?1)", [&value]).map(drop).map_err(|error| sql_error("insert", error))
        })
        .unwrap();
    }
    file.close().unwrap();
    let segments: Vec<_> = fs::read_dir(&logs).unwrap().map(|found| found.unwrap().metadata().unwrap().len()).collect();
    let bytes: u64 = segments.iter().sum();
    assert!(segments.len() > 1 && bytes <= bounds.keep + bounds.segment, "{} segments, {bytes} bytes", segments.len());
    let mut sequences = Vec::new();
    let mut previous = None;
    Log::read(&logs, |entry| {
        assert!(previous.is_none_or(|check| check == entry.previous), "each entry names the one before");
        assert_eq!((entry.kind, entry.page_size), (Kind::Commit, 4096));
        assert!(entry.size > 0 && !entry.pages.is_empty());
        previous = Some(entry.check);
        sequences.push(entry.sequence);
        Ok(())
    })
    .unwrap();
    assert!(sequences[0] > 1, "the oldest segments went");
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1), "what is kept has no hole");
    assert_eq!(*sequences.last().unwrap(), 600 + 2, "a beginning, the table and each insert");
}

#[test]
fn a_copy_takes_a_file_s_log_whole_with_no_engine_open() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join(DIR).join("test.db");
    let observer: Arc<dyn crate::sqlite::Observe> = Arc::new(Log::open(logs.clone(), Bounds::default()).unwrap());
    let file = File::open_observed(home.path().join("test.db"), Config::default(), Some(observer)).unwrap();
    file.transaction(|tx| tx.execute_batch("create table t (v text)").map_err(|error| sql_error("create", error)))
        .unwrap();
    for i in 0..50 {
        let value = format!("row {i}");
        file.write(value.len(), move |tx| {
            tx.execute("insert into t (v) values (?1)", [&value]).map(drop).map_err(|error| sql_error("insert", error))
        })
        .unwrap();
    }
    file.close().unwrap();

    let there = tempfile::tempdir().unwrap();
    let options = crate::Options { node: Some(Node::default()), background: false, ..crate::Options::default() };
    let store = crate::Store::open(there.path(), options).unwrap();
    let copy = super::copy::Copy::start(&store, "test.db").unwrap();
    Log::read(&logs, |entry| copy.apply(&entry)).unwrap();
    assert_eq!(copy.applied(), 52, "a beginning, the table and each row");
    let read = File::open_copy(there.path().join("test.db"), Config::default()).unwrap();
    let rows: i64 = read
        .read(|connection| {
            connection.query_row("select count(*) from t", [], |row| row.get(0)).map_err(|e| sql_error("count", e))
        })
        .unwrap();
    assert_eq!(rows, 50);
    let refused = read.write(0, |tx| tx.execute("delete from t", []).map(drop).map_err(|e| sql_error("delete", e)));
    assert_eq!(refused.unwrap_err().kind(), crate::ErrorKind::Invalid);
}
