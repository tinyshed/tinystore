use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::copy::Copy;
use super::copy_tests::pages_seen;
use super::entry::Point;
use super::node_tests::write_through_every_engine;
use super::snapshot::{self, Part, Taking};
use super::*;
use crate::sqlite::{Config, File, sql_error};
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

/// How long the suite lets work on another thread or node take to reach a
/// point, on a machine as busy as a CI runner.
const WITHIN: Duration = Duration::from_secs(30);

fn eventually(what: &str, holds: impl FnMut() -> bool) {
    crate::testing::holds_within(WITHIN, what, holds);
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
fn a_node_asked_says_the_files_it_serves_and_an_address_with_no_node_says_none() {
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let (home, address) = serving(dirs[0].path());
    home.bucket::<String>("sessions").open().unwrap().set("42", &"x".to_owned()).unwrap();
    let served = served_by(dirs[1].path(), Node::default(), &address).unwrap().unwrap();
    assert!(served.contains(&"kv.db".to_owned()), "{served:?}");
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    assert_eq!(served_by(dirs[1].path(), Node::default(), &silent).unwrap(), None, "nothing listens there");
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
    let home_log = home_dir.path().join(DIR).join("kv.db");
    Log::read(&home_log, |entry| {
        if !taking.whole() {
            taking.apply(&entry)?;
        }
        Ok(())
    })
    .unwrap();
    assert!(taking.whole());
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
fn a_copy_whose_position_no_longer_reads_opens_and_is_taken_again_from_a_snapshot() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..50 {
        sessions.set(format!("{i}"), &format!("first {i}")).unwrap();
    }
    let options = Node { from: vec![address], files: Some(vec!["kv.db".into()]), ..Node::default() };
    let copy = node_with(copy_dir.path(), options.clone());
    caught_up(&home, &copy, &["kv.db"]);
    copy.close().unwrap();
    let position = copy_dir.path().join(DIR).join("kv.db").join(super::copy::Position::FILE);
    let mut kept = std::fs::read(&position).unwrap();
    kept[0] ^= 0xff;
    std::fs::write(&position, kept).unwrap();

    let copy = node_with(copy_dir.path(), options);
    sessions.set("after", &"the damage".to_owned()).unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    let read = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(read.get("after").unwrap().as_deref(), Some("the damage"));
    let snapshots = copy.node().unwrap().snapshots.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(snapshots, 1, "the copy was taken again whole");
    copy.close().unwrap();
    home.close().unwrap();
}

// contract: REPL-11
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

// contract: REPL-12
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
    let home_views = home.counters("views").open().unwrap();
    eventually("an add queued two links away reaches the home", || home_views.get("page").unwrap() == 3);

    let db = far.database("app").migrations(notes).open().unwrap();
    let done = db.exec(crate::sql!("insert into notes (body) values (?)", "far")).unwrap();
    assert_eq!(done.changes, 1);
    let read: Vec<String> = db.all(crate::sql!("select body from notes")).unwrap();
    assert_eq!(read, ["far"], "the copy holds the row the write made");
    let taken = db.exec(crate::sql!("insert into notes (body) values (?)", "far")).unwrap_err();
    assert_eq!(taken.kind(), crate::ErrorKind::Conflict, "the home's unique index: {taken}");
    let broken = taken.constraint().expect("the constraint the home named");
    assert_eq!((broken.kind, broken.columns), (crate::ConstraintKind::Unique, vec!["body".to_owned()]));
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

#[test]
fn a_document_set_on_a_copy_is_indexed_at_its_home_and_found_on_the_copy() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let home_docs = home.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    home_docs.set("seed", &serde_json::json!({ "title": "planted at the home" })).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let docs = copy.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    assert_eq!(docs.search("planted").run().unwrap().hits[0].id, "seed");

    docs.set("far", &serde_json::json!({ "title": "written on a copy" })).unwrap();
    assert_eq!(docs.search("copy").run().unwrap().hits[0].id, "far", "the copy finds its own set at once");
    assert_eq!(home_docs.search("copy").run().unwrap().hits[0].id, "far");
    let many = [
        ("a", serde_json::json!({ "title": "first of many" })),
        ("b", serde_json::json!({ "title": "second of many" })),
    ];
    docs.set_many(many.iter().map(|(id, value)| (*id, value))).unwrap();
    assert_eq!(docs.search("many").run().unwrap().hits.len(), 2);
    assert!(docs.delete("far").unwrap());
    assert!(home_docs.get("far").unwrap().is_none());
    assert!(!docs.delete("far").unwrap());
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_job_added_on_a_copy_runs_at_its_home() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.bucket::<String>("warm").open().unwrap().set("up", &"kv.db made".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let mail = copy.queue::<serde_json::Value>("mail").open().unwrap();
    assert!(mail.id("welcome-7").add(&serde_json::json!({ "to": 7 })).unwrap());
    assert!(!mail.id("welcome-7").add(&serde_json::json!({ "to": 7 })).unwrap(), "the home keeps its id");
    assert!(mail.get("welcome-7").unwrap().is_some(), "the copy holds the job its home added");
    assert!(mail.update("welcome-7", &serde_json::json!({ "to": 8 })).unwrap());
    assert!(mail.add(&serde_json::json!({ "to": 9 })).unwrap());

    let home_mail = home.queue::<serde_json::Value>("mail").open().unwrap();
    let seen = std::sync::Mutex::new(Vec::new());
    let ran = home_mail
        .run_due(|job: serde_json::Value, _: &crate::jobs::Run| {
            seen.lock().unwrap().push(job["to"].as_i64().unwrap());
            Ok::<_, String>(())
        })
        .unwrap();
    assert_eq!(ran, 2);
    let mut seen = seen.into_inner().unwrap();
    seen.sort_unstable();
    assert_eq!(seen, [8, 9]);
    assert!(mail.id("later").delay(Duration::from_secs(3600)).add(&serde_json::json!({})).unwrap());
    assert!(mail.cancel("later").unwrap());
    assert!(home_mail.get("later").unwrap().is_none());
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_file_too_large_to_keep_inline_reaches_a_copy_of_a_copy() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, home_address) = serving(dirs[0].path());
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let files = home.files("docs").open().unwrap();
    files.put("film.bin", &big).unwrap();
    files.put("note.txt", b"small").unwrap();
    let between = node_with(
        dirs[1].path(),
        Node { from: vec![home_address], serve: Some("127.0.0.1:0".into()), ..Node::default() },
    );
    let between_address = bound(between.node().unwrap()).unwrap().to_string();
    let far = node_with(dirs[2].path(), Node { from: vec![between_address], ..Node::default() });
    let far_files = far.files("docs").open().unwrap();
    assert_eq!(far_files.get("note.txt").unwrap().unwrap().read_all().unwrap(), b"small");
    eventually("the film's bytes reach the far node", || {
        far_files.get("film.bin").ok().flatten().and_then(|mut file| file.read_all().ok()).is_some()
    });
    assert!(far_files.get("film.bin").unwrap().unwrap().read_all().unwrap() == big, "byte for byte");

    files.put("second.bin", vec![7u8; 2_000_000]).unwrap();
    eventually("a file put after the copy began reaches it", || {
        far_files.get("second.bin").ok().flatten().and_then(|mut file| file.read_all().ok()).is_some()
    });
    far.close().unwrap();
    between.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_promoted_once_its_home_stopped_is_the_home_the_nodes_behind_it_go_on_from() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, home_address) = serving(dirs[0].path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..50 {
        sessions.set(format!("{i}"), &format!("from the first home {i}")).unwrap();
    }
    let europe_node = |from: Vec<String>, serve: String| Node { from, serve: Some(serve), ..Node::default() };
    let europe = node_with(dirs[1].path(), europe_node(vec![home_address.clone()], "127.0.0.1:0".into()));
    let europe_address = bound(europe.node().unwrap()).unwrap().to_string();
    let russia =
        node_with(dirs[2].path(), Node { from: vec![home_address.clone(), europe_address.clone()], ..Node::default() });
    let read = russia.bucket::<String>("sessions").open().unwrap();
    europe.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &europe, &["kv.db"]);
    caught_up(&home, &russia, &["kv.db"]);
    let before = copy_point(&russia, "kv.db").unwrap();

    home.close().unwrap();
    // a stopped home holds nothing open: its handles go with its process
    drop(sessions);
    europe.close().unwrap();
    let refused = promote(dirs[0].path(), &["kv.db".to_owned()]).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid, "a home's own file is no copy: {refused}");
    let promoted = promote(dirs[1].path(), &[]).unwrap();
    assert_eq!(promoted, [Promoted { file: "kv.db".into(), sequence: before.sequence, epoch: before.epoch + 1 }]);
    let europe = node_with(dirs[1].path(), europe_node(Vec::new(), europe_address));
    let written = europe.bucket::<String>("sessions").open().unwrap();
    written.set("after", &"the new home".to_owned()).unwrap();

    eventually("russia takes the new home's write", || read.get("after").unwrap().as_deref() == Some("the new home"));
    let snapshots = russia.node().unwrap().snapshots.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(snapshots, 1, "the first copy alone: russia went on from its position at the new home");
    assert_eq!(copy_point(&russia, "kv.db").unwrap().epoch, before.epoch + 1);
    read.set("from russia", &"to the new home".to_owned()).unwrap();
    assert_eq!(written.get("from russia").unwrap().as_deref(), Some("to the new home"));
    caught_up(&europe, &russia, &["kv.db"]);

    // the old home comes back as a copy of the new one
    let demoted = demote(dirs[0].path(), &[]).unwrap();
    assert_eq!(demoted.len(), 1);
    assert_eq!(demoted[0].file, "kv.db");
    let europe_address = bound(europe.node().unwrap()).unwrap().to_string();
    let old = node_with(dirs[0].path(), Node { from: vec![europe_address], ..Node::default() });
    let old_read = old.bucket::<String>("sessions").open().unwrap();
    assert_eq!(old_read.get("from russia").unwrap().as_deref(), Some("to the new home"));
    old_read.set("from the old home", &"now a copy".to_owned()).unwrap();
    assert_eq!(written.get("from the old home").unwrap().as_deref(), Some("now a copy"));
    old.close().unwrap();
    russia.close().unwrap();
    europe.close().unwrap();
}

// contract: REPL-08
#[test]
fn an_old_home_back_without_its_demotion_hands_no_node_of_a_later_epoch_its_file() {
    use std::sync::atomic::Ordering::Relaxed;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, home_address) = serving(dirs[0].path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    sessions.set("first", &"from the first home".to_owned()).unwrap();
    let europe_node = |from: Vec<String>, serve: String| Node { from, serve: Some(serve), ..Node::default() };
    let europe = node_with(dirs[1].path(), europe_node(vec![home_address.clone()], "127.0.0.1:0".into()));
    let europe_address = bound(europe.node().unwrap()).unwrap().to_string();
    let russia_node = Node { from: vec![home_address.clone(), europe_address.clone()], ..Node::default() };
    let russia = node_with(dirs[2].path(), russia_node.clone());
    let read = russia.bucket::<String>("sessions").open().unwrap();
    europe.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &europe, &["kv.db"]);
    // europe stops while russia takes on: what russia holds past europe's point is lost
    europe.close().unwrap();
    for i in 0..3 {
        sessions.set(format!("late {i}"), &"europe never had it".to_owned()).unwrap();
    }
    caught_up(&home, &russia, &["kv.db"]);
    home.close().unwrap();
    drop(sessions);

    promote(dirs[1].path(), &[]).unwrap();
    let europe = node_with(dirs[1].path(), europe_node(Vec::new(), europe_address));
    let written = europe.bucket::<String>("sessions").open().unwrap();
    written.set("after", &"the new home".to_owned()).unwrap();
    eventually("russia takes the new home's file", || read.get("after").unwrap().as_deref() == Some("the new home"));
    assert_eq!(read.get("late 0").unwrap(), None, "what europe never had is gone from russia");
    assert_eq!(europe.node().unwrap().lost.load(Relaxed), 3, "the new home counts the commits russia lost");
    drop(read);
    russia.close().unwrap();

    // the old home comes back as a home, never demoted, and writes on
    let old = node_with(dirs[0].path(), Node { serve: Some(home_address), ..Node::default() });
    old.bucket::<String>("sessions").open().unwrap().set("split", &"the old home wrote on".to_owned()).unwrap();
    let russia = node_with(dirs[2].path(), russia_node);
    let read = russia.bucket::<String>("sessions").open().unwrap();
    written.set("later", &"the new home again".to_owned()).unwrap();
    eventually("russia goes on from the new home", || {
        read.get("later").unwrap().as_deref() == Some("the new home again")
    });
    assert_eq!(read.get("split").unwrap(), None, "no snapshot of the old home's file reached russia");
    assert_eq!(russia.node().unwrap().snapshots.load(Relaxed), 0, "russia went on from its position");
    let refused = old.bucket::<String>("sessions").open().unwrap().set("again", &"after".to_owned()).unwrap_err();
    assert_eq!(
        refused.kind(),
        crate::ErrorKind::Conflict,
        "an old home a later epoch reached writes no more: {refused}"
    );
    let fenced = |dir: &Path| status(dir).unwrap().into_iter().find(|file| file.file == "kv.db").unwrap().fenced;
    russia.close().unwrap();
    old.close().unwrap();
    assert_eq!(fenced(dirs[0].path()), Some(2), "its directory keeps the epoch it learned");

    let old = node_with(dirs[0].path(), Node::default());
    let refused = old.bucket::<String>("sessions").open().unwrap().set("again", &"restarted".to_owned()).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Conflict, "and refuses writes across a restart: {refused}");
    old.close().unwrap();
    demote(dirs[0].path(), &[]).unwrap();
    assert_eq!(fenced(dirs[0].path()), None, "a demotion makes it a copy, which writes nothing anyway");
    europe.close().unwrap();
}

#[test]
fn two_nodes_write_their_own_databases_and_copy_each_other_s() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let ports =
        [std::net::TcpListener::bind("127.0.0.1:0").unwrap(), std::net::TcpListener::bind("127.0.0.1:0").unwrap()];
    let [us_address, eu_address] = ports.map(|port| port.local_addr().unwrap().to_string());
    let node = |serve: &str, from: &str, copies: &str| Node {
        serve: Some(serve.to_owned()),
        from: vec![from.to_owned()],
        files: Some(vec![copies.to_owned()]),
        ..Node::default()
    };
    let us = node_with(us_dir.path(), node(&us_address, &eu_address, "sql/eu.db"));
    let eu = node_with(eu_dir.path(), node(&eu_address, &us_address, "sql/us.db"));
    let notes = [("0001_notes.sql", "create table notes (id integer primary key, body text not null);")];
    let us_db = us.database("us").migrations(notes).open().unwrap();
    let eu_db = eu.database("eu").migrations(notes).open().unwrap();
    us_db.exec(crate::sql!("insert into notes (body) values ('an american row')")).unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('a european row')")).unwrap();

    let us_reads_eu = us.database("eu").migrations(notes).open().unwrap();
    let eu_reads_us = eu.database("us").migrations(notes).open().unwrap();
    let bodies =
        |db: &crate::sql::Database| db.all::<String>(crate::sql!("select body from notes order by id")).unwrap();
    eventually("each node reads the other's rows", || {
        bodies(&us_reads_eu) == ["a european row"] && bodies(&eu_reads_us) == ["an american row"]
    });
    us_reads_eu.exec(crate::sql!("insert into notes (body) values ('written in america')")).unwrap();
    assert_eq!(bodies(&eu_db), ["a european row", "written in america"], "a write goes to the file's home");
    assert_eq!(bodies(&us_reads_eu), ["a european row", "written in america"], "and reads back where it was made");
    assert!(us.node().unwrap().copy("sql/us.db").is_none(), "a node copies none of its own files");
    eu.close().unwrap();
    us.close().unwrap();
}

// contract: REPL-09
#[test]
fn a_synchronous_home_answers_a_write_once_a_copy_holds_it() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home =
        node_with(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), synchronous: true, ..Node::default() });
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..20 {
        // each timed alone: a commit that waited for a copy would take the whole wait
        let alone = Instant::now();
        let unheld = sessions.set(format!("alone {i}"), &"no copy follows".to_owned()).unwrap_err();
        assert_eq!(unheld.kind(), crate::ErrorKind::CommittedLocally, "{unheld}");
        assert!(alone.elapsed() < super::log::ACK_WAIT, "with no copy following, a commit waits for none");
    }
    assert_eq!(sessions.get("alone 19").unwrap().as_deref(), Some("no copy follows"), "a commit no copy held stands");
    let address = bound(home.node().unwrap()).unwrap().to_string();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    for i in 0..50 {
        sessions.set(format!("held {i}"), &"by the copy before it answered".to_owned()).unwrap();
        let held = copy_point(&copy, "kv.db").unwrap().sequence;
        assert!(held >= home_point(&home, "kv.db").sequence, "write {i} answered before a copy held it");
    }
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_synchronous_home_with_no_copy_does_its_own_work_and_says_of_each_write_that_no_copy_holds_it() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let home = node_with(dir.path(), Node { serve: Some("127.0.0.1:0".into()), synchronous: true, ..Node::default() });
    // each engine opens, its migrations run and its names are made, with no copy to hold them
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    let mail = home.queue::<serde_json::Value>("mail").open().unwrap();
    let notes = [("0001_notes.sql", "create table notes (id integer primary key, body text not null);")];
    let db = home.database("app").migrations(notes).open().unwrap();
    let docs = home.index::<serde_json::Value>("docs").field("title", 1.0).open().unwrap();
    let files = home.files("docs").open().unwrap();

    let no_copy_held = |error: crate::Error| assert_eq!(error.kind(), crate::ErrorKind::CommittedLocally, "{error}");
    no_copy_held(sessions.set("k", &"v".to_owned()).unwrap_err());
    no_copy_held(mail.add(&json!({ "to": 1 })).unwrap_err());
    no_copy_held(db.exec(crate::sql!("insert into notes (body) values (?)", "grouped")).unwrap_err());
    let in_a_transaction = db.tx(|tx| tx.exec(crate::sql!("insert into notes (body) values (?)", "alone")));
    no_copy_held(in_a_transaction.unwrap_err());
    no_copy_held(docs.set("d", &json!({ "title": "a page" })).unwrap_err());
    no_copy_held(files.put("a.txt", vec![b'x'; 10]).unwrap_err());

    // and each stands
    assert_eq!(sessions.get("k").unwrap().as_deref(), Some("v"));
    let bodies: Vec<String> = db.all(crate::sql!("select body from notes order by id")).unwrap();
    assert_eq!(bodies, ["grouped", "alone"]);
    assert!(docs.get("d").unwrap().is_some());
    assert!(files.get("a.txt").unwrap().is_some());
    // the store's own work goes on
    crate::kv::maintain(&home).unwrap();
    crate::jobs::maintain(&home).unwrap();
    crate::search::maintain(&home).unwrap();
    crate::blobs::maintain(&home).unwrap();
    home.close().unwrap();
}

#[test]
fn a_node_s_status_says_which_files_it_writes_and_where_its_copies_stand() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address.clone()], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    home.bucket::<String>("sessions").open().unwrap().set("k2", &"v".to_owned()).unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    let last = home_point(&home, "kv.db");

    let at_home = status(home_dir.path()).unwrap();
    assert_eq!(at_home.len(), 1);
    assert!(at_home[0].home && at_home[0].applied.is_none());
    assert_eq!(at_home[0].entries, Some((1, last.sequence)));
    let at_copy = status(copy_dir.path()).unwrap();
    assert_eq!(at_copy.len(), 1);
    assert!(!at_copy[0].home && !at_copy[0].taking);
    assert_eq!(at_copy[0].applied, Some(last.sequence));
    assert_eq!(at_copy[0].entries, Some((last.sequence, last.sequence)), "what it received since its snapshot");

    eventually("a running home says who follows it and what each holds", || {
        let links = links(home_dir.path()).unwrap().unwrap_or_default();
        links.followers.len() == 1 && links.followers[0].applied.get("kv.db") == Some(&last.sequence)
    });
    eventually("a running copy says whom it takes from", || {
        links(copy_dir.path()).unwrap().is_some_and(|links| links.from.as_deref() == Some(address.as_str()))
    });
    copy.close().unwrap();
    home.close().unwrap();
    assert_eq!(links(home_dir.path()).unwrap(), None, "a node that stopped says no links");
}

#[test]
fn a_file_written_on_a_copy_is_kept_at_its_home_and_reads_back_on_the_copy() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let home_files = home.files("docs").open().unwrap();
    home_files.put("seed.txt", b"planted").unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let files = copy.files("docs").open().unwrap();
    let read =
        |files: &crate::blobs::Files, path: &str| files.get(path).unwrap().map(|mut file| file.read_all().unwrap());

    let info = files.key("small.txt").content_type("text/plain").meta("by", "the copy").put(b"small").unwrap();
    assert_eq!((info.size, info.content_type.as_str()), (5, "text/plain"));
    assert_eq!(read(&files, "small.txt").as_deref(), Some(&b"small"[..]), "the copy reads its own write at once");
    assert_eq!(read(&home_files, "small.txt").as_deref(), Some(&b"small"[..]));
    assert_eq!(home_files.head("small.txt").unwrap().unwrap().meta["by"], "the copy");

    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 249) as u8).collect();
    files.put("big.bin", &big).unwrap();
    assert!(read(&home_files, "big.bin").unwrap() == big, "the home keeps the big file byte for byte");
    eventually("the copy fetches the big file's bytes", || {
        files.get("big.bin").ok().flatten().and_then(|mut file| file.read_all().ok()).is_some_and(|bytes| bytes == big)
    });

    assert!(files.create("new.txt", b"first").unwrap());
    assert!(!files.create("new.txt", b"second").unwrap(), "the home decides what is new");
    files.copy("small.txt", "copied.txt").unwrap();
    files.rename("copied.txt", "renamed.txt").unwrap();
    assert!(read(&home_files, "copied.txt").is_none());
    assert_eq!(read(&home_files, "renamed.txt").as_deref(), Some(&b"small"[..]));
    let stale = info.etag.clone();
    files.put("small.txt", b"changed").unwrap();
    let refused = files.key("small.txt").if_match(stale).delete().unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Conflict, "{refused}");
    files.delete("small.txt").unwrap();
    assert!(read(&home_files, "small.txt").is_none() && read(&files, "small.txt").is_none());
    assert!(files.expire("new.txt", Duration::from_secs(60)).unwrap());
    assert!(home_files.head("new.txt").unwrap().unwrap().expires.is_some());
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn an_encrypted_bucket_s_value_written_on_a_copy_crosses_the_link_sealed() {
    let (home_dir, copy_dir, keys) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (key, other) = (keys.path().join("one.key"), keys.path().join("other.key"));
    std::fs::write(&key, "3fa9".repeat(16)).unwrap();
    std::fs::write(&other, "81c2".repeat(16)).unwrap();
    let keyed = |dir: &Path, key: &Path, node: Node| {
        let options = Options {
            node: Some(node),
            background: false,
            encryption_key_file: Some(key.to_owned()),
            ..Options::default()
        };
        Store::open(dir, options).unwrap()
    };
    let home = keyed(home_dir.path(), &key, Node { serve: Some("127.0.0.1:0".into()), ..Node::default() });
    let address = bound(home.node().unwrap()).unwrap().to_string();
    let home_secrets = home.bucket::<String>("secrets").encrypted().open().unwrap();
    home_secrets.set("planted", &"at home".to_owned()).unwrap();
    let copy = keyed(copy_dir.path(), &key, Node { from: vec![address.clone()], ..Node::default() });
    let secrets = copy.bucket::<String>("secrets").encrypted().open().unwrap();
    assert_eq!(secrets.get("planted").unwrap().as_deref(), Some("at home"));

    secrets.set("token", &"hunter2".to_owned()).unwrap();
    assert_eq!(secrets.get("token").unwrap().as_deref(), Some("hunter2"), "the copy opens its own write");
    assert_eq!(home_secrets.get("token").unwrap().as_deref(), Some("hunter2"), "the home opens it with the same key");
    assert!(!secrets.create("token", &"again".to_owned()).unwrap());
    assert_eq!(secrets.take("token").unwrap().as_deref(), Some("hunter2"), "a take's value comes back sealed");
    assert!(home_secrets.get("token").unwrap().is_none(), "the home took it");
    assert_eq!(secrets.take("token").unwrap(), None);
    secrets.set("token", &"again".to_owned()).unwrap();
    assert!(secrets.delete("token").unwrap());
    assert!(home_secrets.get("token").unwrap().is_none());
    copy.close().unwrap();

    let wrong_dir = tempfile::tempdir().unwrap();
    let wrong = keyed(wrong_dir.path(), &other, Node { from: vec![address], ..Node::default() });
    let mismatched = wrong.bucket::<String>("secrets").encrypted().open();
    if let Ok(mismatched) = mismatched {
        let refused = mismatched.set("token", &"under another key".to_owned()).unwrap_err();
        assert_eq!(refused.kind(), crate::ErrorKind::Invalid, "{refused}");
    }
    assert!(home_secrets.get("token").unwrap().is_none(), "nothing sealed with another key reaches the home");
    wrong.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_s_rate_limits_and_quotas_are_decided_at_their_home() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.bucket::<String>("warm").open().unwrap().set("up", &"kv.db made".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });

    let hour = Duration::from_secs(3600);
    let limit = copy.rate_limit("api").rate(3, hour).open().unwrap();
    let home_limit = home.rate_limit("api").rate(3, hour).open().unwrap();
    assert!(limit.allow("user").unwrap().ok);
    assert!(home_limit.allow("user").unwrap().ok, "the home counts the copy's request");
    assert!(limit.allow("user").unwrap().ok);
    let refused = limit.allow("user").unwrap();
    assert!(!refused.ok && refused.retry_at.is_some(), "the fourth of three an hour waits, wherever it is asked");
    limit.reset("user").unwrap();
    assert!(home_limit.allow("user").unwrap().ok);

    let quota = copy.quota("ai").window("daily", 2, hour * 24).open().unwrap();
    assert!(quota.allow("user").unwrap().ok);
    assert!(quota.allow("user").unwrap().ok);
    let full = quota.allow("user").unwrap();
    assert!(!full.ok);
    assert_eq!(full.window("daily").unwrap().used, 2);
    quota.refund("user").unwrap();
    let home_quota = home.quota("ai").window("daily", 2, hour * 24).open().unwrap();
    assert_eq!(home_quota.peek("user").unwrap().window("daily").unwrap().used, 1, "the home holds the refund");
    copy.close().unwrap();
    home.close().unwrap();
}

// contract: REPL-06
#[test]
fn a_bucket_and_a_queue_kept_in_a_database_s_file_pass_a_copy_s_writes_to_its_home() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let notes = [("0001_notes.sql", "create table notes (id integer primary key, body text not null);")];
    let home_db = home.database("tenant").migrations(notes).open().unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let db = copy.database("tenant").migrations(notes).open().unwrap();

    let prefs = db.bucket::<String>("prefs").open().unwrap();
    prefs.set("theme", &"dark".to_owned()).unwrap();
    assert_eq!(prefs.get("theme").unwrap().as_deref(), Some("dark"), "the copy reads its own write at once");
    let home_prefs = home_db.bucket::<String>("prefs").open().unwrap();
    assert_eq!(home_prefs.get("theme").unwrap().as_deref(), Some("dark"), "kept in the tenant's file at its home");
    assert!(!home_dir.path().join("kv.db").exists(), "nothing went to kv.db");
    caught_up(&home, &copy, &["sql/tenant.db"]);

    let tasks = db.queue::<serde_json::Value>("tasks").open().unwrap();
    assert!(tasks.id("t1").add(&serde_json::json!({ "do": "it" })).unwrap());
    assert!(tasks.get("t1").unwrap().is_some());
    let home_tasks = home_db.queue::<serde_json::Value>("tasks").open().unwrap();
    assert!(home_tasks.get("t1").unwrap().is_some(), "the job is in the tenant's file at its home");

    // a transaction is code deciding on the writer, which a copy has not
    let refused = db.tx(|tx| tx.exec(crate::sql!("insert into notes (body) values ('in a tx')"))).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "{refused}");
    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    let refused = copy.tx(|tx| -> crate::Result<()> { tx.with(&sessions).set("k", &"v".to_owned()) }).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "{refused}");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_schedule_a_copy_opens_is_kept_and_run_at_its_home() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.queue::<serde_json::Value>("warm").open().unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let hour = Duration::from_secs(3600);
    let report = copy.schedule("report").every(hour).open().unwrap();
    let job = report.get().unwrap().expect("the copy holds the job its home keeps");
    assert!(job.at > std::time::SystemTime::now(), "due within the next hour");
    let home_report = home.schedule("report").every(hour).open().unwrap();
    assert!(home_report.get().unwrap().is_some(), "its home keeps it");
    let refused = copy.schedule("report").every(hour).work(|_: &crate::jobs::Run| Ok::<_, String>(())).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "a copy's worker takes no job: {refused}");
    assert!(report.cancel().unwrap());
    assert!(home_report.get().unwrap().is_none(), "the cancel reached its home");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn once_on_two_copies_runs_a_key_s_function_once_across_the_nodes() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, address) = serving(dirs[0].path());
    home.once::<i64>("charges").open().unwrap();
    let first = node_with(dirs[1].path(), Node { from: vec![address.clone()], ..Node::default() });
    let second = node_with(dirs[2].path(), Node { from: vec![address], ..Node::default() });
    let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let runs: Vec<_> = [first.clone(), second.clone()]
        .into_iter()
        .enumerate()
        .map(|(i, node)| {
            let ran = Arc::clone(&ran);
            std::thread::spawn(move || {
                let charges = node.once::<i64>("charges").open().unwrap();
                charges
                    .run("charge-1", || {
                        ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(300));
                        Ok::<_, crate::Error>(10 + i as i64)
                    })
                    .unwrap()
            })
        })
        .collect();
    let answers: Vec<i64> = runs.into_iter().map(|run| run.join().unwrap()).collect();
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1, "the function ran once across both copies");
    assert_eq!(answers[0], answers[1], "both callers got the one answer");
    assert_eq!(home.once::<i64>("charges").open().unwrap().get("charge-1").unwrap(), Some(answers[0]));

    let charges = first.once::<i64>("charges").open().unwrap();
    let failed = charges.run("charge-2", || Err::<i64, _>(crate::Error::invalid("declined")));
    assert!(failed.is_err());
    assert_eq!(charges.run("charge-2", || Ok::<_, crate::Error>(7)).unwrap(), 7, "a failed run keeps nothing");
    second.close().unwrap();
    first.close().unwrap();
    home.close().unwrap();
}

#[cfg(feature = "replication-tls")]
#[test]
fn nodes_linked_through_tls_copy_and_pass_writes_on_and_a_stranger_s_certificate_links_none() {
    let (home_dir, copy_dir, certs) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let write_pair = |name: &str| {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let (cert, key) = (certs.path().join(format!("{name}.pem")), certs.path().join(format!("{name}.key")));
        std::fs::write(&cert, certified.cert.pem()).unwrap();
        std::fs::write(&key, certified.signing_key.serialize_pem()).unwrap();
        (cert, key)
    };
    let (cert, key) = write_pair("home");
    let (stranger, _) = write_pair("stranger");
    let tls = NodeTls { cert: Some(cert.clone()), key: Some(key), roots: None };
    let home =
        node_with(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), tls: Some(tls), ..Node::default() });
    let port = bound(home.node().unwrap()).unwrap().port();
    home.bucket::<String>("sessions").open().unwrap().set("seed", &"over tls".to_owned()).unwrap();

    let trusting = NodeTls { roots: Some(cert), ..NodeTls::default() };
    let from = vec![format!("localhost:{port}")];
    let copy = node_with(copy_dir.path(), Node { from: from.clone(), tls: Some(trusting), ..Node::default() });
    let sessions = copy.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("seed").unwrap().as_deref(), Some("over tls"));
    sessions.set("back", &"through tls".to_owned()).unwrap();
    assert_eq!(home.bucket::<String>("sessions").open().unwrap().get("back").unwrap().as_deref(), Some("through tls"));
    copy.close().unwrap();

    let other_dir = tempfile::tempdir().unwrap();
    let wary = NodeTls { roots: Some(stranger), ..NodeTls::default() };
    let other = node_with(other_dir.path(), Node { from, tls: Some(wary), ..Node::default() });
    std::thread::sleep(Duration::from_secs(1));
    assert!(copy_point(&other, "kv.db").is_none(), "a certificate it does not trust links nothing");
    other.close().unwrap();
    home.close().unwrap();
}

// contract: REPL-12
#[test]
fn a_counter_s_adds_on_a_copy_answer_at_once_and_reach_its_home_each_once() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.counters("views").open().unwrap().add("seed", 1).unwrap();
    let options = Node { from: vec![address.clone()], ..Node::default() };
    let copy = node_with(copy_dir.path(), options.clone());
    let views = copy.counters("views").open().unwrap();
    for i in 1..=50 {
        assert_eq!(views.add("page", 1).unwrap(), i, "the copy answers its own count at once");
    }
    assert_eq!(views.get("page").unwrap(), 50, "what the queue holds counts on the copy before its home has it");
    let home_views = home.counters("views").open().unwrap();
    eventually("the home counts every queued add", || home_views.get("page").unwrap() == 50);
    eventually("the copy holds them from its home, none counted twice", || {
        views.get("page").unwrap() == 50 && copy.node().unwrap().queue().unwrap().is_empty().unwrap()
    });

    home.close().unwrap();
    for _ in 0..3 {
        views.add("page", 1).unwrap();
    }
    assert_eq!(views.get("page").unwrap(), 53, "the copy counts on while its home is gone");
    drop(views);
    copy.close().unwrap();
    let home = node_with(home_dir.path(), Node { serve: Some(address), ..Node::default() });
    let copy = node_with(copy_dir.path(), options);
    let home_views = home.counters("views").open().unwrap();
    eventually("the adds queued before the copy closed reach the home once it opens again", || {
        home_views.get("page").unwrap() == 53
    });

    home_views.add_queued("dup", 5, "a node", 7).unwrap();
    home_views.add_queued("dup", 5, "a node", 7).unwrap();
    home_views.add_queued("dup", 5, "a node", 6).unwrap();
    assert_eq!(home_views.get("dup").unwrap(), 5, "an op sent twice, or behind the last, counts once");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_promoted_with_adds_still_queued_counts_them_itself_once() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.counters("views").open().unwrap().add("page", 1).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let views = copy.counters("views").open().unwrap();
    eventually("the copy holds its home's count", || views.get("page").unwrap() == 1);
    home.close().unwrap();
    assert_eq!(views.add("page", 2).unwrap(), 3);
    drop(views);
    copy.close().unwrap();

    crate::replication::promote(copy_dir.path(), &[]).unwrap();
    let promoted = node_with(copy_dir.path(), Node::default());
    let views = promoted.counters("views").open().unwrap();
    let queue = promoted.node().unwrap().queue().unwrap();
    eventually("the promoted copy counts what it had queued, and lets it go", || {
        views.get("page").unwrap() == 3 && queue.is_empty().unwrap()
    });
    assert_eq!(views.get("page").unwrap(), 3, "counted once");
    promoted.close().unwrap();
}

/// Two nodes each taking from the other, America the catalog's home, with
/// no list of the files either copies.
fn meshed(us_dir: &Path, eu_dir: &Path) -> (Store, Store) {
    let ports =
        [std::net::TcpListener::bind("127.0.0.1:0").unwrap(), std::net::TcpListener::bind("127.0.0.1:0").unwrap()];
    let [us_address, eu_address] = ports.map(|port| port.local_addr().unwrap().to_string());
    let node = |serve: &str, from: &str, catalog: bool| Node {
        serve: Some(serve.to_owned()),
        from: vec![from.to_owned()],
        catalog,
        ..Node::default()
    };
    let us = node_with(us_dir, node(&us_address, &eu_address, true));
    let eu = node_with(eu_dir, node(&eu_address, &us_address, false));
    (us, eu)
}

const NOTES: [(&str, &str); 1] =
    [("0001_notes.sql", "create table notes (id integer primary key, body text not null);")];

fn bodies(db: &crate::sql::Database) -> Vec<String> {
    db.all::<String>(crate::sql!("select body from notes order by id")).unwrap()
}

#[test]
fn nodes_with_a_catalog_write_the_files_they_made_and_copy_the_others_with_no_list() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (us, eu) = meshed(us_dir.path(), eu_dir.path());
    let sessions = us.bucket::<String>("sessions").open().unwrap();
    sessions.set("seed", &"made in america".to_owned()).unwrap();
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('a european row')")).unwrap();
    assert!(eu.node().unwrap().copy("sql/users.db").is_none(), "europe made sql/users.db, and writes it");

    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    assert!(us.node().unwrap().copy("sql/users.db").is_some(), "america copies what europe made");
    eventually("america reads europe's row", || bodies(&us_db) == ["a european row"]);
    us_db.exec(crate::sql!("insert into notes (body) values ('written in america')")).unwrap();
    assert_eq!(bodies(&eu_db), ["a european row", "written in america"], "a write goes to the file's home");
    let eu_sessions = eu.bucket::<String>("sessions").open().unwrap();
    assert!(eu.node().unwrap().copy("kv.db").is_some(), "europe copies the kv.db america made");
    assert_eq!(eu_sessions.get("seed").unwrap().as_deref(), Some("made in america"));

    let (us_node, eu_node) = (us.node().unwrap(), eu.node().unwrap());
    let catalog = us_node.catalog(&us).unwrap();
    assert_eq!(catalog.home_of("sql/users.db").unwrap().unwrap().node, eu_node.id);
    assert_eq!(catalog.home_of("kv.db").unwrap().unwrap().node, us_node.id);

    // two nodes making one file in the same moment: the catalog keeps one claim
    let both = |store: Store| {
        std::thread::spawn(move || {
            let db = store.database("both").migrations(NOTES).open().unwrap();
            let home = store.node().unwrap().copy("sql/both.db").is_none();
            db.exec(crate::sql!("insert into notes (body) values ('one')")).unwrap();
            home
        })
    };
    let (in_america, in_europe) = (both(us.clone()), both(eu.clone()));
    let homes = [in_america.join().unwrap(), in_europe.join().unwrap()];
    assert_eq!(homes.iter().filter(|home| **home).count(), 1, "one node writes sql/both.db: {homes:?}");
    let us_both = us.database("both").migrations(NOTES).open().unwrap();
    let eu_both = eu.database("both").migrations(NOTES).open().unwrap();
    eventually("each node holds both writes", || bodies(&us_both).len() == 2 && bodies(&eu_both).len() == 2);
    eu.close().unwrap();
    us.close().unwrap();
}

#[test]
fn a_node_cut_off_from_the_catalog_s_home_writes_its_files_and_makes_no_new_one() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (us, eu) = meshed(us_dir.path(), eu_dir.path());
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    us.close().unwrap();

    eu_db.exec(crate::sql!("insert into notes (body) values ('while america is gone')")).unwrap();
    assert_eq!(bodies(&eu_db), ["while america is gone"], "europe writes the file it is home of");
    let refused = eu.database("later").migrations(NOTES).open().unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "a new file waits for the catalog: {refused}");
    eu.close().unwrap();
}

#[test]
fn a_node_takes_a_file_its_from_does_not_serve_straight_from_its_home_and_writes_it_there() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let catalog = |serve: String| Node { serve: Some(serve), catalog: true, ..Node::default() };
    let us = node_with(us_dir.path(), catalog("127.0.0.1:0".into()));
    let us_address = bound(us.node().unwrap()).unwrap().to_string();
    let eu_node = Node { serve: Some("127.0.0.1:0".into()), from: vec![us_address.clone()], ..Node::default() };
    let eu = node_with(eu_dir.path(), eu_node);
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('a european row')")).unwrap();

    // america takes from no node: the catalog names europe, which it reaches straight
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    assert!(us.node().unwrap().copy("sql/users.db").is_some(), "america copies sql/users.db from europe");
    eventually("america reads europe's row", || bodies(&us_db) == ["a european row"]);
    us_db.exec(crate::sql!("insert into notes (body) values ('written in america')")).unwrap();
    assert_eq!(bodies(&eu_db), ["a european row", "written in america"], "the write went to europe");
    drop(us_db);
    us.close().unwrap();

    let us = node_with(us_dir.path(), catalog(us_address));
    eu_db.exec(crate::sql!("insert into notes (body) values ('while america was away')")).unwrap();
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america's direct link is made again at its start", || bodies(&us_db).len() == 3);
    drop(us_db);
    us.close().unwrap();
    eu.close().unwrap();
}

#[test]
fn a_copy_promoted_once_its_home_stopped_moves_the_route_the_catalog_names() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let serving_from = |from: Vec<String>, serve: String| Node { serve: Some(serve), from, ..Node::default() };
    let us = node_with(dirs[0].path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let us_address = bound(us.node().unwrap()).unwrap().to_string();
    let eu = node_with(dirs[1].path(), serving_from(vec![us_address.clone()], "127.0.0.1:0".into()));
    let eu_address = bound(eu.node().unwrap()).unwrap().to_string();
    let ru = node_with(dirs[2].path(), serving_from(vec![eu_address.clone()], "127.0.0.1:0".into()));
    let ru_address = bound(ru.node().unwrap()).unwrap().to_string();
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('a european row')")).unwrap();
    let ru_db = ru.database("users").migrations(NOTES).open().unwrap();
    eventually("russia copies europe's file through europe", || bodies(&ru_db) == ["a european row"]);
    let ru_id = ru.node().unwrap().id.clone();
    drop((eu_db, ru_db));
    eu.close().unwrap();
    ru.close().unwrap();

    promote(dirs[2].path(), &["sql/users.db".to_owned()]).unwrap();
    let ru = node_with(dirs[2].path(), serving_from(vec![us_address], ru_address));
    let catalog = us.node().unwrap().catalog(&us).unwrap();
    eventually("russia's promoted file takes the route at its next epoch", || {
        catalog.home_of("sql/users.db").unwrap().is_some_and(|home| home.node == ru_id && home.epoch == 2)
    });
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america takes the file from russia", || bodies(&us_db) == ["a european row"]);
    us_db.exec(crate::sql!("insert into notes (body) values ('written in america')")).unwrap();
    let ru_db = ru.database("users").migrations(NOTES).open().unwrap();
    assert_eq!(bodies(&ru_db), ["a european row", "written in america"], "the write went to russia");

    // europe comes back undemoted: the catalog tells it of the later epoch
    let eu = node_with(dirs[1].path(), serving_from(vec![bound(us.node().unwrap()).unwrap().to_string()], eu_address));
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eventually("europe, the old home, takes no more writes", || {
        eu_db.exec(crate::sql!("insert into notes (body) values ('the old home')")).is_err()
    });
    drop((us_db, ru_db, eu_db));
    eu.close().unwrap();
    ru.close().unwrap();
    us.close().unwrap();
}

// contract: REPL-10
#[test]
fn a_live_move_hands_a_file_s_home_over_and_loses_no_commit() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let us = node_with(us_dir.path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let us_address = bound(us.node().unwrap()).unwrap().to_string();
    let eu_node = Node { serve: Some("127.0.0.1:0".into()), from: vec![us_address], ..Node::default() };
    let eu = node_with(eu_dir.path(), eu_node);
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    for i in 0..20 {
        eu_db.exec(crate::sql!("insert into notes (body) values (?)", format!("before {i}"))).unwrap();
    }
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america holds europe's rows", || bodies(&us_db).len() == 20);

    let moved = crate::replication::move_home(&us, "sql/users.db").unwrap();
    assert_eq!(moved.epoch, 2);
    assert!(us.node().unwrap().copy("sql/users.db").is_none(), "america writes the file now");
    us_db.exec(crate::sql!("insert into notes (body) values ('at the new home')")).unwrap();
    eventually("europe becomes a copy of america", || eu.node().unwrap().copy("sql/users.db").is_some());
    eventually("europe reads the new home's write", || bodies(&eu_db).len() == 21);
    eu_db.exec(crate::sql!("insert into notes (body) values ('from the old home')")).unwrap();
    assert_eq!(bodies(&us_db).len(), 22, "europe's write went to america");
    assert_eq!(bodies(&us_db)[..20], bodies(&eu_db)[..20], "no commit of the old home was lost");
    let home = us.node().unwrap().catalog(&us).unwrap().home_of("sql/users.db").unwrap().unwrap();
    assert_eq!((home.node.as_str(), home.epoch), (us.node().unwrap().id.as_str(), 2));
    drop((us_db, eu_db));
    eu.close().unwrap();
    us.close().unwrap();
}

#[test]
fn a_move_its_new_home_never_claimed_is_called_off_and_a_later_move_goes_on_past_it() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let us = node_with(us_dir.path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let us_address = bound(us.node().unwrap()).unwrap().to_string();
    let eu_node = Node { serve: Some("127.0.0.1:0".into()), from: vec![us_address], ..Node::default() };
    let eu = node_with(eu_dir.path(), eu_node);
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('before')")).unwrap();
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america holds europe's row", || bodies(&us_db).len() == 1);

    // america asks europe to hand the file over, and never claims it
    let copy = us.node().unwrap().copy("sql/users.db").unwrap();
    let last = super::handover::hand_over(&us, us.node().unwrap(), "sql/users.db", &copy).unwrap();
    drop(copy);
    let fenced = eu_db.exec(crate::sql!("insert into notes (body) values ('during the move')")).unwrap_err();
    assert_eq!(fenced.kind(), crate::ErrorKind::Conflict, "{fenced}");
    eventually("europe calls the move off and writes again", || {
        eu_db.exec(crate::sql!("insert into notes (body) values ('after the move went silent')")).is_ok()
    });
    let catalog = us.node().unwrap().catalog(&us).unwrap();
    let home = catalog.home_of("sql/users.db").unwrap().unwrap();
    assert_eq!((home.node.as_str(), home.epoch), (eu.node().unwrap().id.as_str(), last.epoch + 1));

    // america's claim at that epoch comes too late
    let address = us.node().unwrap().address();
    let late = super::catalog::Claim {
        file: "sql/users.db",
        node: &us.node().unwrap().id,
        address: &address,
        epoch: last.epoch + 1,
    };
    let kept = catalog.claim(&us, &late).unwrap();
    assert_eq!(kept.node, eu.node().unwrap().id, "the catalog kept the old home's claim");

    // a move made again goes on past the epoch the call took
    eventually("america holds europe's rows", || bodies(&us_db).len() == 2);
    let moved = crate::replication::move_home(&us, "sql/users.db").unwrap();
    assert_eq!(moved.epoch, last.epoch + 2);
    us_db.exec(crate::sql!("insert into notes (body) values ('at the new home')")).unwrap();
    eventually("europe reads the new home's write", || bodies(&eu_db).len() == 3);
    drop((us_db, eu_db));
    eu.close().unwrap();
    us.close().unwrap();
}

#[test]
fn an_old_home_whose_move_went_silent_stays_fenced_while_the_catalog_is_out_of_reach() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let catalog_node = Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() };
    let keeper = node_with(dirs[0].path(), catalog_node);
    let keeper_address = bound(keeper.node().unwrap()).unwrap().to_string();
    let eu = node_with(
        dirs[1].path(),
        Node { serve: Some("127.0.0.1:0".into()), from: vec![keeper_address], ..Node::default() },
    );
    let eu_address = bound(eu.node().unwrap()).unwrap().to_string();
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('before')")).unwrap();
    let us = node_with(
        dirs[2].path(),
        Node { serve: Some("127.0.0.1:0".into()), from: vec![eu_address], ..Node::default() },
    );
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america holds europe's row", || bodies(&us_db).len() == 1);

    let copy = us.node().unwrap().copy("sql/users.db").unwrap();
    super::handover::hand_over(&us, us.node().unwrap(), "sql/users.db", &copy).unwrap();
    drop(copy);
    keeper.close().unwrap();
    // past the wait, europe cannot tell whether america took the file
    std::thread::sleep(Duration::from_secs(4));
    let fenced = eu_db.exec(crate::sql!("insert into notes (body) values ('two homes')")).unwrap_err();
    assert_eq!(fenced.kind(), crate::ErrorKind::Conflict, "{fenced}");
    drop((us_db, eu_db));
    us.close().unwrap();
    eu.close().unwrap();
}

// contract: REPL-09
#[test]
fn a_synchronous_home_waits_for_the_copy_it_chose_and_counts_the_commits_none_held() {
    use std::sync::atomic::Ordering::Relaxed;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let eu_address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home_node =
        Node { serve: Some("127.0.0.1:0".into()), synchronous_with: vec![eu_address.clone()], ..Node::default() };
    let home = node_with(dirs[0].path(), home_node);
    let home_address = bound(home.node().unwrap()).unwrap().to_string();
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    let eu = node_with(
        dirs[1].path(),
        Node { serve: Some(eu_address), from: vec![home_address.clone()], ..Node::default() },
    );
    let ru = node_with(dirs[2].path(), Node { from: vec![home_address], ..Node::default() });
    eu.bucket::<String>("sessions").open().unwrap();
    ru.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &eu, &["kv.db"]);
    caught_up(&home, &ru, &["kv.db"]);
    let unheld = || home.node().unwrap().acks.unheld.load(Relaxed);

    // the commits made before europe followed were held by no copy it chose
    let (began, counted) = (Instant::now(), unheld());
    sessions.set("held", &"by europe".to_owned()).unwrap();
    assert!(began.elapsed() < Duration::from_secs(4), "europe, the copy chosen, holds it at once");
    assert_eq!(unheld(), counted, "a commit europe held is not counted");

    eu.close().unwrap();
    eventually("russia alone follows", || sessions.set("probe", &"x".to_owned()).is_err());
    let before = unheld();
    let said = sessions.set("unheld", &"russia holds it, and europe was chosen".to_owned()).unwrap_err();
    assert_eq!(said.kind(), crate::ErrorKind::CommittedLocally, "{said}");
    assert_eq!(unheld(), before + 1, "a commit no chosen copy held is counted");
    assert_eq!(sessions.get("unheld").unwrap().as_deref(), Some("russia holds it, and europe was chosen"));
    ru.close().unwrap();
    home.close().unwrap();
}

/// A link to a node that never asks which files the node serves, as a node's
/// own link does every few seconds: whatever it hears of them, the node told
/// it unasked.
struct Unasking {
    reading: super::transport::Reading,
    out: super::serve::Out,
    frames: super::message::Frames,
}

impl Unasking {
    /// Links to the node at `address` as a node in `dir` would, welcomed.
    fn to(dir: &Path, address: &str) -> Unasking {
        let state = State::new(dir, Node::default(), 0).unwrap();
        let tcp = super::follow::connect(address).unwrap();
        let (reading, writing) = super::transport::connect(&state, address, tcp).unwrap();
        reading.set_timeout(Duration::from_millis(100)).unwrap();
        let out = super::serve::Out::new(writing);
        out.send(&state.hello(String::new())).unwrap();
        let mut link = Unasking { reading, out, frames: super::message::Frames::default() };
        assert!(link.hears(|message| matches!(message, super::message::Message::Welcome { .. })), "a welcome");
        link.frames.unpack_from_here().unwrap();
        link.out.pack().unwrap();
        link
    }

    /// Reads what the node says until a message `wanted` takes comes, within
    /// the suite's bound; whether one did.
    fn hears(&mut self, mut wanted: impl FnMut(&super::message::Message) -> bool) -> bool {
        let deadline = Instant::now() + WITHIN;
        loop {
            while let Some(message) = self.frames.next().unwrap() {
                if wanted(&message) {
                    return true;
                }
            }
            if Instant::now() > deadline {
                return false;
            }
            match self.frames.fill(&mut self.reading) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) if super::message::timed_out(&error) => {}
                Err(error) => panic!("a link: {error}"),
            }
        }
    }
}

#[test]
fn a_file_a_home_makes_after_a_node_linked_reaches_the_node_with_no_wait_for_its_next_ask() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let (home, address) = serving(dirs[0].path());
    home.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    let copy = node_with(dirs[1].path(), Node { from: vec![address.clone()], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    // made just before the file, so that the file is born while the home starts sending on it
    let mut unasking = Unasking::to(dirs[2].path(), &address);

    home.database("fresh").migrations(NOTES).open().unwrap();
    let names_it = |message: &super::message::Message| match message {
        super::message::Message::Listed { files } => files.iter().any(|file| file == "sql/fresh.db"),
        _ => false,
    };
    // a link that never asks hears of it only if its home tells it at once
    assert!(unasking.hears(names_it), "told at once, not at the next ask");
    eventually("the copy holds the new file", || copy.node().unwrap().copy("sql/fresh.db").is_some());
    drop(unasking);
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_step_its_home_ran_after_the_copy_s_snapshot_reaches_the_copy_before_its_engine_opens() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    // born at its home with no step run, the file is whole on the copy before any
    home.database("fresh").open().unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let name = "sql/fresh.db";
    eventually("the copy holds the file", || copy.node().unwrap().copy(name).is_some());
    let early = copy.database("fresh").migrations(NOTES).open().unwrap_err();
    assert_eq!(early.kind(), crate::ErrorKind::Unavailable, "a step its home has not run: {early}");

    // the copy applies nothing while held here, so the home's step stays on its way
    let held = copy.node().unwrap().copy(name).unwrap();
    let ((holding, held_now), (release, released)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel());
    let applying = std::thread::spawn(move || {
        held.holding(|_| {
            holding.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
    });
    held_now.recv().unwrap();
    home.database("fresh").migrations(NOTES).open().unwrap();
    let opening = {
        let copy = copy.clone();
        std::thread::spawn(move || copy.database("fresh").migrations(NOTES).open().map(drop))
    };
    // time for the open to find the step missing before the copy applies it
    for_at_most(Duration::from_millis(500), || opening.is_finished());
    release.send(()).unwrap();
    applying.join().unwrap().unwrap();
    let opened = opening.join().unwrap();
    assert!(opened.is_ok(), "the open waits for the step its home ran: {}", opened.unwrap_err());
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_checked_against_its_home_is_its_home_page_for_page_and_a_page_changed_beside_it_is_found() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..30 {
        sessions.set(format!("{i}"), &format!("value {i}")).unwrap();
    }
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    let checked = crate::replication::verify(&copy, "kv.db").unwrap();
    assert_eq!(checked, home_point(&home, "kv.db").sequence, "checked at the point its home holds");

    // a page written beside the applier, as a fault would
    let beside = rusqlite::Connection::open(copy_dir.path().join("kv.db")).unwrap();
    beside.pragma_update(None, "user_version", 77).unwrap();
    drop(beside);
    let found = crate::replication::verify(&copy, "kv.db").unwrap_err();
    assert_eq!(found.kind(), crate::ErrorKind::Corrupt, "{found}");
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_copy_checked_while_its_home_writes_is_checked_at_the_point_the_home_answered() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    sessions.set("seed", &"value".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    let writing = std::sync::atomic::AtomicBool::new(true);
    let checks = std::thread::scope(|scope| {
        scope.spawn(|| {
            for i in 0.. {
                if !writing.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                // A busy machine leaves the copy applying more slowly than its
                // home commits, so the home keeps a few entries ahead at most:
                // a check then waits for a lag, not for a backlog that grows
                // for as long as the checks run.
                let held = copy_point(&copy, "kv.db").map_or(0, |point| point.sequence);
                let ahead = home_point(&home, "kv.db").sequence.saturating_sub(held);
                if ahead < 64 {
                    sessions.set(format!("{i}"), &"x".repeat(2000)).unwrap();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let checks: Vec<_> = (0..10).map(|_| crate::replication::verify(&copy, "kv.db")).collect();
        writing.store(false, std::sync::atomic::Ordering::Relaxed);
        checks
    });
    for checked in checks {
        assert!(checked.is_ok(), "a check on a busy home: {}", checked.unwrap_err());
    }
    copy.close().unwrap();
    home.close().unwrap();
}

#[test]
fn a_large_file_reaches_a_copy_before_the_commit_that_names_it() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let home_files = home.files("docs").open().unwrap();
    home_files.put("seed.txt", b"planted").unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    let files = copy.files("docs").open().unwrap();
    caught_up(&home, &copy, &[crate::blobs::FILE_IN_STORE]);

    for round in 0..5_u8 {
        let name = format!("large {round}.bin");
        home_files.put(&name, vec![round; 3 << 20]).unwrap();
        eventually("the copy names the file", || files.head(&name).unwrap().is_some());
        // named, and so readable at once: its bytes came before the commit
        let read = files.get(&name).unwrap().unwrap().read_all().unwrap();
        assert_eq!((read.len(), read[0]), (3 << 20, round));
    }
    copy.close().unwrap();
    home.close().unwrap();
}

/// Copies the files of a running store's directory as a crash would leave
/// them, but its lock, which another process holds.
fn crashed_image(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            crashed_image(&entry.path(), &target);
        } else if entry.file_name() != "LOCK" {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn an_old_home_started_again_after_a_crash_and_a_failover_becomes_a_copy() {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let witness =
        node_with(dirs[0].path(), Node { serve: Some("127.0.0.1:0".into()), witness: true, ..Node::default() });
    let judge = Some(bound(witness.node().unwrap()).unwrap().to_string());
    let home_address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home_node = |from: Vec<String>| Node {
        serve: Some(home_address.clone()),
        from,
        catalog: true,
        failover: judge.clone(),
        ..Node::default()
    };
    let home = node_with(dirs[1].path(), home_node(Vec::new()));
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    eventually("the home writes once its witness leased it the file", || {
        sessions.set("first", &"leased".to_owned()).is_ok()
    });
    let copy_node = Node {
        serve: Some("127.0.0.1:0".into()),
        from: vec![home_address.clone()],
        failover: judge.clone(),
        ..Node::default()
    };
    let copy = node_with(dirs[2].path(), copy_node);
    let copy_address = bound(copy.node().unwrap()).unwrap().to_string();
    caught_up(&home, &copy, &["kv.db"]);
    // the home dies: what it left is its files as they stand, no engine open on them at the next start
    crashed_image(dirs[1].path(), dirs[3].path());
    drop(sessions);
    home.close().unwrap();
    eventually("the copy takes the file over once the home's lease ran out", || {
        copy.node().unwrap().copy("kv.db").is_none()
    });
    let at_new_home = copy.bucket::<String>("sessions").open().unwrap();
    eventually("the new home writes", || at_new_home.set("after", &"the failover".to_owned()).is_ok());

    // started again from what the crash left, it hears of the new home and copies it
    let back = node_with(dirs[3].path(), home_node(vec![copy_address]));
    eventually("the old home becomes the new one's copy", || {
        super::copy::is_copy(&back.node().unwrap().log_dir("kv.db"))
    });
    // the copy opens once it is whole, which a busy machine takes a while to reach
    eventually("and reads what the new home wrote", || {
        let opened = back.bucket::<String>("sessions").open();
        opened.is_ok_and(|read| read.get("after").is_ok_and(|found| found.is_some()))
    });
    back.close().unwrap();
    copy.close().unwrap();
    witness.close().unwrap();
}

#[test]
fn an_engine_opening_a_file_its_node_takes_over_waits_for_the_takeover_and_writes_the_file() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.bucket::<String>("sessions").open().unwrap().set("before", &"the takeover".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    caught_up(&home, &copy, &["kv.db"]);
    let copied = copy_point(&copy, "kv.db").unwrap();
    home.close().unwrap();

    // a reference to the copy held here keeps the takeover waiting, the copy already let go of
    let held = copy.node().unwrap().copy("kv.db").unwrap();
    let taking_over = {
        let (store, taken) = (copy.clone(), Arc::clone(&held));
        std::thread::spawn(move || store.node().unwrap().take_over_at(&store, "kv.db", taken, 0))
    };
    eventually("the takeover lets go of the copy", || copy.node().unwrap().copy("kv.db").is_none());
    let opening = {
        let store = copy.clone();
        std::thread::spawn(move || store.bucket::<String>("sessions").open())
    };
    // time for the open to meet the takeover before it goes on
    std::thread::sleep(Duration::from_millis(300));
    drop(held);
    taking_over.join().unwrap().unwrap();

    let sessions = opening.join().unwrap().unwrap();
    sessions.set("after", &"the takeover".to_owned()).unwrap();
    assert_eq!(sessions.get("before").unwrap().as_deref(), Some("the takeover"));
    assert_eq!(home_point(&copy, "kv.db").epoch, copied.epoch + 1, "the node writes the file as its home");
    copy.close().unwrap();
}

/// Waits until `holds`, `bound` at most, and goes on either way: time for a
/// change under way on another thread to reach a point, which it may not.
fn for_at_most(bound: Duration, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + bound;
    while !holds() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_engine_opening_a_file_its_node_hands_over_opens_it_as_the_new_home_s_copy() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let us = node_with(us_dir.path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let us_address = bound(us.node().unwrap()).unwrap().to_string();
    let eu_node = |serve: String| Node { serve: Some(serve), from: vec![us_address.clone()], ..Node::default() };
    let eu = node_with(eu_dir.path(), eu_node("127.0.0.1:0".into()));
    let eu_address = bound(eu.node().unwrap()).unwrap().to_string();
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    eu_db.exec(crate::sql!("insert into notes (body) values ('before the move')")).unwrap();
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america holds europe's row", || bodies(&us_db).len() == 1);
    // europe starts again, so that the open below is the file's one open there
    drop(eu_db);
    eu.close().unwrap();
    let eu = node_with(eu_dir.path(), eu_node(eu_address));
    let (node, name) = (eu.node().unwrap(), "sql/users.db");

    // an engine's open under way, as Store::open_file makes it: the file opened as europe's own, not yet kept
    let opening = node.wait_to_open(name).unwrap();
    assert!(!node.is_new(name) && !node.takes(name) && !super::copy::is_copy(&node.log_dir(name)));
    let log = node.open_log(name).unwrap();
    let file = File::open_observed(eu.dir().join(name), Config::default(), Some(log)).unwrap();
    caught_up(&eu, &us, &[name]);
    crate::replication::move_home(&us, name).unwrap();
    // time for europe to hand the file over before the open goes on
    for_at_most(Duration::from_secs(1), || node.copy(name).is_some());
    let file = node.opened(name, file);
    drop(opening);

    eventually("europe becomes a copy of america", || node.copy(name).is_some());
    assert!(file.is_copy(), "the file opened meanwhile is a copy, not a second writer beside the copy's applier");
    us_db.exec(crate::sql!("insert into notes (body) values ('at the new home')")).unwrap();
    eventually("it reads what the new home writes", || count_notes(&file) == 2);
    drop((file, us_db));
    eu.close().unwrap();
    us.close().unwrap();
}

#[test]
fn an_engine_opening_a_copy_a_snapshot_taken_again_replaces_reads_the_snapshot() {
    use std::sync::atomic::Ordering::Relaxed;
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    sessions.set("before", &"the snapshot".to_owned()).unwrap();
    let copy =
        node_with(copy_dir.path(), Node { from: vec![address], files: Some(vec!["kv.db".into()]), ..Node::default() });
    caught_up(&home, &copy, &["kv.db"]);
    let (node, name) = (copy.node().unwrap(), "kv.db");
    let (taken, held) = (node.snapshots.load(Relaxed), copy_point(&copy, name));

    // an engine's open under way, as Store::open_file makes it: the copy held now opened and read, not yet kept
    let opening = node.wait_to_open(name).unwrap();
    assert!(node.takes(name));
    node.wait_ready(name).unwrap();
    let file = File::open_copy(copy.dir().join(name), Config::default()).unwrap();
    assert!(!has_table(&file, "beside"));
    // a writer beside the home's starts its WAL over: its log says so, and the copy takes a snapshot again
    {
        let beside = rusqlite::Connection::open(home_dir.path().join(name)).unwrap();
        for statement in ["create table beside (v)", "insert into beside (v) values (1)"] {
            beside.execute(statement, []).unwrap();
            beside.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(())).unwrap();
        }
    }
    sessions.set("after", &"the snapshot".to_owned()).unwrap();
    eventually("the copy takes a snapshot again", || node.snapshots.load(Relaxed) > taken);
    // time for the snapshot to take the file's place before the open goes on
    for_at_most(Duration::from_secs(1), || copy_point(&copy, name) != held);
    let file = node.opened(name, file);
    drop(opening);

    caught_up(&home, &copy, &[name]);
    assert!(has_table(&file, "beside"), "the file opened meanwhile reads the snapshot, not the file it replaced");
    assert_eq!(node.snapshots.load(Relaxed), taken + 1, "the snapshot took the file's place at its first try");
    drop((file, sessions));
    copy.close().unwrap();
    home.close().unwrap();
}

fn count_notes(file: &File) -> i64 {
    file.read(|connection| {
        connection.query_row("select count(*) from notes", [], |row| row.get(0)).map_err(|e| sql_error("notes", e))
    })
    .unwrap()
}

fn has_table(file: &File, table: &str) -> bool {
    let tables: i64 = file
        .read(|connection| {
            let named = "select count(*) from sqlite_master where name = ?1";
            connection.query_row(named, [table], |row| row.get(0)).map_err(|e| sql_error(table, e))
        })
        .unwrap();
    tables == 1
}

// contract: REPL-08
#[test]
fn a_cut_between_a_home_and_its_witness_never_leaves_two_homes_taking_writes() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let witness =
        node_with(dirs[0].path(), Node { serve: Some("127.0.0.1:0".into()), witness: true, ..Node::default() });
    let judge = Some(bound(witness.node().unwrap()).unwrap().to_string());
    let home = node_with(
        dirs[1].path(),
        Node { serve: Some("127.0.0.1:0".into()), failover: judge.clone(), ..Node::default() },
    );
    let home_address = bound(home.node().unwrap()).unwrap().to_string();
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    eventually("the home writes once its witness leased it the file", || {
        sessions.set("first", &"leased".to_owned()).is_ok()
    });
    let copy_node =
        Node { serve: Some("127.0.0.1:0".into()), from: vec![home_address], failover: judge, ..Node::default() };
    let copy = node_with(dirs[2].path(), copy_node);
    let copied = copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);

    // a writer on each node keeps the times its writes as a home went through
    let stop = Arc::new(AtomicBool::new(false));
    let written = |store: Store, bucket: crate::kv::Bucket<String>, name: &'static str| {
        let (stop, times) = (Arc::clone(&stop), Arc::new(Mutex::new(Vec::new())));
        let kept = Arc::clone(&times);
        let thread = std::thread::spawn(move || {
            for i in 0.. {
                if stop.load(Relaxed) {
                    break;
                }
                let home_now = store.node().unwrap().copy("kv.db").is_none();
                if home_now && bucket.set(format!("{name} {i}"), &"written".to_owned()).is_ok() {
                    kept.lock().unwrap().push(Instant::now());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        (thread, times)
    };
    let (at_home, home_times) = written(home.clone(), sessions.clone(), "home");
    let (at_copy, copy_times) = written(copy.clone(), copied.clone(), "copy");

    let home_id = home.node().unwrap().id.clone();
    let judging = witness.node().unwrap().witness.as_ref().unwrap();
    eventually("the witness holds the home's lease", || {
        judging.holder("kv.db").is_some_and(|holder| holder.node == home_id)
    });
    judging.cut(&home_id);
    eventually("the copy takes the file over once the home's lease ran out", || {
        copy.node().unwrap().copy("kv.db").is_none()
    });
    eventually("the new home writes", || !copy_times.lock().unwrap().is_empty());
    stop.store(true, Relaxed);
    at_home.join().unwrap();
    at_copy.join().unwrap();
    let last_at_home = *home_times.lock().unwrap().last().unwrap();
    let first_at_copy = *copy_times.lock().unwrap().first().expect("the new home wrote");
    assert!(last_at_home < first_at_copy, "the old home's last write came before the new home's first");

    // the old home hears of the new one and becomes its copy
    judging.heal(&home_id);
    eventually("the old home becomes the new one's copy", || home.node().unwrap().copy("kv.db").is_some());
    copied.set("after the cut", &"from the new home".to_owned()).unwrap();
    eventually("and reads what the new home wrote", || sessions.get("after the cut").unwrap().is_some());
    copy.close().unwrap();
    home.close().unwrap();
    witness.close().unwrap();
}

// contract: REPL-08
#[test]
fn a_judged_node_that_cannot_reach_its_witness_writes_nothing_not_even_a_file_born_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let nobody = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home =
        node_with(dir.path(), Node { serve: Some("127.0.0.1:0".into()), failover: Some(nobody), ..Node::default() });
    let refused = home.bucket::<String>("sessions").open().unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "{refused}");

    // A database opened with no migrations is born here and writes nothing, so
    // that its first write is timed alone: making a file, on a busy machine,
    // takes longer than the wait for a lease.
    let db = home.database("app").open().unwrap();
    let began = Instant::now();
    let refused = db.exec(crate::sql!("create table t (v text)")).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "{refused}");
    assert!(began.elapsed() < Duration::from_secs(10), "a write waits for a lease a bound, not for good");
    drop(db);
    home.close().unwrap();
}

#[test]
fn a_witness_that_starts_again_keeps_each_lease_s_holder_and_its_home_writes_on() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let witness_address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let witness_node = || Node { serve: Some(witness_address.clone()), witness: true, ..Node::default() };
    let witness = node_with(dirs[0].path(), witness_node());
    let judge = Some(witness_address.clone());
    let home = node_with(
        dirs[1].path(),
        Node { serve: Some("127.0.0.1:0".into()), failover: judge.clone(), ..Node::default() },
    );
    let home_address = bound(home.node().unwrap()).unwrap().to_string();
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    sessions.set("before", &"leased".to_owned()).unwrap();
    let copy_node =
        Node { serve: Some("127.0.0.1:0".into()), from: vec![home_address], failover: judge, ..Node::default() };
    let copy = node_with(dirs[2].path(), copy_node);
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);

    witness.close().unwrap();
    let home_id = home.node().unwrap().id.clone();
    let kept = std::fs::read_to_string(dirs[0].path().join(DIR).join("witness")).unwrap();
    assert!(kept.lines().any(|line| line.starts_with(&format!("1 {home_id} ")) && line.ends_with(" kv.db")), "{kept}");

    let witness = node_with(dirs[0].path(), witness_node());
    eventually("the home writes on once it renewed its lease", || sessions.set("after", &"renewed".to_owned()).is_ok());
    // past a lease's span and the gathering, no copy took what the home renews
    std::thread::sleep(super::witness::TTL + Duration::from_secs(2));
    assert!(copy.node().unwrap().copy("kv.db").is_some(), "the copy took no lease the home held");
    sessions.set("later", &"still the home".to_owned()).unwrap();
    let judging = witness.node().unwrap().witness.as_ref().unwrap();
    assert_eq!(judging.holder("kv.db").map(|holder| (holder.node, holder.epoch)), Some((home_id, 1)));
    copy.close().unwrap();
    home.close().unwrap();
    witness.close().unwrap();
}

#[test]
fn a_synchronous_home_s_lease_goes_only_to_a_copy_it_waited_for_once_every_one_stands() {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let witness =
        node_with(dirs[0].path(), Node { serve: Some("127.0.0.1:0".into()), witness: true, ..Node::default() });
    let judge = Some(bound(witness.node().unwrap()).unwrap().to_string());
    let eu_address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home_node = Node {
        serve: Some("127.0.0.1:0".into()),
        failover: judge.clone(),
        synchronous_with: vec![eu_address.clone()],
        ..Node::default()
    };
    let home = node_with(dirs[1].path(), home_node);
    let home_address = bound(home.node().unwrap()).unwrap().to_string();
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    let eu_node = || Node {
        serve: Some(eu_address.clone()),
        from: vec![home_address.clone()],
        failover: judge.clone(),
        ..Node::default()
    };
    let eu = node_with(dirs[2].path(), eu_node());
    let ru_node = Node {
        serve: Some("127.0.0.1:0".into()),
        from: vec![home_address.clone()],
        failover: judge.clone(),
        ..Node::default()
    };
    let ru = node_with(dirs[3].path(), ru_node);
    eu.bucket::<String>("sessions").open().unwrap();
    ru.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &eu, &["kv.db"]);
    caught_up(&home, &ru, &["kv.db"]);
    sessions.set("held", &"by europe".to_owned()).unwrap();

    // europe, the copy the home waits for, goes; then the home falls silent at its witness
    eu.close().unwrap();
    let judging = witness.node().unwrap().witness.as_ref().unwrap();
    let home_id = home.node().unwrap().id.clone();
    eventually("the witness holds the home's lease", || {
        judging.holder("kv.db").is_some_and(|holder| holder.node == home_id)
    });
    judging.cut(&home_id);
    std::thread::sleep(super::witness::TTL + Duration::from_secs(3));
    assert!(ru.node().unwrap().copy("kv.db").is_some(), "russia, which the home never waited for, takes no lease");

    // europe comes back and takes it, holding what the home answered
    let eu = node_with(dirs[2].path(), eu_node());
    let eu_id = eu.node().unwrap().id.clone();
    eventually("europe takes the lease the home lost", || {
        judging.holder("kv.db").is_some_and(|holder| holder.node == eu_id)
    });
    eventually("and writes the file", || !super::copy::is_copy(&eu.node().unwrap().log_dir("kv.db")));
    let at_europe = eu.bucket::<String>("sessions").open().unwrap();
    assert_eq!(at_europe.get("held").unwrap().as_deref(), Some("by europe"));
    eu.close().unwrap();
    ru.close().unwrap();
    home.close().unwrap();
    witness.close().unwrap();
}

#[test]
fn a_synchronous_node_a_witness_judges_names_the_copies_it_waits_for() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node { failover: Some("127.0.0.1:1".into()), synchronous: true, ..Node::default() };
    let options = Options { node: Some(node), background: false, ..Options::default() };
    let refused = Store::open(dir.path(), options).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid, "{refused}");
}

// contract: REPL-11
#[test]
fn a_node_that_serves_beyond_this_machine_takes_tls_or_says_insecure() {
    let dir = tempfile::tempdir().unwrap();
    let everywhere = Node { serve: Some("0.0.0.0:0".into()), ..Node::default() };
    let options = Options { node: Some(everywhere), background: false, ..Options::default() };
    let refused = Store::open(dir.path(), options).unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Invalid, "{refused}");
    assert!(refused.to_string().contains("insecure"), "{refused}");
}

#[test]
#[cfg_attr(windows, ignore = "listens on every interface, which Windows' firewall asks about")]
fn a_node_that_says_insecure_serves_beyond_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let insecure = Node { serve: Some("0.0.0.0:0".into()), insecure: true, ..Node::default() };
    node_with(dir.path(), insecure).close().unwrap();
}

#[test]
fn a_direct_link_takes_nothing_of_a_node_that_is_not_the_home_the_catalog_names() {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let keeper =
        node_with(dirs[0].path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let keeper_address = bound(keeper.node().unwrap()).unwrap().to_string();
    let home_address = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home_node = Node { serve: Some(home_address.clone()), from: vec![keeper_address.clone()], ..Node::default() };
    let home = node_with(dirs[1].path(), home_node);
    let db = home.database("tenant").migrations(NOTES).open().unwrap();
    db.exec(crate::sql!("insert into notes (body) values ('the home''s row')")).unwrap();
    // a node that takes from the catalog's home takes the database straight from its own
    let reader = node_with(dirs[2].path(), Node { from: vec![keeper_address], ..Node::default() });
    let read = reader.database("tenant").migrations(NOTES).open().unwrap();
    eventually("the reader holds the home's row", || bodies(&read) == ["the home's row"]);

    // the home goes, and a stranger with a database of that name listens at its address
    drop(db);
    home.close().unwrap();
    let stranger = node_with(dirs[3].path(), Node { serve: Some(home_address), ..Node::default() });
    let theirs = stranger.database("tenant").migrations(NOTES).open().unwrap();
    theirs.exec(crate::sql!("insert into notes (body) values ('a stranger''s row')")).unwrap();
    std::thread::sleep(Duration::from_secs(8));
    assert_eq!(bodies(&read), ["the home's row"], "the reader took nothing of the stranger");
    drop((read, theirs));
    stranger.close().unwrap();
    reader.close().unwrap();
    keeper.close().unwrap();
}

#[test]
fn a_node_that_stops_refuses_at_once_the_writes_waiting_for_a_lease() {
    let dir = tempfile::tempdir().unwrap();
    let nobody = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let home =
        node_with(dir.path(), Node { serve: Some("127.0.0.1:0".into()), failover: Some(nobody), ..Node::default() });
    // opened with no migrations, the database wrote nothing: its first write
    // waits for a lease at once, with no file to make first
    let db = home.database("app").open().unwrap();
    let waiting =
        std::thread::spawn(move || (db.exec(crate::sql!("create table t (v text)")).map(drop), Instant::now()));
    std::thread::sleep(Duration::from_millis(500));
    let began = Instant::now();
    // the close itself waits for the node's threads, the one dialing the
    // witness among them, which a refused dial holds two seconds on Windows
    home.close().unwrap();
    let (refused, at) = waiting.join().unwrap();
    assert!(refused.is_err(), "a write no lease holds is refused");
    let waited = at.saturating_duration_since(began);
    assert!(waited < Duration::from_secs(2), "the write waited {waited:?} for a lease once the node stopped");
}

#[cfg(feature = "metrics")]
#[test]
fn a_synchronous_home_measures_its_waits_for_a_copy_and_how_far_behind_each_copy_is() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let node = Node { serve: Some("127.0.0.1:0".into()), synchronous: true, ..Node::default() };
    let measured = Some(crate::metrics::SelfMetrics { system: false });
    let options = Options { node: Some(node), background: false, self_metrics: measured, ..Options::default() };
    let home = Store::open(home_dir.path(), options).unwrap();
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    for i in 0..2 {
        sessions.set(format!("alone {i}"), &"no copy follows".to_owned()).unwrap_err();
    }
    let address = bound(home.node().unwrap()).unwrap().to_string();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
    copy.bucket::<String>("sessions").open().unwrap();
    caught_up(&home, &copy, &["kv.db"]);
    sessions.set("held", &"by the copy".to_owned()).unwrap();

    let metrics = home.metrics().unwrap();
    // as the store's background writes them: no copy takes metrics.db, which a write of the program's waits for
    crate::sqlite::own_work(|| metrics.flush()).unwrap();
    let hour = Duration::from_secs(3600);
    let newest = |name: &str| {
        let latest = metrics.query().name(name).filter("file", "kv").since(hour).latest().unwrap();
        latest.iter().map(|latest| latest.value).collect::<Vec<f64>>()
    };
    assert_eq!(newest("tinystore_replication_committed_locally_total"), [2.0]);
    assert!(newest("tinystore_replication_ack_wait_ms_count").first().is_some_and(|waits| *waits >= 3.0));
    assert_eq!(newest("tinystore_replication_behind_entries"), [0.0], "the copy held every commit it answered");
    copy.close().unwrap();
    home.close().unwrap();
}
