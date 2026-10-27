//! A set of files kept in a database's file: a file and the row that names it
//! commit together or neither does, a rollback and a crash leave no bytes
//! behind, and a backup holds both.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::blobs::fixture::bytes;
use crate::sql::Database;
use crate::{Error, ErrorKind, Options, Result, Store, TestClock, sql};

const PLUGINS: &str = "create table plugins (file text primary key) strict";
/// Bytes past what a database's files keep inline, so that they are an object.
const PACKAGE: usize = 100 << 10;

struct Fixture {
    store: Store,
    clock: Arc<TestClock>,
    // Last: fields drop in order, and Windows removes no file the store still holds.
    dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)));
    let store = open(dir.path(), &clock);
    Fixture { dir, store, clock }
}

fn open(dir: &Path, clock: &Arc<TestClock>) -> Store {
    Store::open(dir, Options { clock: Some(clock.clone()), background: false, ..Options::default() }).unwrap()
}

impl Fixture {
    fn app(&self) -> Database {
        app(&self.store)
    }

    fn reopen(&mut self) {
        self.store.close().unwrap();
        self.store = open(self.dir.path(), &self.clock);
    }

    /// The files of `sql/app.blobs/objects/`.
    fn objects(&self) -> Vec<PathBuf> {
        objects_in(&self.dir.path().join("sql/app.blobs/objects"))
    }
}

fn app(store: &Store) -> Database {
    store.database("app").migrations([("0001_plugins.sql", PLUGINS)]).open().unwrap()
}

fn objects_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for fan in std::fs::read_dir(dir).into_iter().flatten() {
        for file in std::fs::read_dir(fan.unwrap().path()).unwrap() {
            found.push(file.unwrap().path());
        }
    }
    found
}

fn installed(db: &Database, name: &str) -> bool {
    db.one::<String>(sql!("select file from plugins where file = ?", name)).unwrap().is_some()
}

// contract: DUR-06
#[test]
fn a_file_and_the_row_that_names_it_commit_together_or_neither_does() {
    let f = fixture();
    let db = f.app();
    let packages = db.files("packages").open().unwrap();
    db.tx(|tx| -> Result<()> {
        tx.exec(sql!("insert into plugins (file) values (?)", "stripe-1.0.0.js"))?;
        let info = tx.with(&packages).put("stripe-1.0.0.js", bytes(PACKAGE))?;
        assert_eq!(info.size, PACKAGE as u64);
        assert!(tx.with(&packages).head("stripe-1.0.0.js")?.is_some(), "the transaction sees its own file");
        tx.with(&packages).put("readme.md", b"a note kept inline")?;
        Ok(())
    })
    .unwrap();
    assert!(installed(&db, "stripe-1.0.0.js"));
    let mut stripe = packages.get("stripe-1.0.0.js").unwrap().expect("committed with its row");
    assert_eq!(stripe.read_all().unwrap(), bytes(PACKAGE));
    assert_eq!(packages.get("readme.md").unwrap().unwrap().read_all().unwrap(), b"a note kept inline");
    assert_eq!(f.objects().len(), 1, "the package's bytes, beside the database's file");

    let failed = db.tx(|tx| -> Result<()> {
        tx.exec(sql!("insert into plugins (file) values (?)", "slack-1.0.0.js"))?;
        tx.with(&packages).put("slack-1.0.0.js", bytes(PACKAGE + 1))?;
        Err(Error::invalid("the install changed its mind"))
    });
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Invalid);
    assert!(!installed(&db, "slack-1.0.0.js"));
    assert!(packages.get("slack-1.0.0.js").unwrap().is_none());
    assert_eq!(f.objects().len(), 1, "the bytes the rollback took are gone at once");
}

#[test]
fn a_delete_a_copy_and_a_create_commit_with_the_rows_too() {
    let f = fixture();
    let db = f.app();
    let packages = db.files("packages").open().unwrap();
    packages.put("stripe-1.0.0.js", bytes(PACKAGE)).unwrap();
    db.tx(|tx| -> Result<()> {
        let packages = tx.with(&packages);
        packages.key("stripe-2.0.0.js").copy_from("stripe-1.0.0.js")?;
        packages.delete("stripe-1.0.0.js")?;
        assert!(!packages.create("stripe-2.0.0.js", b"another")?, "a create finds the copy");
        assert!(packages.key("upload.js").upload().is_err(), "an upload's bytes go in with a put");
        Ok(())
    })
    .unwrap();
    assert!(packages.head("stripe-1.0.0.js").unwrap().is_none());
    assert_eq!(packages.get("stripe-2.0.0.js").unwrap().unwrap().read_all().unwrap(), bytes(PACKAGE));
    let error = db.tx(|tx| tx.with(&f.store.files("avatars").open()?).put("a.png", b"png")).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Invalid, "files of blobs.db are outside a database's transaction: {error}");
}

// contract: DUR-06
#[test]
fn bytes_a_crash_left_past_what_the_file_reserved_go_at_the_next_open() {
    let mut f = fixture();
    let packages = f.app().files("packages").open().unwrap();
    packages.put("stripe-1.0.0.js", bytes(PACKAGE)).unwrap();
    // what a transaction that placed its bytes and died before its commit leaves
    let left = f.dir.path().join("sql/app.blobs/objects/fa1/fa1000");
    std::fs::create_dir_all(left.parent().unwrap()).unwrap();
    std::fs::write(&left, b"bytes no row names").unwrap();
    f.reopen();
    let packages = f.app().files("packages").open().unwrap();
    assert!(!left.exists());
    assert_eq!(packages.get("stripe-1.0.0.js").unwrap().unwrap().read_all().unwrap(), bytes(PACKAGE));
    assert_eq!(f.objects().len(), 1);
}

#[test]
fn a_databases_files_live_in_its_file_and_the_folder_beside_it() {
    let f = fixture();
    let db = f.app();
    let packages = db.files("packages").open().unwrap();
    packages.key("stripe-1.0.0.js").content_type("text/javascript").put(bytes(PACKAGE)).unwrap();
    assert!(!f.dir.path().join("blobs").exists(), "no blobs.db");
    let listed = packages.list().page().unwrap();
    assert_eq!(listed.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["stripe-1.0.0.js"]);
    let tables: i64 =
        db.scalar(sql!("select count(*) from sqlite_schema where name like '_tinystore_blobs_%'")).unwrap();
    assert!(tables > 0, "its tables beside the database's own");
}

// contract: BAK-01
#[cfg(feature = "backup")]
#[test]
fn a_backup_holds_a_databases_files_with_the_rows_that_name_them() {
    let f = fixture();
    let db = f.app();
    let packages = db.files("packages").open().unwrap();
    db.tx(|tx| -> Result<()> {
        tx.exec(sql!("insert into plugins (file) values (?)", "stripe-1.0.0.js"))?;
        tx.with(&packages).put("stripe-1.0.0.js", bytes(PACKAGE)).map(drop)
    })
    .unwrap();
    let zip = f.dir.path().with_extension("zip");
    f.store.backup().write(&zip).unwrap();
    let restored = tempfile::tempdir().unwrap();
    crate::backup::restore(&zip, restored.path().join("data")).unwrap();
    let again = open(&restored.path().join("data"), &f.clock);
    let db = app(&again);
    assert!(installed(&db, "stripe-1.0.0.js"));
    let mut stripe = db.files("packages").open().unwrap().get("stripe-1.0.0.js").unwrap().unwrap();
    assert_eq!(stripe.read_all().unwrap(), bytes(PACKAGE));
    let _ = std::fs::remove_file(&zip);
}
