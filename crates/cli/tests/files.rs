//! `tinystore files` as a shell runs it: a file put and deleted through the
//! server that holds the directory, or a sidecar the command starts, and
//! listed and written to a local path beside it.

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

/// The store once the server that held it has let it go.
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
fn a_file_is_put_listed_got_and_deleted_from_a_shell() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    // past what one message carries, so that it goes up in pieces
    let large: Vec<u8> = (0..3_000_000u32).map(|n| (n % 251) as u8).collect();
    let (avatar, dump) = (dirs.path().join("avatar.png"), dirs.path().join("dump.bin"));
    std::fs::write(&avatar, b"\x89PNG").unwrap();
    std::fs::write(&dump, &large).unwrap();
    let put = tinystore(&[&"files", &data, &"put", &"avatars", &"users/42/avatar.png", &avatar]);
    assert!(said(&put).contains("put users/42/avatar.png in files avatars, 4 B image/png"), "{}", said(&put));
    let put = tinystore(&[&"files", &data, &"put", &"avatars", &"dump.bin", &dump]);
    assert!(put.status.success(), "{}", said(&put));
    let listed = tinystore(&[&"files", &data, &"list", &"avatars", &"users/", &"--json"]);
    let lines: Vec<serde_json::Value> =
        String::from_utf8_lossy(&listed.stdout).lines().map(|line| line.parse().unwrap()).collect();
    assert_eq!(lines.len(), 1, "{}", said(&listed));
    assert_eq!((lines[0]["path"].as_str(), lines[0]["size"].as_u64()), (Some("users/42/avatar.png"), Some(4)));
    let back = dirs.path().join("back.bin");
    let got = tinystore(&[&"files", &data, &"get", &"avatars", &"dump.bin", &back]);
    assert!(got.status.success(), "{}", said(&got));
    assert!(std::fs::read(&back).unwrap() == large, "the bytes come back as they went");
    let again = tinystore(&[&"files", &data, &"get", &"avatars", &"dump.bin", &back]);
    assert!(!again.status.success() && said(&again).contains("--force"), "{}", said(&again));
    let deleted = tinystore(&[&"files", &data, &"delete", &"avatars", &"dump.bin"]);
    assert!(said(&deleted).contains("deleted dump.bin from files avatars"), "{}", said(&deleted));
    let none = tinystore(&[&"files", &data, &"list", &"photos"]);
    assert!(said(&none).contains("holds no files photos"), "a read beside makes no set: {}", said(&none));
    let store = stopped(&data);
    let files = store.files("avatars").open().unwrap();
    assert!(files.head("dump.bin").unwrap().is_none());
    assert_eq!(files.head("users/42/avatar.png").unwrap().unwrap().content_type, "image/png");
}
