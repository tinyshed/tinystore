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

// contract: BAK-03
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

// contract: CLI-01
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

// contract: BAK-03
#[test]
fn backup_beside_a_holder_whose_key_is_kept_apart_says_it_leaves_the_key_out() {
    let dirs = tempfile::tempdir().unwrap();
    let (data, zip, key) =
        (dirs.path().join("data"), dirs.path().join("backup.zip"), dirs.path().join("tinystore.key"));
    std::fs::write(&key, "0f".repeat(32)).unwrap();
    let held = Store::open(&data, Options { encryption_key_file: Some(key.clone()), ..Options::default() }).unwrap();
    let passwords = held.bucket::<String>("passwords").encrypted().open().unwrap();
    passwords.set("source-42", &"hunter2".to_owned()).unwrap();

    let with_key = tinystore(&[&"backup", &data, &zip, &"--encryption-key-file", &key, &"--include-encryption-key"]);
    assert!(!with_key.status.success(), "a key asked for and not taken: {}", said(&with_key));
    assert!(said(&with_key).contains("takes no key from"), "{}", said(&with_key));
    assert!(!zip.exists(), "no zip without the key it was asked to take");

    let without = tinystore(&[&"backup", &data, &zip, &"--encryption-key-file", &key]);
    assert!(without.status.success(), "{}", said(&without));
    assert!(said(&without).contains("which the backup leaves out"), "{}", said(&without));

    // an operator who does not name the key file is told the same
    let (plain, asked, restored) =
        (dirs.path().join("plain.zip"), dirs.path().join("asked.zip"), dirs.path().join("restored"));
    let unnamed = tinystore(&[&"backup", &data, &plain]);
    assert!(unnamed.status.success(), "{}", said(&unnamed));
    assert!(said(&unnamed).contains("which the backup leaves out"), "{}", said(&unnamed));
    let include = tinystore(&[&"backup", &data, &asked, &"--include-encryption-key"]);
    assert!(!include.status.success(), "a key asked for and not there: {}", said(&include));
    assert!(said(&include).contains("values sealed with the key"), "{}", said(&include));
    assert!(!asked.exists(), "no zip without the key it was asked to take");
    let restore = tinystore(&[&"restore", &plain, &restored]);
    assert!(restore.status.success(), "{}", said(&restore));
    assert!(said(&restore).contains("put it back first"), "{}", said(&restore));
    held.close().unwrap();
}

// contract: CLI-01
#[test]
fn backup_of_a_directory_that_is_not_there_makes_no_store() {
    let dirs = tempfile::tempdir().unwrap();
    let (typo, zip) = (dirs.path().join("dta"), dirs.path().join("backup.zip"));
    let backed = tinystore(&[&"backup", &typo, &zip]);
    assert!(!backed.status.success(), "{}", said(&backed));
    assert!(said(&backed).contains("no store is there"), "{}", said(&backed));
    assert!(!typo.exists() && !zip.exists());
}
