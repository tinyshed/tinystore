//! `tinystore kv set` and `delete` as a shell runs them: a key written
//! through the server that holds the directory, or a sidecar the command
//! starts, as the program that reads it back wrote it.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use tinystore::{ErrorKind, Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn tinystore(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    Command::new(BINARY).args(args).output().unwrap()
}

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The store once the server that held it has let it go: on Windows a
/// process holds its files until it has exited.
fn stopped(dir: &Path) -> Store {
    let stop = tinystore(&[&"stop", &dir]);
    assert!(stop.status.success(), "{}", said(&stop));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Store::open(dir, Options::default()) {
            Ok(store) => return store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

// contract: CLI-05
#[test]
fn a_key_set_from_a_shell_reads_back_as_the_type_it_was_given() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let text = tinystore(&[&"kv", &data, &"set", &"flags", &"zip", &"02134", &"--ttl", &"1h"]);
    assert!(said(&text).contains("set zip in bucket flags, expiring in 1h"), "{}", said(&text));
    let number = tinystore(&[&"kv", &data, &"set", &"counts", &"hits", &"42", &"--as", &"int"]);
    assert!(number.status.success(), "{}", said(&number));
    let json = tinystore(&[&"kv", &data, &"set", &"settings", &"42", &r#"{"theme": "dark"}"#, &"--as", &"json"]);
    assert!(json.status.success(), "{}", said(&json));
    let wrong = tinystore(&[&"kv", &data, &"set", &"counts", &"hits", &"many", &"--as", &"int"]);
    assert_eq!(wrong.status.code(), Some(2), "{}", said(&wrong));
    let gone = tinystore(&[&"kv", &data, &"delete", &"counts", &"misses"]);
    assert!(said(&gone).contains("bucket counts holds no key misses"), "{}", said(&gone));
    let store = stopped(&data);
    let zip = store.bucket::<String>("flags").open().unwrap().entry("zip").unwrap().unwrap();
    assert_eq!(zip.value, "02134", "nothing is guessed from how a value looks");
    assert!(zip.expires_at.is_some());
    assert_eq!(store.bucket::<i64>("counts").open().unwrap().get("hits").unwrap(), Some(42));
    let settings = store.bucket::<serde_json::Value>("settings").open().unwrap().get("42").unwrap();
    assert_eq!(settings, Some(serde_json::json!({"theme": "dark"})));
}

// contract: CLI-05
#[test]
fn an_encrypted_bucket_and_one_in_a_database_are_written_as_their_program_keeps_them() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options::default()).unwrap();
    store.bucket::<String>("secrets").encrypted().open().unwrap().set("old", &"sealed".to_owned()).unwrap();
    store.database("app").open().unwrap().bucket::<String>("drafts").open().unwrap();
    store.close().unwrap();
    let sealed = tinystore(&[&"kv", &data, &"set", &"secrets", &"token", &"abc"]);
    assert!(sealed.status.success(), "{}", said(&sealed));
    let draft = tinystore(&[&"kv", &data, &"set", &"drafts", &"7", &"kept in app.db", &"--database", &"app"]);
    assert!(draft.status.success(), "{}", said(&draft));
    let deleted = tinystore(&[&"kv", &data, &"delete", &"secrets", &"old"]);
    assert!(said(&deleted).contains("deleted old from bucket secrets"), "{}", said(&deleted));
    let store = stopped(&data);
    let secrets = store.bucket::<String>("secrets").encrypted().open().unwrap();
    assert_eq!((secrets.get("token").unwrap(), secrets.get("old").unwrap()), (Some("abc".to_owned()), None));
    let drafts = store.database("app").open().unwrap().bucket::<String>("drafts").open().unwrap();
    assert_eq!(drafts.get("7").unwrap(), Some("kept in app.db".to_owned()));
}
