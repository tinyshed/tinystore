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

// contract: SEC-03
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

// contract: SEC-03
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

// contract: SEC-03
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

/// A key's file of `digits`, its 64 hex digits.
fn key_file(dir: &Path, name: &str, digits: &str) -> std::path::PathBuf {
    let file = dir.join(name);
    fs::write(&file, digits.repeat(32)).unwrap();
    file
}

/// The store of `dir`, sealing with the key of `sealing` and opening with
/// those of `older` too.
fn with_keys(dir: &Path, sealing: &Path, older: &[&Path]) -> Store {
    let older_encryption_keys = older.iter().map(|file| file.to_path_buf()).collect();
    let options = Options {
        background: false,
        encryption_key_file: Some(sealing.to_path_buf()),
        older_encryption_keys,
        ..Options::default()
    };
    Store::open(dir, options).unwrap()
}

// contract: SEC-04
#[test]
fn a_store_opened_with_another_key_keeps_the_values_it_cannot_open_and_seals_new_ones_beside_them() {
    let f = fixture();
    passwords(&f.store).set("source-42", &PASSWORD.to_owned()).unwrap();
    f.store.close().unwrap();
    // a restore without its key, and a new key put where the store reads it
    let old = f.dir.path().join("old.key");
    fs::rename(f.dir.path().join("encryption.key"), &old).unwrap();
    fs::write(f.dir.path().join("encryption.key"), "ab".repeat(32)).unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();

    let passwords = passwords(&store);
    let read = passwords.get("source-42").unwrap_err();
    assert_eq!(read.kind(), ErrorKind::Invalid, "{read}");
    assert!(read.to_string().contains("which the store does not hold"), "{read}");
    passwords.set("source-43", &"another".to_owned()).unwrap();
    assert_eq!(passwords.get("source-43").unwrap().as_deref(), Some("another"), "sealed with the new key");
    let left = passwords.reseal().unwrap_err();
    assert_eq!(left.kind(), ErrorKind::Invalid, "a value no key of the store opens stops a reseal: {left}");
    store.close().unwrap();

    // the old key found again opens what it sealed, beside what the new one did
    let new = f.dir.path().join("encryption.key");
    let store = with_keys(f.dir.path(), &new, &[&old]);
    let passwords = self::passwords(&store);
    assert_eq!(passwords.get("source-42").unwrap().as_deref(), Some(PASSWORD));
    assert_eq!(passwords.get("source-43").unwrap().as_deref(), Some("another"));
    assert_eq!(passwords.reseal().unwrap(), 1, "the one value the old key sealed");
    store.close().unwrap();

    let store = with_keys(f.dir.path(), &new, &[]);
    assert_eq!(self::passwords(&store).get("source-42").unwrap().as_deref(), Some(PASSWORD), "the old key can go");
}

// contract: SEC-04
#[test]
fn a_reseal_seals_with_the_new_key_what_the_old_one_did_keeping_each_version_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let (old, new) = (key_file(dir.path(), "old.key", "0a"), key_file(dir.path(), "new.key", "0b"));
    let data = dir.path().join("data");
    let store = with_keys(&data, &old, &[]);
    let sessions = store.bucket::<String>("sessions").ttl(HOUR).encrypted().open().unwrap();
    let large = "x".repeat(2000);
    store
        .tx(|tx| -> Result<()> {
            let sessions = tx.with(&sessions);
            (0..2500).try_for_each(|n| sessions.set(format!("s-{n:04}"), &format!("token-{n}")))
        })
        .unwrap();
    sessions.under("admin").set("root", &large).unwrap();
    let before = sessions.entry("s-0007").unwrap().unwrap();
    store.close().unwrap();

    let store = with_keys(&data, &new, &[&old]);
    let sessions = store.bucket::<String>("sessions").ttl(HOUR).encrypted().open().unwrap();
    sessions.set("s-new", &"sealed-new".to_owned()).unwrap();
    assert_eq!(sessions.reseal().unwrap(), 2501, "every value the old key sealed, past a batch, a branch's too");
    assert_eq!(sessions.reseal().unwrap(), 0, "and then none");
    let after = sessions.entry("s-0007").unwrap().unwrap();
    assert_eq!((after.version, after.expires_at), (before.version, before.expires_at), "the same value");
    store.close().unwrap();

    let store = with_keys(&data, &new, &[]);
    let sessions = store.bucket::<String>("sessions").ttl(HOUR).encrypted().open().unwrap();
    assert_eq!(sessions.get("s-2499").unwrap().as_deref(), Some("token-2499"), "opened without the old key");
    assert_eq!(sessions.under("admin").get("root").unwrap(), Some(large), "a value kept apart from its cell");
    let plain = store.bucket::<String>("notes").open().unwrap();
    assert_eq!(plain.reseal().unwrap_err().kind(), ErrorKind::Invalid, "a bucket that seals nothing");
}

#[test]
fn a_reseal_of_large_values_seals_every_one_across_commits_bound_by_their_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (old, new) = (key_file(dir.path(), "old.key", "0a"), key_file(dir.path(), "new.key", "0b"));
    let data = dir.path().join("data");
    let store = with_keys(&data, &old, &[]);
    let files = store.bucket::<String>("files").encrypted().open().unwrap();
    // more than one commit's bytes, and far fewer values than one commit's count
    let large = |n: u8| char::from(b'a' + n).to_string().repeat(800_000);
    (0..12).for_each(|n| files.set(format!("f-{n:02}"), &large(n)).unwrap());
    store.close().unwrap();

    let store = with_keys(&data, &new, &[&old]);
    assert_eq!(store.bucket::<String>("files").encrypted().open().unwrap().reseal().unwrap(), 12);
    store.close().unwrap();

    let store = with_keys(&data, &new, &[]);
    let files = store.bucket::<String>("files").encrypted().open().unwrap();
    for n in 0..12 {
        assert_eq!(files.get(format!("f-{n:02}")).unwrap(), Some(large(n)), "f-{n:02} opened without the old key");
    }
}

// contract: SEC-04
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
    // a bucket that holds nothing sealed yet would seal under a new key what the old one cannot open
    let empty = store.bucket::<String>("tokens").encrypted().open().unwrap_err();
    assert_eq!(empty.kind(), ErrorKind::Io, "{empty}");
    assert!(empty.to_string().contains("kv bucket tokens: the encryption key"), "{empty}");
    assert!(empty.to_string().contains("put the key back"), "{empty}");
    assert!(!key.exists(), "no key was made for a bucket with nothing sealed either");
    store.close().unwrap();

    fs::write(&key, kept).unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    assert_eq!(
        passwords(&store).get("source-42").unwrap().as_deref(),
        Some(PASSWORD),
        "found again, the key opens them"
    );
    let tokens = store.bucket::<String>("tokens").encrypted().open().unwrap();
    tokens.set("t-1", &"a-token".to_owned()).unwrap();
    assert_eq!(tokens.get("t-1").unwrap().as_deref(), Some("a-token"), "and seals under the one key");
}

// contract: SEC-04
#[test]
#[cfg(feature = "sql")]
fn a_key_lost_is_not_replaced_for_a_databases_bucket_while_kv_db_holds_values_sealed_with_it() {
    let f = fixture();
    passwords(&f.store).set("source-42", &PASSWORD.to_owned()).unwrap();
    f.store.close().unwrap();
    let key = f.dir.path().join("encryption.key");
    fs::remove_file(&key).unwrap();

    // the database's bucket opens first, and nothing has opened kv.db
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let db = store.database("app").open().unwrap();
    let lost = db.bucket::<String>("tokens").encrypted().open().unwrap_err();
    assert_eq!(lost.kind(), ErrorKind::Io, "{lost}");
    assert!(lost.to_string().contains("put the key back"), "{lost}");
    assert!(!key.exists(), "no key was made for the database's bucket");
}

// contract: SEC-04
#[test]
#[cfg(feature = "sql")]
fn a_key_lost_is_not_replaced_for_a_bucket_of_kv_db_while_a_database_holds_values_sealed_with_it() {
    let f = fixture();
    let db = f.store.database("app").open().unwrap();
    db.bucket::<String>("secrets").encrypted().open().unwrap().set("s-1", &PASSWORD.to_owned()).unwrap();
    drop(db);
    f.store.close().unwrap();
    let key = f.dir.path().join("encryption.key");
    let kept = fs::read_to_string(&key).unwrap();
    fs::remove_file(&key).unwrap();

    // kv.db's buckets open first, as a program starts, and the database later
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    let lost = store.bucket::<String>("tokens").encrypted().open().unwrap_err();
    assert_eq!(lost.kind(), ErrorKind::Io, "{lost}");
    assert!(lost.to_string().contains("put the key back"), "{lost}");
    assert!(!key.exists(), "no key was made for kv.db's bucket");
    store.close().unwrap();

    fs::write(&key, kept).unwrap();
    let store = Store::open(f.dir.path(), Options { background: false, ..Options::default() }).unwrap();
    store.bucket::<String>("tokens").encrypted().open().unwrap();
    let secrets = store.database("app").open().unwrap().bucket::<String>("secrets").encrypted().open().unwrap();
    assert_eq!(secrets.get("s-1").unwrap().as_deref(), Some(PASSWORD), "put back, the one key opens both files");
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
