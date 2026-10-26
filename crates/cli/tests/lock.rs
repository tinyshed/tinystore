//! `tinystore lock hold` as a shell runs it: a command run while it holds a
//! key, through the server that holds the directory, another hold refused
//! meanwhile, and the command stopped once its lease is lost.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tinystore::{Options, Store};

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");

fn said(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// A command that prints its fence, sleeps `seconds` and exits 7.
#[cfg(unix)]
fn sleeping(seconds: u32) -> Vec<String> {
    let script = format!("echo fence $TINYSTORE_FENCE; sleep {seconds}; exit 7");
    vec!["sh".into(), "-c".into(), script]
}

#[cfg(windows)]
fn sleeping(seconds: u32) -> Vec<String> {
    let script = format!("Write-Output \"fence $env:TINYSTORE_FENCE\"; Start-Sleep {seconds}; exit 7");
    vec!["powershell".into(), "-NoProfile".into(), "-Command".into(), script]
}

fn hold(dir: &std::path::Path, flags: &[&str], seconds: u32) -> Command {
    let mut command = Command::new(BINARY);
    command.arg("lock").arg(dir).args(["hold", "cron", "nightly"]).args(flags).arg("--").args(sleeping(seconds));
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

// contract: CLI-08
#[test]
fn a_command_runs_while_it_holds_the_key_and_ends_with_its_own_exit() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let first = hold(&data, &["--ttl", "1s"], 4).spawn().unwrap();
    // the first hold has the key once its command has said its fence
    let deadline = Instant::now() + Duration::from_secs(20);
    let refused = loop {
        let second = hold(&data, &["--wait", "0"], 0).output().unwrap();
        if said(&second).contains("is held by another") || Instant::now() > deadline {
            break second;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(refused.status.code(), Some(1), "{}", said(&refused));
    assert!(!said(&refused).contains("fence"), "a hold refused runs nothing: {}", said(&refused));
    let ended = first.wait_with_output().unwrap();
    assert_eq!(ended.status.code(), Some(7), "renewed past its ttl, it ends as its command did: {}", said(&ended));
    assert!(said(&ended).contains("fence "), "{}", said(&ended));
    let stop = Command::new(BINARY).arg("stop").arg(&data).output().unwrap();
    assert!(stop.status.success(), "{}", said(&stop));
}

// contract: CLI-08
#[test]
fn a_command_whose_lease_is_lost_is_stopped() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let began = Instant::now();
    let lost = hold(&data, &["--ttl", "1s", "--max-hold", "2s"], 60).output().unwrap();
    assert_eq!(lost.status.code(), Some(1), "{}", said(&lost));
    assert!(said(&lost).contains("the lease was lost"), "{}", said(&lost));
    assert!(began.elapsed() < Duration::from_secs(30), "stopped, not waited for: {:?}", began.elapsed());
    let stop = Command::new(BINARY).arg("stop").arg(&data).output().unwrap();
    assert!(stop.status.success(), "{}", said(&stop));
}
