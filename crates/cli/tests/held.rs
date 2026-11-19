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

/// A program that opened its store with `cli`, as a shell meets it while the
/// program runs: a write goes through its endpoint, `status` names it, and
/// `stop` leaves it to the program.
// contract: CLI-05
#[test]
fn a_command_writes_to_a_store_its_program_opened_with_cli() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options { cli: true, ..Options::default() }).unwrap();

    let job = r#"{"teacher": 4353}"#;
    let added = tinystore(&[&"jobs", &data, &"add", &"teacher-reviews", &job, &"--id", &"t-4353"]);
    assert!(added.status.success(), "{}", said(&added));
    let set = tinystore(&[&"kv", &data, &"set", &"flags", &"crawl", &"now"]);
    assert!(set.status.success(), "{}", said(&set));
    let queue = store.queue::<serde_json::Value>("teacher-reviews").open().unwrap();
    assert_eq!(queue.get("t-4353").unwrap().map(|job| job.value), Some(serde_json::json!({"teacher": 4353})));
    let flags = store.bucket::<String>("flags").open().unwrap();
    assert_eq!(flags.get("crawl").unwrap().as_deref(), Some("now"), "the program reads the shell's write");

    let status = tinystore(&[&"status", &data]);
    let pid = std::process::id();
    let held = format!("held by a program · pid {pid} · the command line may write");
    assert!(said(&status).contains(&held), "{}", said(&status));
    let status = tinystore(&[&"status", &data, &"--json"]);
    let status: serde_json::Value = String::from_utf8_lossy(&status.stdout).trim().parse().unwrap();
    let served = &status["served"];
    assert_eq!((served["embedded"].as_bool(), served["pid"].as_u64()), (Some(true), Some(u64::from(pid))), "{status}");
    let stopped = tinystore(&[&"stop", &data]);
    assert!(!stopped.status.success(), "{}", said(&stopped));
    assert!(said(&stopped).contains("it stops when that program closes it"), "{}", said(&stopped));
    assert!(data.join("server").join("SERVE").exists(), "the program still serves");

    store.close().unwrap();
    assert!(!data.join("server").join("SERVE").exists(), "SERVE left with the store");
}

// contract: CLI-05
#[test]
fn a_command_that_writes_refuses_a_store_its_program_opened_without_cli() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options::default()).unwrap();
    let refused = tinystore(&[&"kv", &data, &"set", &"flags", &"crawl", &"now"]);
    assert!(!refused.status.success(), "{}", said(&refused));
    assert!(said(&refused).contains("open it in that program with cli or a sidecar"), "{}", said(&refused));
    store.close().unwrap();
}
