use std::fs;
use std::path::Path;

use crate::kv::engine::Kv;
use crate::kv::fixture::{HOUR, Session, fixture, session};
use crate::kv::{Bucket, Bytes};
use crate::{ErrorKind, Options, Result, Store};

const PASSWORD: &str = "hunter2-the-password";

/// Whether a file of the store's directory holds `text`, in a database, its
/// log or anything else kept there. `LOCK` is the store's alone while it is
/// open, and Windows lets nobody else read it.
fn on_disk(dir: &Path, text: &str) -> bool {
    let files = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path());
    files.filter(|path| path.is_file() && !path.ends_with("LOCK")).any(|path| {
        let bytes = fs::read(&path).unwrap();
        bytes.windows(text.len()).any(|window| window == text.as_bytes())
    })
}

/// Runs a statement on kv.db's writer, as someone who can write the file but
/// has no key would.
fn tamper(store: &Store, sql: &'static str) {
    let kv = Kv::of(store).unwrap();
    let run =
        move |tx: &crate::sqlite::Tx<'_>| tx.execute_batch(sql).map_err(|error| crate::sqlite::sql_error(sql, error));
    kv.file().write(0, run).unwrap();
}

fn passwords(store: &Store) -> Bucket<String> {
    store.bucket::<String>("passwords").encrypted().open().unwrap()
}

#[test]
fn an_encrypted_bucket_keeps_no_value_a_file_shows() {
    let f = fixture();
    let notes = f.store.bucket::<String>("notes").open().unwrap();
    notes.set("n1", &"a-note-kept-plain".to_owned()).unwrap();
    passwords(&f.store).set("source-42", &PASSWORD.to_owned()).unwrap();
    assert_eq!(passwords(&f.store).get("source-42").unwrap().as_deref(), Some(PASSWORD));

    assert!(on_disk(f.dir.path(), "a-note-kept-plain"), "the look finds a value that is not sealed");
    assert!(!on_disk(f.dir.path(), PASSWORD));
    assert!(on_disk(f.dir.path(), "source-42"), "a key is not sealed");

    let f = f.reopen();
    assert_eq!(passwords(&f.store).get("source-42").unwrap().as_deref(), Some(PASSWORD), "the key is read again");
}

#[test]
fn every_call_of_an_encrypted_bucket_gives_what_was_written() {
    let f = fixture();
    let sessions = f.store.bucket::<Session>("sessions").ttl(HOUR).encrypted().open().unwrap();
    sessions.set("a", &session(1)).unwrap();
    assert!(sessions.create("b", &session(2)).unwrap());
    assert!(!sessions.create("b", &session(3)).unwrap());
    assert_eq!(sessions.entry("b").unwrap().unwrap().value, session(2));
    assert_eq!(sessions.list(10, None).unwrap().entries.len(), 2);
    assert_eq!(sessions.all().map(|entry| entry.unwrap().value).collect::<Vec<_>>(), [session(1), session(2)]);
    assert_eq!(sessions.take("a").unwrap(), Some(session(1)));
    assert_eq!(sessions.take("a").unwrap(), None);
    sessions.under("user-7").set("a", &session(7)).unwrap();
    assert_eq!(sessions.under("user-7").get("a").unwrap(), Some(session(7)));
    assert_eq!(sessions.get("a").unwrap(), None, "a branch's key is its own");

    let counts = f.store.bucket::<i64>("counts").encrypted().open().unwrap();
    counts.set("n", &i64::MIN).unwrap();
    assert_eq!(counts.get("n").unwrap(), Some(i64::MIN), "an integer comes back an integer");
    let seen = f.store.bucket::<()>("seen").encrypted().open().unwrap();
    assert!(seen.add("event-1").unwrap() && !seen.add("event-1").unwrap());
    let files = f.store.bucket::<Bytes>("files").encrypted().open().unwrap();
    let large = Bytes((0..20_000u32).map(|n| n.to_le_bytes()[0]).collect());
    files.set("large", &large).unwrap();
    assert_eq!(files.get("large").unwrap(), Some(large), "a value past a row's own bytes");

    let moved = f.store.tx(|tx| -> Result<Option<i64>> {
        let counts = tx.with(&counts);
        let n = counts.take("n")?;
        counts.set("m", &42)?;
        assert_eq!(counts.list(10, None)?.entries.len(), 1);
        Ok(n)
    });
    assert_eq!((moved.unwrap(), counts.get("m").unwrap()), (Some(i64::MIN), Some(42)));
}

#[test]
fn a_value_copied_under_another_key_does_not_open_and_can_be_deleted() {
    let f = fixture();
    let passwords = passwords(&f.store);
    passwords.set("mine", &PASSWORD.to_owned()).unwrap();
    passwords.set("theirs", &"another-password".to_owned()).unwrap();
    // someone who writes the file puts their own sealed value where mine was
    tamper(
        &f.store,
        "update _tinystore_kv_cells as c set value = (
            select value from _tinystore_kv_cells where bucket = c.bucket and path > c.path)
        where path = (select min(path) from _tinystore_kv_cells where bucket = c.bucket)",
    );
    let moved = passwords.get("mine").unwrap_err();
    assert_eq!(moved.kind(), ErrorKind::Corrupt, "{moved}");
    assert!(moved.to_string().contains("kv bucket passwords: key \"mine\""), "{moved}");
    assert_eq!(passwords.get("theirs").unwrap().as_deref(), Some("another-password"));

    assert_eq!(passwords.take("mine").unwrap_err().kind(), ErrorKind::Corrupt);
    assert!(passwords.has("mine").unwrap(), "a take that could not read its value left it");
    assert!(passwords.delete("mine").unwrap(), "a delete reads no value");
}

#[test]
fn a_store_opened_with_another_key_reads_and_writes_nothing_until_the_bucket_is_cleared() {
    let f = fixture();
    passwords(&f.store).set("source-42", &PASSWORD.to_owned()).unwrap();
    f.store.close().unwrap();
    fs::write(f.dir.path().join("encryption.key"), "ab".repeat(32)).unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();

    let passwords = passwords(&store);
    let read = passwords.get("source-42").unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "{read}");
    assert!(read.to_string().contains("was sealed with the key"), "{read}");
    // a write under another key would be lost to the key that opens the rest
    let written = passwords.set("source-43", &"another".to_owned()).unwrap_err();
    assert_eq!(written.kind(), ErrorKind::Invalid, "{written}");
    assert!(written.to_string().contains("or clear the bucket"), "{written}");
    assert!(!passwords.has("source-43").unwrap());

    assert!(passwords.delete("source-42").unwrap(), "a delete needs no key");
    passwords.clear().unwrap();
    passwords.set("source-43", &"another".to_owned()).unwrap();
    assert_eq!(passwords.get("source-43").unwrap().as_deref(), Some("another"), "cleared whole, it takes this key");
}

#[test]
fn a_key_lost_is_not_replaced_while_a_bucket_holds_values_sealed_with_it() {
    let f = fixture();
    passwords(&f.store).set("source-42", &PASSWORD.to_owned()).unwrap();
    f.store.close().unwrap();
    let key = f.dir.path().join("encryption.key");
    let kept = fs::read_to_string(&key).unwrap();
    fs::remove_file(&key).unwrap();

    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let lost = store.bucket::<String>("passwords").encrypted().open().unwrap_err();
    assert_eq!(lost.kind(), ErrorKind::Io, "{lost}");
    assert!(lost.to_string().contains("and the store makes no other"), "{lost}");
    assert!(!key.exists(), "no key was made in its place");
    // a bucket that holds nothing sealed has nothing to lose, and the store's key is made for it
    store.bucket::<String>("tokens").encrypted().open().unwrap();
    assert!(key.exists());
    store.close().unwrap();

    fs::write(&key, kept).unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    assert_eq!(
        passwords(&store).get("source-42").unwrap().as_deref(),
        Some(PASSWORD),
        "found again, the key opens them"
    );
}

#[test]
fn a_name_keeps_whether_it_is_encrypted() {
    let f = fixture();
    f.store.bucket::<String>("notes").open().unwrap();
    let sealed_later = f.store.bucket::<String>("notes").encrypted().open().unwrap_err();
    assert_eq!(sealed_later.kind(), ErrorKind::Invalid);
    assert!(sealed_later.to_string().contains("the name holds values, not encrypted values"), "{sealed_later}");

    passwords(&f.store);
    let plain_later = f.store.bucket::<String>("passwords").open().unwrap_err();
    assert!(plain_later.to_string().contains("the name holds encrypted values, not values"), "{plain_later}");
}

#[test]
fn the_key_is_the_file_the_options_name_which_the_store_never_makes() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("keys/tinystore.key");
    let options = || Options { background: false, encryption_key_file: Some(key.clone()), ..Options::default() };

    let store = Store::open(dir.path().join("data"), options()).unwrap();
    let missing = store.bucket::<String>("passwords").encrypted().open().unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::Io, "{missing}");
    assert!(!key.exists() && !dir.path().join("data/encryption.key").exists(), "no key was made");

    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, format!("{}\n", "0f".repeat(32))).unwrap();
    passwords(&store).set("source-42", &PASSWORD.to_owned()).unwrap();
    store.close().unwrap();
    assert!(!dir.path().join("data/encryption.key").exists());

    let store = Store::open(dir.path().join("data"), options()).unwrap();
    assert_eq!(passwords(&store).get("source-42").unwrap().as_deref(), Some(PASSWORD));
}

#[test]
fn a_store_that_seals_nothing_makes_no_key() {
    let f = fixture();
    f.store.bucket::<String>("notes").open().unwrap().set("n1", &"plain".to_owned()).unwrap();
    assert!(!f.dir.path().join("encryption.key").exists());
    passwords(&f.store);
    assert!(f.dir.path().join("encryption.key").exists(), "the first encrypted bucket made it");
}
