use std::sync::{Arc, Barrier};
use std::thread;

use super::*;
use crate::Options;
use crate::kv::engine::Kv;
use crate::kv::fixture::{MINUTE, SECOND, Session, fixture, session};
use crate::kv::path::Branch;
use crate::kv::{self, Bytes};

#[test]
fn a_value_comes_back_as_its_type() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").open().unwrap();
    sessions.set("token", &session(42)).unwrap();
    assert_eq!(sessions.get("token").unwrap(), Some(session(42)));
    assert!(sessions.has("token").unwrap());
    assert_eq!(sessions.get("other").unwrap(), None);

    let counts = f.store.bucket::<i64>("counts").open().unwrap();
    counts.set(7, &-3).unwrap();
    assert_eq!(counts.get("7").unwrap(), Some(-3), "an integer key is its decimal spelling");

    let prices = f.store.bucket::<f64>("prices").open().unwrap();
    prices.set("zero", &-0.0).unwrap();
    assert_eq!(prices.get("zero").unwrap().unwrap().to_bits(), (-0.0f64).to_bits());
}

#[test]
fn delete_removes_a_key_and_says_whether_it_was_there() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    assert!(sessions.delete("token").unwrap());
    assert!(!sessions.delete("token").unwrap());
    assert_eq!(sessions.get("token").unwrap(), None);
}

#[test]
fn create_writes_only_a_key_that_is_not_there() {
    let f = fixture();
    let seen = f.store.bucket::<()>("stripe-events").ttl(MINUTE).open().unwrap();
    assert!(seen.add("evt_1").unwrap());
    assert!(!seen.add("evt_1").unwrap(), "a key that is there is false, never an error");
    f.clock.advance(2 * MINUTE);
    assert!(seen.add("evt_1").unwrap(), "an expired key is not there");
}

#[test]
fn of_two_takes_of_one_key_one_gets_the_value() {
    let f = fixture();
    let codes = f.store.bucket::<i64>("login-codes").open().unwrap();
    codes.set("code", &42).unwrap();
    let start = Arc::new(Barrier::new(8));
    let takers: Vec<_> = (0..8)
        .map(|_| {
            let (codes, start) = (codes.clone(), Arc::clone(&start));
            thread::spawn(move || {
                start.wait();
                codes.take("code").unwrap()
            })
        })
        .collect();
    let taken: Vec<i64> = takers.into_iter().filter_map(|taker| taker.join().unwrap()).collect();
    assert_eq!(taken, [42]);
    assert_eq!(codes.get("code").unwrap(), None);
}

#[test]
fn a_write_at_a_stale_version_is_a_conflict_naming_its_key() {
    let f = fixture();
    let settings = f.store.bucket::<String>("settings").open().unwrap();
    settings.set("home", &"a".to_owned()).unwrap();
    let seen = settings.entry("home").unwrap().unwrap();
    settings.set("home", &"b".to_owned()).unwrap();

    let error = settings.key("home").if_version(seen.version).set(&"c".to_owned()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(error.to_string().starts_with(r#"kv bucket settings: key "home": "#), "{error}");
    assert_eq!(settings.get("home").unwrap().as_deref(), Some("b"));

    let current = settings.entry("home").unwrap().unwrap();
    settings.key("home").if_version(current.version).set(&"c".to_owned()).unwrap();
    assert_eq!(settings.get("home").unwrap().as_deref(), Some("c"));
}

#[test]
fn a_version_never_repeats_across_a_delete_and_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let first = {
        let store = Store::open(dir.path(), Options::default()).unwrap();
        let settings = store.bucket::<i64>("settings").open().unwrap();
        settings.set("a", &1).unwrap();
        let version = settings.entry("a").unwrap().unwrap().version;
        settings.delete("a").unwrap();
        store.close().unwrap();
        version
    };
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let settings = store.bucket::<i64>("settings").open().unwrap();
    settings.set("a", &1).unwrap();
    assert_ne!(settings.entry("a").unwrap().unwrap().version, first);
    store.close().unwrap();
}

#[test]
fn a_key_expires_on_the_stores_clock_and_maintenance_deletes_its_row() {
    let f = fixture();
    let codes = f.store.bucket::<i64>("codes").ttl(15 * MINUTE).open().unwrap();
    codes.set("code", &42).unwrap();
    f.clock.advance(14 * MINUTE);
    assert_eq!(codes.get("code").unwrap(), Some(42));
    f.clock.advance(2 * MINUTE);
    assert_eq!(codes.get("code").unwrap(), None);
    assert_eq!(f.rows("_tinystore_kv_cells"), 1, "the row stays until maintenance");
    assert_eq!(kv::maintain(&f.store).unwrap().expired, 1);
    assert_eq!(f.rows("_tinystore_kv_cells"), 0);
}

#[test]
fn a_set_keeps_the_expiry_a_live_key_has_and_expire_gives_a_new_one() {
    let f = fixture();
    let codes = f.store.bucket::<i64>("codes").ttl(10 * MINUTE).open().unwrap();
    codes.set("code", &1).unwrap();
    let first = codes.entry("code").unwrap().unwrap().expires_at;
    f.clock.advance(MINUTE);
    codes.set("code", &2).unwrap();
    assert_eq!(codes.entry("code").unwrap().unwrap().expires_at, first);

    assert!(codes.expire("code", MINUTE).unwrap());
    f.clock.advance(2 * MINUTE);
    assert_eq!(codes.get("code").unwrap(), None, "expire can shorten a key's life");
    assert!(!codes.expire("code", MINUTE).unwrap(), "an expired key is not there to expire");
}

#[test]
fn an_idle_key_lives_on_while_it_is_read() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").idle(30 * MINUTE).open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    for _ in 0..4 {
        f.clock.advance(20 * MINUTE);
        assert!(sessions.get("token").unwrap().is_some());
        assert_eq!(kv::maintain(&f.store).unwrap().renewed, 1, "the flush writes the renewal the read asked for");
    }
    f.clock.advance(31 * MINUTE);
    assert_eq!(sessions.get("token").unwrap(), None);
}

#[test]
fn a_read_of_an_idle_key_waits_for_no_commit() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").idle(30 * MINUTE).open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    let commits = f.commits();
    f.clock.advance(5 * MINUTE);
    for _ in 0..100 {
        assert!(sessions.get("token").unwrap().is_some());
        assert!(sessions.has("token").unwrap());
    }
    assert_eq!(f.commits(), commits, "two hundred reads wrote nothing");
    assert_eq!(kv::maintain(&f.store).unwrap().renewed, 1, "they asked for one renewal");
}

#[test]
fn an_idle_key_read_in_its_last_minute_is_renewed_before_the_read_returns() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").idle(30 * MINUTE).open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    f.clock.advance(29 * MINUTE + 30 * SECOND);
    let entry = sessions.entry("token").unwrap().unwrap();
    assert_eq!(entry.expires_at, Some(f.store.now() + 30 * MINUTE));
    f.clock.advance(2 * MINUTE);
    assert!(sessions.get("token").unwrap().is_some(), "renewed with no flush between");
}

#[test]
fn a_renewal_never_extends_a_key_written_again_since_its_read() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").idle(30 * MINUTE).open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    f.clock.advance(20 * MINUTE);
    sessions.get("token").unwrap();
    sessions.delete("token").unwrap();
    sessions.key("token").ttl(MINUTE).create(&session(2)).unwrap();
    assert_eq!(kv::maintain(&f.store).unwrap().renewed, 0, "the renewal was the deleted key's");
    f.clock.advance(2 * MINUTE);
    assert_eq!(sessions.get("token").unwrap(), None);
}

#[test]
fn a_write_of_an_idle_key_is_a_use_and_a_ttl_key_keeps_its_expiry() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").idle(30 * MINUTE).open().unwrap();
    sessions.set("token", &session(1)).unwrap();
    f.clock.advance(20 * MINUTE);
    sessions.set("token", &session(2)).unwrap();
    f.clock.advance(20 * MINUTE);
    assert_eq!(sessions.get("token").unwrap(), Some(session(2)), "it lives thirty minutes from its last write");
}

#[test]
fn a_take_whose_value_no_longer_reads_keeps_it() {
    let f = fixture();
    f.store.bucket::<String>("mixed").open().unwrap().set("a", &"text".to_owned()).unwrap();
    let as_numbers = f.store.bucket::<i64>("mixed").open().unwrap();
    assert_eq!(as_numbers.take("a").unwrap_err().kind(), ErrorKind::Corrupt);
    assert_eq!(f.store.bucket::<String>("mixed").open().unwrap().get("a").unwrap().as_deref(), Some("text"));
}

#[test]
fn clearing_a_branch_removes_the_branches_under_it_and_no_other() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").open().unwrap();
    sessions.under(1).set("phone", &session(1)).unwrap();
    sessions.under(1).under("old").set("tablet", &session(1)).unwrap();
    sessions.under(2).set("phone", &session(2)).unwrap();
    sessions.under("1\0x").set("laptop", &session(3)).unwrap();

    sessions.under(1).clear().unwrap();
    assert_eq!(sessions.under(1).get("phone").unwrap(), None);
    assert_eq!(sessions.under(1).under("old").get("tablet").unwrap(), None);
    assert!(sessions.under(2).has("phone").unwrap());
    assert!(sessions.under("1\0x").has("laptop").unwrap(), "an owner that starts like another is not under it");
}

#[test]
fn a_large_clear_hides_its_keys_at_once_and_maintenance_deletes_them() {
    let f = fixture();
    let items = f.store.bucket::<i64>("items").open().unwrap();
    let kv = Kv::of(&f.store).unwrap();
    let id = items.scope.id;
    let paths: Vec<Vec<u8>> =
        (0..CLEAR_BOUND + 5).map(|n| Branch::default().under("big").unwrap().path(&n.to_string()).unwrap()).collect();
    let revision = kv.revision();
    kv.file()
        .transaction(|tx| -> Result<()> {
            for path in &paths {
                let version = next_version(&revision, tx)?;
                cells::put(tx, id, path, (version, None), &Raw::Int(1), None)?;
            }
            Ok(())
        })
        .unwrap();
    items.under("small").set("a", &1).unwrap();

    items.under("big").clear().unwrap();
    assert_eq!(items.under("big").get("7").unwrap(), None, "hidden at once");
    assert_eq!(f.rows("_tinystore_kv_branches"), 1, "marked rather than deleted");
    items.under("big").set("7", &2).unwrap();
    assert_eq!(items.under("big").get("7").unwrap(), Some(2), "a key written after the clear is a new key");

    let done = kv::maintain(&f.store).unwrap();
    assert_eq!(done.cleared, CLEAR_BOUND + 4, "every hidden row but the one written again");
    assert_eq!(f.rows("_tinystore_kv_branches"), 0);
    assert_eq!(items.under("big").get("7").unwrap(), Some(2));
    assert!(items.under("small").has("a").unwrap());
}

#[test]
fn a_branch_reads_in_pages_in_the_order_of_its_keys() {
    let f = fixture();
    let items = f.store.bucket::<i64>("items").open().unwrap();
    for n in 0..2500 {
        items.under("list").set(format!("{n:05}"), &n).unwrap();
    }
    items.under("list").under("deeper").set("x", &-1).unwrap();

    let first = items.under("list").list(1000, None).unwrap();
    assert_eq!(first.entries.len(), 1000);
    assert_eq!(first.entries[0].key, "00000");
    assert_eq!(first.next.as_deref(), Some("00999"));
    let second = items.under("list").list(1000, first.next.as_deref()).unwrap();
    assert_eq!(second.entries[0].key, "01000");

    let all: Vec<i64> = items.under("list").all().map(|entry| entry.unwrap().value).collect();
    assert_eq!(all, (0..2500).collect::<Vec<_>>(), "a branch's own keys, not those of the branches under it");
}

#[test]
fn a_large_value_lives_in_a_row_of_its_own_until_it_is_replaced() {
    let f = fixture();
    let files = f.store.bucket::<Bytes>("files").open().unwrap();
    let big = Bytes(vec![7; 10_000]);
    files.set("a", &big).unwrap();
    assert_eq!(f.rows("_tinystore_kv_spilled"), 1);
    assert_eq!(files.get("a").unwrap(), Some(big));
    files.set("a", &Bytes(vec![1, 2])).unwrap();
    assert_eq!(f.rows("_tinystore_kv_spilled"), 0);
    files.set("b", &Bytes(vec![9; 600])).unwrap();
    files.delete("b").unwrap();
    assert_eq!(f.rows("_tinystore_kv_spilled"), 0);
}

#[test]
fn a_value_over_a_mebibyte_is_refused() {
    let f = fixture();
    let files = f.store.bucket::<Bytes>("files").open().unwrap();
    let error = files.set("a", &Bytes(vec![0; MAX_VALUE + 1])).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Limit);
}

#[test]
fn a_bad_name_or_key_is_refused() {
    let f = fixture();
    let error = f.store.bucket::<i64>("Sessions").open().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid);
    let items = f.store.bucket::<i64>("items").open().unwrap();
    assert_eq!(items.set("", &1).unwrap_err().kind(), ErrorKind::Invalid);
    assert_eq!(items.under("").set("a", &1).unwrap_err().kind(), ErrorKind::Invalid);
}

#[test]
fn a_value_of_another_type_is_corrupt_rather_than_misread() {
    let f = fixture();
    f.store.bucket::<String>("mixed").open().unwrap().set("a", &"text".to_owned()).unwrap();
    let error = f.store.bucket::<i64>("mixed").open().unwrap().get("a").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt);
}

#[test]
fn values_outlive_the_store_that_wrote_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    store.bucket::<Session>("sessions").open().unwrap().under(5).set("t", &session(5)).unwrap();
    store.close().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    let sessions = store.bucket::<Session>("sessions").open().unwrap();
    assert_eq!(sessions.under(5).get("t").unwrap(), Some(session(5)));
    store.close().unwrap();
}

#[test]
fn a_key_call_says_its_expiry_and_version_before_its_last_step() {
    let f = fixture();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    notes.key("a").ttl(MINUTE).set(&"one".to_owned()).unwrap();
    let seen = notes.entry("a").unwrap().unwrap();
    assert_eq!(seen.expires_at, Some(f.store.now() + MINUTE));
    assert_eq!(notes.key("a").if_version(seen.version).take().unwrap().as_deref(), Some("one"));
    let taken = notes.key("a").if_version(seen.version).delete().unwrap_err();
    assert_eq!(taken.kind(), ErrorKind::Conflict, "the key was taken since its version was read");

    notes.set("b", &"two".to_owned()).unwrap();
    assert!(notes.key("b").expires_at(f.store.now() + 2 * MINUTE).expire().unwrap());
    assert_eq!(notes.key("b").ttl(MINUTE).delete().unwrap_err().kind(), ErrorKind::Invalid, "a ttl on a delete");
    assert_eq!(notes.key("b").expire().unwrap_err().kind(), ErrorKind::Invalid, "an expiry needs a ttl or a time");
    assert!(!notes.key("b").create(&"three".to_owned()).unwrap(), "create writes only a key that is not there");
}

#[test]
fn an_expired_key_is_absent_to_every_call() {
    let f = fixture();
    let codes = f.store.bucket::<i64>("codes").ttl(MINUTE).open().unwrap();
    codes.set("gone", &1).unwrap();
    let version = codes.entry("gone").unwrap().unwrap().version;
    f.clock.advance(MINUTE);
    assert_eq!(codes.get("gone").unwrap(), None);
    assert_eq!(codes.entry("gone").unwrap(), None);
    assert!(!codes.has("gone").unwrap());
    assert_eq!(codes.list(10, None).unwrap().entries, []);
    assert_eq!(codes.take("gone").unwrap(), None);
    assert!(!codes.delete("gone").unwrap());
    assert!(!codes.expire("gone", MINUTE).unwrap());
    let stale = codes.key("gone").if_version(version).set(&2).unwrap_err();
    assert_eq!(stale.kind(), ErrorKind::Conflict, "a version of an expired key no longer holds");
    assert!(codes.create("gone", &3).unwrap(), "create takes an expired key");
    assert_eq!(codes.get("gone").unwrap(), Some(3));
}

/// SQLite's `synchronous` on a file's writer: 2 syncs each commit, 1 leaves
/// the sync to checkpoints.
fn synchronous(file: &crate::sqlite::File) -> i64 {
    file.transaction(|tx| -> crate::Result<i64> {
        tx.pragma_query_value(None, "synchronous", |row| row.get(0))
            .map_err(|error| crate::sqlite::sql_error("synchronous", error))
    })
    .unwrap()
}

#[test]
fn kv_db_commits_as_far_as_its_store_says_and_syncs_each_commit_unless_told() {
    for (durability, level) in [(None, 2), (Some(crate::Durability::Full), 2), (Some(crate::Durability::Os), 1)] {
        let dir = tempfile::tempdir().unwrap();
        let options = crate::Options { durability, background: false, ..crate::Options::default() };
        let store = crate::Store::open(dir.path(), options).unwrap();
        let hits = store.bucket::<i64>("hits").open().unwrap();
        hits.set("a", &1).unwrap();
        assert_eq!(synchronous(Kv::of(&store).unwrap().file()), level, "{durability:?}");
        assert_eq!(hits.get("a").unwrap(), Some(1));
        store.close().unwrap();
    }
}
