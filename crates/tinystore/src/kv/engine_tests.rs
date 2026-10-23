//! kv on nodes: what the engine keeps in memory of a file that is a copy, or
//! that a copy became the home of.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Kv;
use crate::kv::fixture::{DAY, MINUTE, SECOND};
use crate::replication::Node;
use crate::{Clock, ErrorKind, Options, Store, TestClock};

const NOTES: [(&str, &str); 1] = [("0001_notes.sql", "create table notes (id integer primary key)")];

fn node_with(dir: &Path, node: Node) -> Store {
    let options = Options { node: Some(node), background: false, ..Options::default() };
    Store::open(dir, options).unwrap()
}

fn node_on(dir: &Path, node: Node, clock: &Arc<TestClock>) -> Store {
    let options = Options { node: Some(node), clock: Some(clock.clone()), background: false, ..Options::default() };
    Store::open(dir, options).unwrap()
}

/// How long work on another node may take to reach a point, on a machine as
/// busy as a CI runner.
fn eventually(what: &str, holds: impl FnMut() -> bool) {
    crate::testing::holds_within(Duration::from_secs(30), what, holds);
}

// contract: KV-05
#[test]
fn a_copy_that_becomes_the_home_takes_versions_past_every_one_its_old_home_took() {
    let (us_dir, eu_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let us = node_with(us_dir.path(), Node { serve: Some("127.0.0.1:0".into()), catalog: true, ..Node::default() });
    let from = vec![us.node().unwrap().address()];
    let eu = node_with(eu_dir.path(), Node { serve: Some("127.0.0.1:0".into()), from, ..Node::default() });
    let eu_db = eu.database("users").migrations(NOTES).open().unwrap();
    let at_eu = eu_db.bucket::<String>("sessions").open().unwrap();
    at_eu.set("first", &"made in europe".to_owned()).unwrap();

    // america's kv opens on its copy, then europe writes on
    let us_db = us.database("users").migrations(NOTES).open().unwrap();
    eventually("america's copy holds europe's first key", || {
        us_db.bucket::<String>("sessions").open().is_ok_and(|at_us| at_us.get("first").is_ok_and(|got| got.is_some()))
    });
    let at_us = us_db.bucket::<String>("sessions").open().unwrap();
    for i in 0..50 {
        at_eu.set(format!("{i}"), &"before the move".to_owned()).unwrap();
    }
    let last = at_eu.entry("49").unwrap().unwrap().version.revision();
    eventually("america's copy holds europe's last key", || at_us.get("49").unwrap().is_some());

    crate::replication::move_home(&us, "sql/users.db").unwrap();
    at_us.set("after", &"at the new home".to_owned()).unwrap();
    let after = at_us.entry("after").unwrap().unwrap().version.revision();
    assert!(after > last, "version {after} at the new home, {last} already taken at the old one");
    drop((at_eu, at_us, eu_db, us_db));
    eu.close().unwrap();
    us.close().unwrap();
}

// contract: KV-07
#[test]
fn reads_of_an_idle_key_on_a_copy_renew_it_at_its_home() {
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node_on(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() }, &clock);
    let from = vec![home.node().unwrap().address()];
    let copy = node_on(copy_dir.path(), Node { from, ..Node::default() }, &clock);
    let at_home = home.bucket::<String>("sessions").idle(30 * DAY).open().unwrap();
    at_home.set("token", &"signed in".to_owned()).unwrap();
    let at_copy = copy.bucket::<String>("sessions").idle(30 * DAY).open().unwrap();
    eventually("the copy holds the session", || at_copy.get("token").unwrap().is_some());

    // read on the copy, written only at the sign-in; the pass waits for no answer
    clock.advance(10 * DAY);
    assert!(at_copy.get("token").unwrap().is_some());
    assert_eq!(Kv::of(&copy).unwrap().flush_renewals().unwrap(), 1, "passed home");
    let at_its_home = Kv::of(&home).unwrap();
    eventually("the home took the copy's renewal", || at_its_home.renewals().kept() == (1, 0));
    crate::kv::maintain(&home).unwrap();
    let expires = |at: &crate::kv::Bucket<String>| at.entry("token").unwrap().unwrap().expires_at;
    assert_eq!(expires(&at_home), Some(clock.now() + 30 * DAY), "the copy's read renewed it at its home");
    eventually("the copy holds the renewal", || expires(&at_copy) == expires(&at_home));

    // a read in the last minute renews it before the read returns, waiting
    // for the home's answer but not for the copy to hold the renewal
    clock.advance(30 * DAY - 30 * SECOND);
    assert_eq!(expires(&at_copy), Some(clock.now() + 30 * DAY), "renewed with no flush between");
    clock.advance(MINUTE);
    eventually("alive on the copy past its old expiry", || at_copy.get("token").unwrap().is_some());
    assert!(at_home.get("token").unwrap().is_some(), "and at its home");
    drop((at_home, at_copy));
    copy.close().unwrap();
    home.close().unwrap();
}

// contract: KV-07
#[test]
fn a_copy_with_no_link_home_keeps_its_renewals_and_its_reads_wait_for_none() {
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let (home_dir, copy_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let home = node_on(home_dir.path(), Node { serve: Some("127.0.0.1:0".into()), ..Node::default() }, &clock);
    let from = vec![home.node().unwrap().address()];
    let copy = node_on(copy_dir.path(), Node { from, ..Node::default() }, &clock);
    let at_home = home.bucket::<String>("sessions").idle(30 * DAY).open().unwrap();
    at_home.set("token", &"signed in".to_owned()).unwrap();
    let at_copy = copy.bucket::<String>("sessions").idle(30 * DAY).open().unwrap();
    eventually("the copy holds the session", || at_copy.get("token").unwrap().is_some());
    drop(at_home);
    home.close().unwrap();
    let closed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    eventually("the copy has no link to its home", || {
        crate::replication::links(copy_dir.path())
            .unwrap()
            .is_some_and(|links| links.written > closed && links.from.is_none())
    });

    // a read in the key's last minute: waiting for a link would take ten seconds
    clock.advance(30 * DAY - 30 * SECOND);
    let kv = Kv::of(&copy).unwrap();
    let read = Instant::now();
    assert!(at_copy.get("token").unwrap().is_some());
    assert!(read.elapsed() < Duration::from_secs(5), "the read waited {:?}", read.elapsed());
    assert_eq!(kv.renewals().kept(), (0, 1), "kept for the next pass home");

    // the pass, on the store's scheduler beside every engine's work
    let pass = Instant::now();
    assert_eq!(kv.flush_renewals().unwrap_err().kind(), ErrorKind::Unavailable);
    assert!(pass.elapsed() < Duration::from_secs(5), "the pass waited {:?}", pass.elapsed());
    assert_eq!(kv.renewals().kept(), (0, 1), "kept again");
    drop((at_copy, kv));
    copy.close().unwrap();
}
