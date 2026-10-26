//! A backup of a whole store and its restore: every engine back as it was,
//! every byte checked, the program's files and the store's key only when
//! asked for, and nothing left by a backup or a restore that fails.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::*;
use crate::metrics::Series;
use crate::text::hex;
use crate::{ErrorKind, Options, TestClock, sql};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Email {
    to: String,
}

const NOTES: &str = "create table notes (id integer primary key, text text not null) strict";
/// Bytes past what blobs keeps inline, so that they are a file of `objects/`.
const PHOTO: usize = 100 << 10;

struct Fixture {
    _dirs: tempfile::TempDir,
    data: PathBuf,
    zip: PathBuf,
    restored: PathBuf,
    clock: Arc<TestClock>,
}

fn fixture() -> Fixture {
    let dirs = tempfile::tempdir().unwrap();
    let (data, zip, restored) =
        (dirs.path().join("data"), dirs.path().join("backup.zip"), dirs.path().join("restored"));
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    Fixture { _dirs: dirs, data, zip, restored, clock }
}

impl Fixture {
    fn open(&self, dir: &Path) -> Store {
        Store::open(dir, Options { clock: Some(self.clock.clone()), background: false, ..Options::default() }).unwrap()
    }

    /// A store with something in every engine.
    fn filled(&self) -> Store {
        let store = self.open(&self.data);
        store.bucket::<String>("codes").open().unwrap().set("K7Q2", &"kept".to_owned()).unwrap();
        let emails = store.queue::<Email>("emails").open().unwrap();
        emails.id("e-1").add(&Email { to: "ann@example.com".to_owned() }).unwrap();
        let app = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
        app.exec(sql!("insert into notes (text) values (?)", "a note")).unwrap();
        store.files("photos").open().unwrap().put("cat.jpg", photo()).unwrap();
        store.events("audit").open().unwrap().append("role.changed", &serde_json::json!({ "user": 42 })).unwrap();
        let sample = Series::gauge("temperature").label("room", "kitchen").sample(store.now(), 21.5);
        store.metrics().unwrap().ingest(&[sample]).unwrap();
        store.index::<String>("notes").open().unwrap().set("n-1", &"sourdough starter".to_owned()).unwrap();
        store
    }

    /// Checks that a store holds what `filled` wrote.
    fn holds_everything(&self, store: &Store) {
        assert_eq!(store.bucket::<String>("codes").open().unwrap().get("K7Q2").unwrap().as_deref(), Some("kept"));
        let email = store.queue::<Email>("emails").open().unwrap().get("e-1").unwrap().expect("the job kept");
        assert_eq!(email.value.to, "ann@example.com");
        let app = store.database("app").migrations([("0001_notes.sql", NOTES)]).open().unwrap();
        assert_eq!(app.scalar::<String>(sql!("select text from notes")).unwrap(), "a note");
        let mut cat = store.files("photos").open().unwrap().get("cat.jpg").unwrap().expect("the file kept");
        assert_eq!(cat.read_all().unwrap(), photo());
        let events = store.events("audit").open().unwrap().list().page().unwrap().events;
        assert_eq!(events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(), ["role.changed"]);
        let read = store.metrics().unwrap().query().name("temperature").since(Duration::from_secs(3600)).read();
        assert_eq!(read.unwrap().series[0].samples()[0].value, 21.5);
        let notes = store.index::<String>("notes").open().unwrap();
        let hits = notes.search("starter").run().unwrap().hits;
        assert_eq!(hits.iter().map(|hit| hit.id.as_str()).collect::<Vec<_>>(), ["n-1"]);
    }
}

fn photo() -> Vec<u8> {
    (0..PHOTO).map(|at| (at % 251) as u8).collect()
}

/// The names of what a directory holds, its folders' too, `/` between parts.
fn tree(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                folders.push(path);
            } else {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

// contract: BAK-01
#[test]
fn a_backup_restores_every_engine_while_its_store_keeps_working() {
    let f = fixture();
    let store = f.filled();
    let backup = store.backup().write(&f.zip).unwrap();
    assert_eq!((backup.key_left_out, backup.files), (false, 8), "an engine's file each, a database's, a photo's bytes");
    store.bucket::<String>("codes").open().unwrap().set("after", &"not in it".to_owned()).unwrap();
    assert!(!tree(&f.data).iter().any(|name| name.starts_with(".snapshot-")), "the snapshot is gone");
    drop(store);

    assert_eq!(restore(&f.zip, &f.restored).unwrap(), backup);
    let restored = f.open(&f.restored);
    f.holds_everything(&restored);
    assert_eq!(restored.bucket::<String>("codes").open().unwrap().get("after").unwrap(), None);
}

#[test]
fn a_restored_records_and_metrics_file_gives_its_free_pages_back_as_its_store_made_it() {
    let f = fixture();
    let store = f.filled();
    store.backup().write(&f.zip).unwrap();
    store.close().unwrap();
    restore(&f.zip, &f.restored).unwrap();
    for name in ["records.db", "metrics.db"] {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let connection = rusqlite::Connection::open_with_flags(f.restored.join(name), flags).unwrap();
        let mode: i64 = connection.query_row("pragma auto_vacuum", [], |row| row.get(0)).unwrap();
        assert_eq!(mode, 2, "{name}: incremental, as its store made it");
    }
}

// contract: BAK-01
#[test]
fn an_engine_no_handle_opened_is_backed_up_from_its_files() {
    let f = fixture();
    f.filled().close().unwrap();
    let store = f.open(&f.data);
    let backup = store.backup().write(&f.zip).unwrap();
    assert_eq!(backup.files, 8);
    drop(store);
    restore(&f.zip, &f.restored).unwrap();
    f.holds_everything(&f.open(&f.restored));
}

// contract: BAK-02
#[test]
fn a_changed_byte_is_refused_and_leaves_nothing() {
    let f = fixture();
    f.filled().backup().write(&f.zip).unwrap();
    let mut zip = fs::read(&f.zip).unwrap();
    let at = zip.windows(4).position(|bytes| bytes == b"SQLi").expect("an SQLite file's header") + 200;
    zip[at] ^= 0xff;
    fs::write(&f.zip, &zip).unwrap();
    let error = restore(&f.zip, &f.restored).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    assert!(tree(&f.restored).is_empty(), "{:?}", tree(&f.restored));
}

// contract: BAK-02
#[test]
fn a_restore_writes_into_an_empty_directory_and_none_other() {
    let f = fixture();
    f.filled().backup().write(&f.zip).unwrap();
    fs::create_dir_all(&f.restored).unwrap();
    fs::write(f.restored.join("left.txt"), "x").unwrap();
    let error = restore(&f.zip, &f.restored).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "{error}");
    assert_eq!(tree(&f.restored), ["left.txt"], "what was there stays");
}

// contract: BAK-03
#[test]
fn a_file_of_the_programs_is_kept_only_when_named_and_one_of_the_stores_never() {
    let f = fixture();
    let store = f.filled();
    fs::write(f.data.join("secret.key"), "k3y").unwrap();
    fs::create_dir_all(f.data.join("keys")).unwrap();
    fs::write(f.data.join("keys/signing.pem"), "pem").unwrap();
    store.backup().write(&f.zip).unwrap();
    restore(&f.zip, &f.restored).unwrap();
    assert!(!f.restored.join("secret.key").exists(), "a file of the program's is left out unless named");

    fs::remove_dir_all(&f.restored).unwrap();
    fs::remove_file(&f.zip).unwrap();
    store.backup().include("secret.key").include("keys/signing.pem").write(&f.zip).unwrap();
    restore(&f.zip, &f.restored).unwrap();
    assert_eq!(fs::read_to_string(f.restored.join("secret.key")).unwrap(), "k3y");
    assert_eq!(fs::read_to_string(f.restored.join("keys/signing.pem")).unwrap(), "pem");

    for name in ["../outside", "/etc/passwd", "a\\b", "kv.db", "kv.db-wal", "LOCK", "manifest.json", "sql/app.db"] {
        let error = store.backup().include(name).write(f.zip.with_extension("refused")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Invalid, "{name}: {error}");
    }
    let error = store.backup().include("encryption.key").write(f.zip.with_extension("refused")).unwrap_err();
    assert!(error.to_string().contains("include_encryption_key()"), "{error}");
}

// contract: BAK-04
#[test]
fn a_backup_that_fails_leaves_no_zip() {
    let f = fixture();
    let store = f.filled();
    let error = store.backup().include("missing.txt").write(&f.zip).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io, "{error}");
    let beside: Vec<String> =
        tree(f.zip.parent().unwrap()).into_iter().filter(|name| name.contains("backup.zip")).collect();
    assert!(beside.is_empty(), "{beside:?}");
    assert!(!tree(&f.data).iter().any(|name| name.starts_with(".snapshot-")), "the snapshot is gone");
}

// contract: BAK-03
#[test]
fn the_encryption_key_is_left_out_unless_asked_for_and_the_backup_says_so() {
    let f = fixture();
    let store = f.open(&f.data);
    let passwords = store.bucket::<String>("passwords").encrypted().open().unwrap();
    passwords.set("db", &"hunter2".to_owned()).unwrap();
    let without = store.backup().write(&f.zip).unwrap();
    assert!(without.key_left_out);
    restore(&f.zip, &f.restored).unwrap();
    assert!(!f.restored.join(KEY).exists());
    let error = f.open(&f.restored).bucket::<String>("passwords").encrypted().open().unwrap_err();
    assert!(error.to_string().contains("encrypted with the key"), "a bucket whose key is not there says so: {error}");

    let (zip, restored) = (f.zip.with_extension("keyed"), f.restored.with_extension("keyed"));
    let with = store.backup().include_encryption_key().write(&zip).unwrap();
    assert!(!with.key_left_out);
    restore(&zip, &restored).unwrap();
    let back = f.open(&restored);
    let passwords = back.bucket::<String>("passwords").encrypted().open().unwrap();
    assert_eq!(passwords.get("db").unwrap().as_deref(), Some("hunter2"));
}

// contract: BAK-03
#[test]
fn a_backup_beside_a_holder_that_keeps_its_key_apart_says_it_leaves_the_key_out() {
    let f = fixture();
    let key = f.zip.with_file_name("tinystore.key");
    fs::write(&key, "0f".repeat(32)).unwrap();
    let options = Options { encryption_key_file: Some(key), background: false, ..Options::default() };
    let held = Store::open(&f.data, options).unwrap();
    let db = held.database("app").open().unwrap();
    db.bucket::<String>("passwords").encrypted().open().unwrap().set("db", &"hunter2".to_owned()).unwrap();

    // beside its holder, the store reads a key in its directory alone, and finds none
    let beside = Store::open_beside(&f.data).unwrap();
    let asked = beside.backup().include_encryption_key().write(&f.zip).unwrap_err();
    assert_eq!(asked.kind(), ErrorKind::Io, "{asked}");
    assert!(asked.to_string().contains("values sealed with the key"), "{asked}");
    assert!(!f.zip.exists(), "no zip without the key it was asked to take");
    let without = beside.backup().write(&f.zip).unwrap();
    assert!(without.key_left_out, "the backup says the values it holds open with a key it does not");
    assert!(restore(&f.zip, &f.restored).unwrap().key_left_out, "and its restore says so too");
    held.close().unwrap();
}

// contract: BAK-03
#[test]
fn a_backup_of_a_store_that_seals_nothing_misses_no_key() {
    let f = fixture();
    let store = f.open(&f.data);
    store.bucket::<String>("tokens").encrypted().open().unwrap();
    assert!(f.data.join(KEY).exists(), "the key is made for the encrypted bucket");
    assert!(!store.backup().write(&f.zip).unwrap().key_left_out, "nothing it holds needs the key");
}

// contract: BAK-04
#[test]
fn a_closed_store_takes_no_backup() {
    let f = fixture();
    let store = f.filled();
    let again = store.clone();
    store.close().unwrap();
    let error = again.backup().write(&f.zip).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Closed, "{error}");
    assert!(!f.zip.exists());
}

// contract: BAK-01
#[test]
fn a_backup_copies_while_a_writer_writes_and_holds_a_moment_of_it() {
    let f = fixture();
    let store = f.filled();
    let codes = store.bucket::<i64>("counted").open().unwrap();
    let writer = {
        let codes = codes.clone();
        std::thread::spawn(move || (0..300).for_each(|n| codes.set(format!("k{n:03}"), &n).unwrap()))
    };
    store.backup().write(&f.zip).unwrap();
    writer.join().unwrap();
    drop(store);
    restore(&f.zip, &f.restored).unwrap();
    let restored = f.open(&f.restored);
    let counted = restored.bucket::<i64>("counted").open().unwrap();
    let mut held = 0;
    for n in 0..300 {
        match counted.get(format!("k{n:03}")).unwrap() {
            Some(value) => {
                assert_eq!((value, n), (n, held), "the writes before the copy's moment, in order");
                held += 1;
            }
            None => break,
        }
    }
    assert!((held..300).all(|n| counted.get(format!("k{n:03}")).unwrap().is_none()), "a moment: nothing after a gap");
}

/// A collector's cursor in the program's database, saying how far its samples
/// in metrics.db go.
const CURSOR: &str = "create table cursor (id integer primary key, upto integer not null) strict; \
                      insert into cursor values (1, -1)";

// contract: BAK-01
#[test]
fn a_backup_is_one_moment_of_the_store_so_a_cursor_names_only_samples_it_holds() {
    for metrics_first in [false, true] {
        let f = fixture();
        let store = f.open(&f.data);
        // the files were copied in the order their engines opened
        let early = metrics_first.then(|| store.metrics().unwrap());
        let db = store.database("app").migrations([("0001_cursor.sql", CURSOR)]).open().unwrap();
        let stats = early.unwrap_or_else(|| store.metrics().unwrap());
        let base = store.now() - Duration::from_secs(3600);
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for i in 0_i64.. {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let at = base + Duration::from_millis(i as u64);
                    stats.ingest(&[Series::gauge("reading").sample(at, i as f64)]).unwrap();
                    db.exec(sql!("update cursor set upto = ? where id = 1", i)).unwrap();
                }
            });
            for n in 0..4 {
                std::thread::sleep(Duration::from_millis(30));
                let (zip, restored) = (f.zip.with_extension(format!("{n}.zip")), f.restored.join(n.to_string()));
                store.backup().write(&zip).unwrap();
                restore(&zip, &restored).unwrap();
                let again = f.open(&restored);
                let app = again.database("app").migrations([("0001_cursor.sql", CURSOR)]).open().unwrap();
                let cursor: i64 = app.scalar(sql!("select upto from cursor where id = 1")).unwrap();
                let read = again.metrics().unwrap().query().name("reading").since(Duration::from_secs(7200)).read();
                let kept = read.unwrap().series.iter().flat_map(|series| series.samples().to_vec()).count() as i64;
                assert!(
                    kept == cursor + 1 || kept == cursor + 2,
                    "metrics first {metrics_first}, backup {n}: the cursor says {cursor}, and {kept} samples are kept"
                );
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }
}

// contract: BAK-02
#[test]
fn a_file_whose_sha256_is_not_its_manifests_is_refused_though_its_crc_is_the_zips() {
    let f = fixture();
    let (bytes, other) = (b"kv's bytes".to_vec(), b"kv's other".to_vec());
    let listed = manifest::Listed {
        name: "kv.db".to_owned(),
        engine: "kv",
        size: other.len() as u64,
        sha256: hex(&Sha256::digest(&other)),
    };
    let manifest = Manifest { created: 0, files: vec![listed], key_left_out: false }.to_json();
    let mut writer = zip::Writer::new(Vec::new());
    writer.add("kv.db", bytes.len() as u64, bytes.as_slice(), |_| {}).unwrap();
    writer.add(MANIFEST, manifest.len() as u64, manifest.as_bytes(), |_| {}).unwrap();
    fs::write(&f.zip, writer.finish().unwrap()).unwrap();
    let error = restore(&f.zip, &f.restored).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    assert!(error.to_string().contains("kv.db: its SHA-256 is not its manifest's"), "{error}");
    assert!(tree(&f.restored).is_empty());
}

#[test]
fn an_archive_shorter_than_a_zip_end_is_corrupt() {
    let f = fixture();
    for length in [0, 1, 3, 21] {
        fs::write(&f.zip, vec![b'P'; length]).unwrap();
        let error = restore(&f.zip, &f.restored).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Corrupt, "{length} bytes: {error}");
    }
}
