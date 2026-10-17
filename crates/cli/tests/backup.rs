//! `tinystore backup` and `tinystore restore` as a person runs them: a store's
//! directory as one zip, the zip back into an empty directory, and a
//! directory a program holds read beside it.

use std::path::Path;
use std::process::{Command, Output};

use tinystore::{Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args.iter().map(|arg| arg.as_ref())).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn filled(data: &Path) {
    let store = Store::open(data, Options::default()).unwrap();
    store.bucket::<String>("codes").open().unwrap().set("K7Q2", &"kept".to_owned()).unwrap();
    std::fs::write(data.join("secret.key"), "k3y").unwrap();
    store.close().unwrap();
}

#[test]
fn backup_writes_a_zip_that_restore_takes_back_into_an_empty_directory() {
    let dirs = tempfile::tempdir().unwrap();
    let (data, zip, restored) =
        (dirs.path().join("data"), dirs.path().join("backup.zip"), dirs.path().join("restored"));
    filled(&data);
    let backed = tinystore(&[&"backup", &data, &zip, &"--include", &"secret.key"]);
    assert!(backed.status.success(), "{}", said(&backed));
    assert!(said(&backed).contains("backed up to"), "{}", said(&backed));
    let restore = tinystore(&[&"restore", &zip, &restored]);
    assert!(restore.status.success(), "{}", said(&restore));
    let back = Store::open(&restored, Options::default()).unwrap();
    assert_eq!(back.bucket::<String>("codes").open().unwrap().get("K7Q2").unwrap().as_deref(), Some("kept"));
    assert_eq!(std::fs::read_to_string(restored.join("secret.key")).unwrap(), "k3y");
    drop(back);
    let again = tinystore(&[&"restore", &zip, &restored]);
    assert!(!again.status.success() && said(&again).contains("not empty"), "{}", said(&again));
}

#[test]
fn backup_reads_beside_a_program_that_holds_the_directory() {
    let dirs = tempfile::tempdir().unwrap();
    let (data, zip, restored) =
        (dirs.path().join("data"), dirs.path().join("backup.zip"), dirs.path().join("restored"));
    let held = Store::open(&data, Options::default()).unwrap();
    held.bucket::<String>("codes").open().unwrap().set("K7Q2", &"kept".to_owned()).unwrap();
    let backed = tinystore(&[&"backup", &data, &zip]);
    assert!(backed.status.success(), "{}", said(&backed));
    held.bucket::<String>("codes").open().unwrap().set("K7Q3", &"after".to_owned()).unwrap();
    let restore = tinystore(&[&"restore", &zip, &restored]);
    assert!(restore.status.success(), "{}", said(&restore));
    let back = Store::open(&restored, Options::default()).unwrap();
    let codes = back.bucket::<String>("codes").open().unwrap();
    assert_eq!(codes.get("K7Q2").unwrap().as_deref(), Some("kept"));
    assert_eq!(codes.get("K7Q3").unwrap(), None, "written after the backup");
    let usage = tinystore(&[&"backup", &data]);
    assert_eq!(usage.status.code(), Some(2), "{}", said(&usage));
}
