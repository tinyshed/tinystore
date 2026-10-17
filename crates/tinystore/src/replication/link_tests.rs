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

#[test]
fn a_synchronous_home_answers_a_write_once_a_copy_holds_it() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home =
        node_with(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), synchronous: true, ..Node::default() });
    let sessions = home.bucket::<String>("sessions").open().unwrap();
    let alone = Instant::now();
    for i in 0..20 {
        sessions.set(format!("alone {i}"), &"no copy follows".to_owned()).unwrap();
    }
    assert!(alone.elapsed() < Duration::from_secs(4), "with no copy following, a commit waits for none");
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
fn a_node_s_status_says_which_files_it_writes_and_where_its_copies_stand() {
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (home, address) = serving(home_dir.path());
    home.bucket::<String>("sessions").open().unwrap().set("k", &"v".to_owned()).unwrap();
    let copy = node_with(copy_dir.path(), Node { from: vec![address], ..Node::default() });
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
    copy.close().unwrap();
    home.close().unwrap();
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
    let refused = secrets.take("token").unwrap_err();
    assert_eq!(refused.kind(), crate::ErrorKind::Unavailable, "a take would carry the value back unsealed: {refused}");
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
