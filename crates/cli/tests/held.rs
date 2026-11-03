//! The commands that open a store themselves, `migrate` and `search drop`,
//! as a shell runs them while a program holds the directory: each refuses,
//! naming who holds it, and changes nothing.

use std::process::{Command, Output};

use tinystore::{Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

// contract: CLI-05
#[test]
fn a_command_that_opens_the_store_itself_refuses_a_directory_a_program_holds() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let migrations = dirs.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(migrations.join("0001_users.sql"), "create table users (id integer primary key);").unwrap();
    let store = Store::open(&data, Options::default()).unwrap();

    let migrated = tinystore(&[&"migrate", &data, &"app", &migrations]);
    assert!(!migrated.status.success() && said(&migrated).contains("holds"), "{}", said(&migrated));
    let dropped = tinystore(&[&"search", &"drop", &data, &"notes"]);
    assert!(!dropped.status.success() && said(&dropped).contains("holds"), "{}", said(&dropped));

    let db = store.database("app").open().unwrap();
    let users = db.scalar::<i64>("select count(*) from sqlite_master where name = 'users'").unwrap();
    assert_eq!(users, 0, "the refused migration applied nothing");
    store.close().unwrap();
}
