use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::copy::Copy;
use super::entry::{self, Kind, MAGIC};
use super::node_tests::{FILES, node, write_through_every_engine};
use super::*;
use crate::{ErrorKind, Options, Store, sql};

/// A file's pages as a reader of it sees them, page 1's change counters,
/// which each file's own commits move, left out.
pub(super) fn pages_seen(path: &Path) -> Vec<u8> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let connection = rusqlite::Connection::open_with_flags(path, flags).unwrap();
    let mut select = connection.prepare("select data from sqlite_dbpage order by pgno").unwrap();
    let pages = select.query_map([], |row| row.get::<_, Vec<u8>>(0)).unwrap();
    let mut image: Vec<u8> = pages.flat_map(Result::unwrap).collect();
    without_change_counters(&mut image);
    image
}

fn without_change_counters(image: &mut [u8]) {
    if image.len() >= 96 {
        image[24..28].fill(0);
        image[92..96].fill(0);
    }
}

/// Copies each file of `home` into `copy` through its log, and checks after
/// each commit that the copy is the home's file at that commit.
fn ship_checking_each_commit(home: &Path, copy: &Store, names: &[&str]) {
    for name in names {
        let shipped = Copy::start(copy, name).unwrap();
        let held_already = shipped.applied();
        let mut image = Vec::new();
        Log::read(&home.join(DIR).join(name), |entry| {
            if entry.kind == Kind::Commit {
                let page_size = entry.page_size as usize;
                image.resize(entry.size as usize * page_size, 0);
                for (number, page) in &entry.pages {
                    let at = (*number as usize - 1) * page_size;
                    image[at..at + page_size].copy_from_slice(page);
                }
            }
            shipped.apply(&entry, None)?;
            if entry.kind == Kind::Commit && entry.sequence > held_already {
                let mut expected = image.clone();
                without_change_counters(&mut expected);
                assert!(pages_seen(&copy.dir().join(name)) == expected, "{name} at entry {}", entry.sequence);
            }
            Ok(())
        })
        .unwrap();
    }
}

const NOTES: [(&str, &str); 2] = [
    ("0001_notes.sql", "create table notes (id integer primary key, body text not null);"),
    ("0002_owner.sql", "alter table notes add column owner text;"),
];

#[test]
fn a_copy_is_its_home_at_each_commit_and_its_engines_read_it_and_write_nothing() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path());
    write_through_every_engine(&home, 1);
    let copy = node(copy_dir.path());
    ship_checking_each_commit(home_dir.path(), &copy, &FILES);

    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("1/7").unwrap().as_deref(), Some("session 7 of round 1"));
    assert_eq!(sessions.get("1/3").unwrap(), None);
    assert_eq!(copy.counters("views").open().unwrap().get("home").unwrap(), 1);
    let db = copy.database("app").migrations(NOTES).open().unwrap();
    let notes: Vec<(i64, String)> = db.all(sql!("select id, body from notes order by id")).unwrap();
    assert_eq!(notes.len(), 24);
    let docs = copy.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    assert_eq!(docs.search("replicated page 7").run().unwrap().hits[0].id, "1-7");
    let file = copy.files("docs").open().unwrap().get("1.txt").unwrap().unwrap().read_all().unwrap();
    assert_eq!(file, vec![b'x'; 9000]);
    assert_eq!(copy.events("audit").open().unwrap().list().page().unwrap().events.len(), 1);
    assert_eq!(copy.metrics().unwrap().names().page().unwrap().names[0].name, "logins");
    let mail = copy.queue::<serde_json::Value>("mail").open().unwrap();
    assert_eq!(mail.add(&serde_json::json!({ "to": "no" })).unwrap_err().kind(), ErrorKind::Unavailable);

    // a write that passes to its home finds no link to send it on: this node takes from none
    let refused = sessions.set("new", &"no".to_owned()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Unavailable, "{refused}");
    let refused = db.exec(sql!("insert into notes (body) values (?)", "no")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Unavailable, "{refused}");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_takes_what_its_home_writes_next_its_caches_following() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path());
    write_through_every_engine(&home, 1);
    let copy = node(copy_dir.path());
    ship_checking_each_commit(home_dir.path(), &copy, &FILES);
    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("2/1").unwrap(), None);

    write_through_every_engine(&home, 2);
    home.bucket::<String>("later").open().unwrap().set("k", &"v".to_owned()).unwrap();
    ship_checking_each_commit(home_dir.path(), &copy, &FILES);
    assert_eq!(sessions.get("2/1").unwrap().as_deref(), Some("session 1 of round 2"));
    assert_eq!(copy.bucket::<String>("later").open().unwrap().get("k").unwrap().as_deref(), Some("v"));
    assert_eq!(copy.events("audit").open().unwrap().list().page().unwrap().events.len(), 2);
    let metrics = copy.metrics().unwrap();
    assert_eq!(metrics.names().page().unwrap().names.len(), 1);
    home.metrics().unwrap().counter("signups").open().unwrap().inc();
    home.metrics().unwrap().flush().unwrap();
    ship_checking_each_commit(home_dir.path(), &copy, &["metrics.db"]);
    assert_eq!(metrics.names().page().unwrap().names.len(), 2, "a name its home made since is read");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_reader_of_a_copy_never_sees_half_a_commit() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pairs = [("0001_pairs.sql", "create table t (pair integer not null);")];
    let home = node(home_dir.path());
    let db = home.database("pairs").migrations(pairs).open().unwrap();
    for pair in 0..200 {
        db.tx(|tx| {
            tx.exec(sql!("insert into t (pair) values (?)", pair))?;
            tx.exec(sql!("insert into t (pair) values (?)", pair))
        })
        .unwrap();
    }
    let copy = node(copy_dir.path());
    let shipped = Copy::start(&copy, "sql/pairs.db").unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let mut reader = None;
    Log::read(&home_dir.path().join(DIR).join("sql/pairs.db"), |entry| {
        shipped.apply(&entry, None)?;
        if reader.is_none() && entry.kind == Kind::Commit {
            let read = copy.database("pairs").migrations(pairs).open()?;
            let watching = Arc::clone(&done);
            reader = Some(thread::spawn(move || {
                let mut reads = 0;
                while !watching.load(Ordering::SeqCst) || reads == 0 {
                    let count: Vec<(i64,)> = read.all(sql!("select count(*) from t")).unwrap();
                    assert_eq!(count[0].0 % 2, 0, "a reader saw half of a commit");
                    reads += 1;
                }
                reads
            }));
        }
        Ok(())
    })
    .unwrap();
    done.store(true, Ordering::SeqCst);
    assert!(reader.unwrap().join().unwrap() > 0);
    assert_eq!(shipped.applied(), 2 + 200, "a beginning, the table and each pair");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_refuses_an_entry_that_does_not_follow_and_takes_one_it_has_as_nothing() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..5 {
        sessions.set(format!("{i}"), &"v".to_owned()).unwrap();
    }
    home.close().unwrap();
    let copy = node(copy_dir.path());
    let shipped = Copy::start(&copy, "kv.db").unwrap();
    let segment = fs::read(home_dir.path().join(DIR).join("kv.db").join(format!("{:020}.log", 1))).unwrap();
    let mut at = MAGIC.len();
    let mut kept = Vec::new();
    while let Some((_, used)) = entry::decode(&segment[at..]).unwrap() {
        kept.push(at..at + used);
        at += used;
    }
    let entry_at = |index: usize| entry::decode(&segment[kept[index].clone()]).unwrap().unwrap().0;
    shipped.apply(&entry_at(0), None).unwrap();
    let skipped = shipped.apply(&entry_at(2), None).unwrap_err();
    assert_eq!(skipped.kind(), ErrorKind::Invalid, "{skipped}");
    shipped.apply(&entry_at(1), None).unwrap();
    shipped.apply(&entry_at(1), None).unwrap();
    assert_eq!(shipped.applied(), 2, "an entry it holds is taken as nothing");
    let mut forged = entry_at(2);
    forged.previous = [7; 16];
    assert_eq!(shipped.apply(&forged, None).unwrap_err().kind(), ErrorKind::Conflict);
    let mut other = entry_at(1);
    other.check = [9; 16];
    assert_eq!(shipped.apply(&other, None).unwrap_err().kind(), ErrorKind::Conflict);
    drop(shipped);

    let again = Copy::start(&copy, "kv.db").unwrap();
    assert_eq!(again.applied(), 2, "a copy opened again goes on from its position");
    for index in 2..kept.len() {
        again.apply(&entry_at(index), None).unwrap();
    }
    assert_eq!(copy.bucket::<String>("sessions").open().unwrap().get("4").unwrap().as_deref(), Some("v"));
    copy.close().unwrap();
}

#[test]
fn a_copy_is_not_made_of_a_file_the_store_wrote_nor_from_a_log_begun_on_a_file_with_pages() {
    let dir = tempfile::tempdir().unwrap();
    let store = node(dir.path());
    store.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    assert_eq!(Copy::start(&store, "kv.db").err().unwrap().kind(), ErrorKind::Invalid);
    store.close().unwrap();

    let alone = tempfile::tempdir().unwrap();
    let before = Store::open(alone.path(), Options { background: false, ..Options::default() }).unwrap();
    before.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    before.close().unwrap();
    let became = node(alone.path());
    became.bucket::<String>("sessions").open().unwrap().set("k2", &"v".to_owned()).unwrap();
    became.close().unwrap();
    let copy_dir = tempfile::tempdir().unwrap();
    let copy = node(copy_dir.path());
    let shipped = Copy::start(&copy, "kv.db").unwrap();
    let refused = Log::read(&alone.path().join(DIR).join("kv.db"), |entry| shipped.apply(&entry, None)).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Conflict, "a copy starts from a snapshot: {refused}");
}

#[test]
fn a_migration_its_home_has_not_shipped_yet_is_unavailable_on_a_copy() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let migrations = [("0001_t.sql", "create table t (v text);")];
    let home = node(home_dir.path());
    home.database("app").migrations(migrations).open().unwrap();
    home.close().unwrap();
    let copy = node(copy_dir.path());
    let shipped = Copy::start(&copy, "sql/app.db").unwrap();
    let early = copy.database("app").migrations(migrations).open().err().unwrap();
    assert_eq!(early.kind(), ErrorKind::Unavailable, "{early}");
    Log::read(&home_dir.path().join(DIR).join("sql/app.db"), |entry| shipped.apply(&entry, None)).unwrap();
    copy.database("app").migrations(migrations).open().unwrap();
}
