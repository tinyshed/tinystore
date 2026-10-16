use std::fs;
use std::path::Path;

use serde_json::json;

use super::entry::{self, Kind, MAGIC};
use super::*;
use crate::{Options, Store, sql};

const FILES: [&str; 7] = ["kv.db", "jobs.db", "sql/app.db", "search.db", "blobs/blobs.db", "records.db", "metrics.db"];

fn node(dir: &Path) -> Store {
    let options = Options { node: Some(Node::default()), background: false, ..Options::default() };
    Store::open(dir, options).unwrap()
}

/// Writes through every engine whose file the test reads back.
fn write_through_every_engine(store: &Store, round: u32) {
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    for i in 0..40 {
        sessions.set(format!("{round}/{i}"), &format!("session {i} of round {round}")).unwrap();
    }
    sessions.delete(format!("{round}/3")).unwrap();
    store.counters("views").open().unwrap().add("home", 1).unwrap();
    let mail = store.queue::<serde_json::Value>("mail").open().unwrap();
    for i in 0..10 {
        mail.add(&json!({ "to": i, "round": round })).unwrap();
    }
    let db = store
        .database("app")
        .migrations([
            ("0001_notes.sql", "create table notes (id integer primary key, body text not null);"),
            ("0002_owner.sql", "alter table notes add column owner text;"),
        ])
        .open()
        .unwrap();
    for i in 0..30 {
        db.exec(sql!("insert into notes (body, owner) values (?, ?)", format!("note {i}"), format!("owner {}", i % 4)))
            .unwrap();
    }
    db.exec(sql!("delete from notes where id % 5 = 0")).unwrap();
    let docs = store.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    for i in 0..20 {
        docs.set(format!("{round}-{i}"), &json!({ "title": format!("replicated page {i} round {round}") })).unwrap();
    }
    crate::search::maintain(store).unwrap();
    store.files("docs").open().unwrap().put(&format!("{round}.txt"), vec![b'x'; 9000]).unwrap();
    store.events("audit").open().unwrap().append("login", &json!({ "round": round })).unwrap();
    let metrics = store.metrics().unwrap();
    metrics.counter("logins").open().unwrap().add(f64::from(round));
    metrics.flush().unwrap();
}

/// The file a log's commits make from an empty one, after checking that
/// its entries count from 1, chain, and begin once with an empty file.
fn rebuilt_from_its_log(dir: &Path) -> Vec<u8> {
    let mut image = Vec::new();
    let mut expected = 1;
    let mut previous = [0; 16];
    Log::read(dir, |entry| {
        assert_eq!(
            (entry.sequence, entry.previous),
            (expected, previous),
            "{}: entries count and chain",
            dir.display()
        );
        assert_eq!(entry.kind == Kind::Begin, entry.sequence == 1, "{}: one beginning, the first", dir.display());
        expected += 1;
        previous = entry.check;
        let page_size = entry.page_size as usize;
        if entry.kind == Kind::Commit {
            image.resize(entry.size as usize * page_size, 0);
            for (number, page) in entry.pages {
                let at = (number as usize - 1) * page_size;
                if at < image.len() {
                    image[at..at + page_size].copy_from_slice(page);
                }
            }
        }
        assert_ne!(entry.kind, Kind::Gap, "{}: nothing wrote beside the store", dir.display());
        Ok(())
    })
    .unwrap();
    image
}

fn assert_each_log_rebuilds_its_file(dir: &Path) {
    for name in FILES {
        let file = fs::read(dir.join(name)).unwrap();
        assert!(file.len() > 4096, "{name} holds something");
        assert!(rebuilt_from_its_log(&dir.join(DIR).join(name)) == file, "{name}: its log rebuilds it byte for byte");
    }
}

#[test]
fn a_node_logs_every_file_and_each_log_rebuilds_its_file_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = node(dir.path());
    write_through_every_engine(&store, 1);
    store.close().unwrap();
    let store = node(dir.path());
    write_through_every_engine(&store, 2);
    store.close().unwrap();
    assert_each_log_rebuilds_its_file(dir.path());
}

#[test]
fn a_store_alone_keeps_no_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options { background: false, ..Options::default() }).unwrap();
    write_through_every_engine(&store, 1);
    store.close().unwrap();
    assert!(!dir.path().join(DIR).exists());
}

/// Copies a store's directory as a process that died left it: its files,
/// WALs and logs as they are, the store still open.
fn copy_as_left(from: &Path, to: &Path) {
    for found in fs::read_dir(from).unwrap() {
        let found = found.unwrap();
        let target = to.join(found.file_name());
        if found.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_as_left(&found.path(), &target);
        } else if found.file_name() != "LOCK" {
            fs::copy(found.path(), target).unwrap();
        }
    }
}

#[test]
fn a_log_whose_tail_a_crash_took_is_mended_from_the_wal_at_any_byte() {
    let dir = tempfile::tempdir().unwrap();
    let store = node(dir.path());
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    for i in 0..12 {
        sessions.set(format!("key {i}"), &"v".repeat(300 + i)).unwrap();
    }
    let left = tempfile::tempdir().unwrap();
    copy_as_left(dir.path(), left.path());
    let segment = left.path().join(DIR).join("kv.db").join(format!("{:020}.log", 1));
    let whole = fs::read(&segment).unwrap();
    let (_, beginning) = entry::decode(&whole[MAGIC.len()..]).unwrap().unwrap();
    let cuts: Vec<usize> = (MAGIC.len() + beginning..whole.len()).step_by(997).chain([whole.len() - 1]).collect();
    for cut in cuts {
        let crashed = tempfile::tempdir().unwrap();
        copy_as_left(left.path(), crashed.path());
        fs::write(crashed.path().join(DIR).join("kv.db").join(format!("{:020}.log", 1)), &whole[..cut]).unwrap();
        let reopened = node(crashed.path());
        let sessions = reopened.bucket::<String>("sessions").open().unwrap();
        assert_eq!(sessions.get("key 11").unwrap().as_deref(), Some("v".repeat(311).as_str()));
        sessions.set("after", &"a".to_owned()).unwrap();
        reopened.close().unwrap();
        let file = fs::read(crashed.path().join("kv.db")).unwrap();
        assert!(rebuilt_from_its_log(&crashed.path().join(DIR).join("kv.db")) == file, "cut at byte {cut}");
    }
    store.close().unwrap();
}

#[test]
fn a_crash_after_the_wal_started_over_loses_nothing_its_log_had_not_synced() {
    let dir = tempfile::tempdir().unwrap();
    let store = node(dir.path());
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    for i in 0..1500 {
        sessions.set(format!("key {i}"), &"v".repeat(1200)).unwrap();
    }
    let left = tempfile::tempdir().unwrap();
    copy_as_left(dir.path(), left.path());
    // A crash can tear only the last segment: the one before was synced when
    // the last began.
    let logs = left.path().join(DIR).join("kv.db");
    let last = fs::read_dir(&logs).unwrap().map(|found| found.unwrap().file_name()).max().unwrap();
    let whole = fs::read(logs.join(&last)).unwrap();
    let mut entries = Vec::new();
    let mut at = MAGIC.len();
    while let Some((entry, used)) = entry::decode(&whole[at..]).unwrap() {
        entries.push((at, entry.position));
        at += used;
    }
    let generation = |position: crate::sqlite::Position| (position.checkpoints, position.salts);
    let current = generation(entries.last().unwrap().1);
    let mut generations = std::collections::HashSet::new();
    Log::read(&logs, |entry| {
        generations.insert(generation(entry.position));
        Ok(())
    })
    .unwrap();
    assert!(generations.len() > 2, "the WAL started over at least once");
    let start = entries.iter().find(|(_, position)| generation(*position) == current).unwrap().0;
    let step = (whole.len() - start) / 8;
    for cut in (start..whole.len()).step_by(step.max(1)).chain([whole.len() - 3]) {
        let crashed = tempfile::tempdir().unwrap();
        copy_as_left(left.path(), crashed.path());
        fs::write(crashed.path().join(DIR).join("kv.db").join(&last), &whole[..cut]).unwrap();
        let reopened = node(crashed.path());
        reopened.bucket::<String>("sessions").open().unwrap().set("after", &"a".to_owned()).unwrap();
        reopened.close().unwrap();
        let file = fs::read(crashed.path().join("kv.db")).unwrap();
        assert!(rebuilt_from_its_log(&crashed.path().join(DIR).join("kv.db")) == file, "cut at byte {cut}");
    }
    store.close().unwrap();
}
