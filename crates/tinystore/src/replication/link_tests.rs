use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::copy::Copy;
use super::copy_tests::pages_seen;
use super::entry::Point;
use super::node_tests::write_through_every_engine;
use super::snapshot::{self, Part, Taking};
use super::*;
use crate::{Options, Store};

/// The files a node that copies every file takes from its home: all but its
/// own records and metrics.
const TAKEN: [&str; 5] = ["kv.db", "jobs.db", "sql/app.db", "search.db", "blobs/blobs.db"];

fn node_with(dir: &Path, node: Node) -> Store {
    let options = Options { node: Some(node), background: false, ..Options::default() };
    Store::open(dir, options).unwrap()
}

/// The address a node's server is bound to, which asked for any port.
fn bound(state: &State) -> Option<std::net::SocketAddr> {
    *state.listening.lock().unwrap()
}

fn serving(dir: &Path) -> (Store, String) {
    let store = node_with(dir, Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let address = bound(store.node().unwrap()).unwrap().to_string();
    (store, address)
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !holds() {
        assert!(Instant::now() < deadline, "{what}: not within 30 seconds");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Where the home's log of `name` stands with no commit under way.
fn home_point(home: &Store, name: &str) -> Point {
    home.node().unwrap().source(name).unwrap().stable().unwrap()
}

fn copy_point(copy: &Store, name: &str) -> Option<Point> {
    copy.node().unwrap().copy(name).map(|copy| copy.point())
}

fn caught_up(home: &Store, copy: &Store, names: &[&str]) {
    for name in names {
        eventually(&format!("{name} caught up"), || copy_point(copy, name) == Some(home_point(home, name)));
        assert!(pages_seen(&home.dir().join(name)) == pages_seen(&copy.dir().join(name)), "{name} is its home's");
    }
}

#[test]
fn a_snapshot_read_while_its_home_writes_becomes_the_home_s_file() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node_with(home_dir.path(), Node::default());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..3000 {
        sessions.set(format!("{i}"), &"x".repeat(1500)).unwrap();
    }
    let copy = node_with(copy_dir.path(), Node::default());
    let dir = copy.log_of("kv.db").unwrap();
    let source = home.node().unwrap().source("kv.db").unwrap();
    let mut taking = None;
    let mut chunks = 0;
    let at = snapshot::read(source.as_ref(), |part| {
        match part {
            Part::Begin { page_size, at } => {
                taking = Some(Taking::begin(&dir, &copy.dir().join("kv.db"), page_size, at)?);
            }
            Part::Pages(pages) => {
                taking.as_mut().unwrap().pages(&pages)?;
                chunks += 1;
                // the home goes on between the chunks
                for i in 0..40 {
                    sessions.set(format!("{}", (chunks * 97 + i * 13) % 3000), &format!("chunk {chunks}"))?;
                }
                sessions.delete(format!("{}", chunks * 31))?;
            }
            Part::End { ready_at } => taking.as_mut().unwrap().end(ready_at),
        }
        Ok(())
    })
    .unwrap();
    assert!(chunks > 4, "the file took {chunks} chunks");
    let mut taking = taking.unwrap();
    assert!(!taking.whole(), "commits ended while it read");
    let log = Log::open(dir.clone(), Bounds::default(), Arc::default()).unwrap();
    let home_log = home_dir.path().join(DIR).join("kv.db");
    Log::read(&home_log, |entry| {
        if !taking.whole() {
            let bytes = super::entry::encode(
                &super::entry::Head {
                    kind: entry.kind,
                    sequence: entry.sequence,
                    epoch: entry.epoch,
                    previous: entry.previous,
                    position: entry.position,
                    page_size: entry.page_size,
                    size: entry.size,
                },
                &entry.pages,
            )
            .0;
            taking.apply(&entry, &bytes, &log)?;
        }
        Ok(())
    })
    .unwrap();
    assert!(taking.whole());
    drop(log);
    taking.finish().unwrap();
    let shipped = Copy::open(&copy, "kv.db").unwrap();
    assert!(shipped.applied() > at.sequence);
    sessions.set("after", &"the snapshot".to_owned()).unwrap();
    Log::read(&home_log, |entry| shipped.apply(&entry, None)).unwrap();
    assert!(pages_seen(&home_dir.path().join("kv.db")) == pages_seen(&copy_dir.path().join("kv.db")));
    drop(shipped);
    let read = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(read.get("after").unwrap().as_deref(), Some("the snapshot"));
    assert_eq!(read.get("31").unwrap(), None, "a delete between two chunks");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_node_three_links_away_holds_the_home_s_files_and_follows_its_writes() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, home_address) = serving(dirs[0].path());
    write_through_every_engine(&home, 1);
    let between = node_with(
        dirs[1].path(),
        Node { from: vec![home_address], serve: Some("127.0.0.1:0".into()), ..Node::default() },
    );
    let between_address = bound(between.node().unwrap()).unwrap().to_string();
    let far = node_with(dirs[2].path(), Node { from: vec![between_address], ..Node::default() });

    let sessions = far.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("1/7").unwrap().as_deref(), Some("session 7 of round 1"));
    caught_up(&home, &between, &TAKEN);
    caught_up(&home, &far, &TAKEN);

    write_through_every_engine(&home, 2);
    eventually("the far node reads round 2", || sessions.get("2/1").unwrap().is_some());
    caught_up(&home, &far, &TAKEN);
    let docs = far.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    assert_eq!(docs.search("replicated page 7 round 2").run().unwrap().hits[0].id, "2-7");
    sessions.set("here", &"passed on".to_owned()).unwrap();
    let at_home = home.bucket::<String>("sessions").open().unwrap().get("here").unwrap();
    assert_eq!(at_home.as_deref(), Some("passed on"), "a write on a copy goes to its home");
    far.events("own").open().unwrap().append("started", &serde_json::json!({})).unwrap();
    assert!(far.dir().join("records.db").exists(), "a node keeps its own records");
    far.close().unwrap();
    between.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_link_cut_goes_on_from_its_position_and_a_node_reads_its_copy_while_cut() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..200 {
        sessions.set(format!("{i}"), &format!("first {i}")).unwrap();
    }
    let options = Node { from: vec![address.clone()], files: Some(vec!["kv.db".into()]), ..Node::default() };
    let copy = node_with(copy_dir.path(), options.clone());
    let read = copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    home.close().unwrap();

    assert_eq!(read.get("7").unwrap().as_deref(), Some("first 7"), "a copy reads while its home is gone");
    copy.close().unwrap();
    let copy = node_with(copy_dir.path(), options);
    let read = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(read.get("8").unwrap().as_deref(), Some("first 8"), "a copy opens with no link to its home");

    let home = node_with(home_dir.path(), Node { serve: Some(address), ..Node::default() });
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..50 {
        sessions.set(format!("{i}"), &format!("second {i}")).unwrap();
    }
    eventually("the copy takes what its home wrote after the cut", || {
        read.get("49").unwrap().as_deref() == Some("second 49")
    });
    caught_up(&home, &copy, &["kv.db"]);
    let snapshots = copy.node().unwrap().snapshots.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(snapshots, 0, "no snapshot again: the copy went on from its position");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_node_with_another_token_takes_nothing() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node_with(
        home_dir.path(),
        Node { serve: Some("127.0.0.1:0".into()), token: Some("right".into()), ..Node::default() },
    );
    home.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    let address = bound(home.node().unwrap()).unwrap().to_string();
    let wrong = node_with(
        copy_dir.path(),
        Node { from: vec![address.clone()], token: Some("wrong".into()), ..Node::default() },
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(copy_point(&wrong, "kv.db").is_none());
    wrong.close().unwrap();
    let right =
        node_with(copy_dir.path(), Node { from: vec![address], token: Some("right".into()), ..Node::default() });
    assert_eq!(right.bucket::<String>("sessions").open().unwrap().get("k").unwrap().as_deref(), Some("v"));
    right.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_write_on_a_copy_two_links_away_commits_at_its_home_and_reads_back_at_once() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, home_address) = serving(dirs[0].path());
    let notes = [("0001_notes.sql", "create table notes (id integer primary key, body text not null unique);")];
    let home_db = home.database("app").migrations(notes).open().unwrap();
    let home_sessions = home.bucket::<String>("sessions").open().unwrap();
    home_sessions.set("seed", &"from the home".to_owned()).unwrap();
    home.counters("views").open().unwrap().add("page", 1).unwrap();
    let between = node_with(
        dirs[1].path(),
        Node { from: vec![home_address], serve: Some("127.0.0.1:0".into()), ..Node::default() },
    );
    let between_address = bound(between.node().unwrap()).unwrap().to_string();
    let far = node_with(dirs[2].path(), Node { from: vec![between_address], ..Node::default() });

    let sessions = far.bucket::<String>("sessions").open().unwrap();
    sessions.set("written far", &"away".to_owned()).unwrap();
    assert_eq!(sessions.get("written far").unwrap().as_deref(), Some("away"), "a node reads its own write at once");
    assert_eq!(home_sessions.get("written far").unwrap().as_deref(), Some("away"), "the home committed it");
    assert!(sessions.create("once", &"first".to_owned()).unwrap());
    assert!(!sessions.create("once", &"second".to_owned()).unwrap(), "the home decides what is new");
    assert_eq!(sessions.take("once").unwrap().as_deref(), Some("first"));
    assert!(!sessions.delete("once").unwrap());
    let seen = sessions.entry("seed").unwrap().unwrap().version;
    home_sessions.set("seed", &"changed at the home".to_owned()).unwrap();
    let late = sessions.key("seed").if_version(seen).set(&"lost".to_owned()).unwrap_err();
    assert_eq!(late.kind(), crate::ErrorKind::Conflict, "{late}");

    let views = far.counters("views").open().unwrap();
    assert_eq!(views.add("page", 2).unwrap(), 3);
    assert_eq!(views.get("page").unwrap(), 3);

    let db = far.database("app").migrations(notes).open().unwrap();
    let done = db.exec(crate::sql!("insert into notes (body) values (?)", "far")).unwrap();
    assert_eq!(done.changes, 1);
    let read: Vec<String> = db.all(crate::sql!("select body from notes")).unwrap();
    assert_eq!(read, ["far"], "the copy holds the row the write made");
    let taken = db.exec(crate::sql!("insert into notes (body) values (?)", "far")).unwrap_err();
    assert_eq!(taken.kind(), crate::ErrorKind::Conflict, "the home's unique index: {taken}");
    db.batch([
        crate::sql!("insert into notes (body) values ('a')"),
        crate::sql!("insert into notes (body) values ('b')"),
    ])
    .unwrap();
    let count: i64 = home_db.scalar(crate::sql!("select count(*) from notes")).unwrap();
    assert_eq!(count, 3);
    caught_up(&home, &far, &["kv.db", "sql/app.db"]);
    far.close().unwrap();
    between.close().unwrap();
    home.close().unwrap();
}
