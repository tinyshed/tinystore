//! A store opened with `cli` as the command line meets it: its `SERVE`, its
//! endpoint's proof and calls, its bound, and its close, which wakes every
//! thread it blocks and removes `SERVE` before it lets go of `LOCK`.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::wire::client::{Client, KvValue};
use crate::{ErrorKind, Options, Store};

fn with_cli(dir: &Path) -> Store {
    Store::open(dir, Options { cli: true, background: false, ..Options::default() }).unwrap()
}

fn published(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(dir.join("server").join("SERVE")).unwrap()).unwrap()
}

/// A byte stream to the endpoint `SERVE` names, as the command line dials it.
#[cfg(unix)]
fn dial(dir: &Path) -> std::os::unix::net::UnixStream {
    let endpoint = published(dir)["endpoints"][0].as_str().unwrap().to_owned();
    std::os::unix::net::UnixStream::connect(endpoint.strip_prefix("unix://").unwrap()).unwrap()
}

#[cfg(windows)]
fn dial(dir: &Path) -> fs::File {
    let endpoint = published(dir)["endpoints"][0].as_str().unwrap().to_owned();
    let name = format!(r"\\.\pipe\{}", endpoint.strip_prefix("pipe:").unwrap());
    fs::OpenOptions::new().read(true).write(true).open(name).unwrap()
}

/// A connection the endpoint welcomed, its proof checked against `SERVE`.
fn welcomed<S: Read + Write>(dir: &Path, stream: S) -> Client<S> {
    let challenge = [9; 16];
    let (client, welcome) = Client::hello(stream, "test", Some(challenge)).unwrap();
    let secret = published(dir)["secret"].as_str().unwrap().to_owned();
    let secret = super::super::from_base64url(&secret).unwrap();
    assert!(super::super::proven(&secret, &challenge, &welcome.proof.unwrap()), "the endpoint proves SERVE's secret");
    client
}

#[test]
fn a_store_opened_with_cli_publishes_serve_and_takes_the_command_lines_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let serve = published(dir.path());
    assert_eq!((serve["embedded"].as_bool(), serve.get("sidecar")), (Some(true), None), "{serve}");
    assert_eq!(serve["pid"].as_u64(), Some(u64::from(std::process::id())));

    let mut client = welcomed(dir.path(), dial(dir.path()));
    let bucket = client.bucket("sessions", None).unwrap();
    client.set(bucket, "ada", &KvValue::Text("signed in".to_owned()), None).unwrap();
    let sessions = store.bucket::<String>("sessions").open().unwrap();
    assert_eq!(sessions.get("ada").unwrap().as_deref(), Some("signed in"), "the program reads the shell's write");
    let refused = client.stop().unwrap_err();
    assert!(refused.to_string().contains("program decides when its server stops"), "{refused}");
    drop(client);
    store.close().unwrap();
}

#[test]
fn a_store_without_cli_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), Options::default()).unwrap();
    assert!(!dir.path().join("server").exists());
    store.close().unwrap();
}

#[test]
fn a_second_open_is_told_the_program_that_holds_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let refused = Store::open(dir.path(), Options::default()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::InUse, "{refused}");
    let said = format!("a program holds it inside its own process, pid {}", std::process::id());
    assert!(refused.to_string().contains(&said), "{refused}");
    store.close().unwrap();
}

#[test]
fn a_serve_a_dead_process_left_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("server")).unwrap();
    fs::write(dir.path().join("server").join("SERVE"), r#"{"protocol":1,"endpoints":["unix:///gone"]}"#).unwrap();
    let store = with_cli(dir.path());
    assert_eq!(published(dir.path())["embedded"].as_bool(), Some(true));
    drop(welcomed(dir.path(), dial(dir.path())));
    store.close().unwrap();
}

// contract: STORE-13
#[test]
fn a_close_removes_serve_before_it_lets_go_of_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let serve = dir.path().join("server").join("SERVE");
    let watched = dir.path().to_owned();
    let watching = std::thread::spawn(move || {
        loop {
            if let Ok(lock) = crate::store::take_lock(&watched) {
                drop(lock);
                return serve.exists();
            }
            std::thread::yield_now();
        }
    });
    std::thread::sleep(Duration::from_millis(50));
    store.close().unwrap();
    assert!(!watching.join().unwrap(), "SERVE was there when LOCK was let go");
    #[cfg(unix)]
    assert!(!dir.path().join("server").join("tinystore.sock").exists(), "the socket is removed too");
}

/// A store dropped with no close, a client still connected: the client's
/// connection holds the store until it ends, as a pipe does, and then the
/// store closes and lets go.
#[test]
fn a_store_dropped_while_a_client_is_connected_lets_go_once_the_client_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let mut client = welcomed(dir.path(), dial(dir.path()));
    drop(store);
    let bucket = client.bucket("sessions", None).unwrap();
    client.set(bucket, "ada", &KvValue::Int(1), None).unwrap();
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(10);
    let store = loop {
        match Store::open(dir.path(), Options::default()) {
            Ok(store) => break store,
            Err(error) if error.kind() == ErrorKind::InUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    };
    assert!(!dir.path().join("server").join("SERVE").exists(), "the endpoint left with the store");
    assert_eq!(store.bucket::<i64>("sessions").open().unwrap().get("ada").unwrap(), Some(1));
    store.close().unwrap();
}

#[test]
fn a_close_wakes_an_endpoint_no_client_reached() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    std::thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    store.close().unwrap();
    assert!(began.elapsed() < Duration::from_secs(1), "the accept held the close {:?}", began.elapsed());
    assert!(!dir.path().join("server").join("SERVE").exists());
}

/// A client welcomed that says nothing more, its endpoint's read blocked:
/// the close tells it GOAWAY and ends at once, since no call runs.
#[test]
fn a_close_wakes_a_read_waiting_for_a_silent_client() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let mut client = welcomed(dir.path(), dial(dir.path()));
    std::thread::sleep(Duration::from_millis(100));
    let began = Instant::now();
    store.close().unwrap();
    assert!(began.elapsed() < Duration::from_secs(1), "the read held the close {:?}", began.elapsed());
    assert!(client.bucket("sessions", None).is_err(), "the connection ended with the store");
}

/// A client that sends PINGs and never reads their PONGs: the endpoint's
/// write waits for room the client never makes, and its read for a pipe the
/// writer never empties. The close cuts both off once its two seconds pass.
#[test]
fn a_close_wakes_a_write_to_a_client_that_never_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let mut sending = dial(dir.path());
    let hello = frame(1, &[0x81, 0x01, 0x01]);
    sending.write_all(&hello).unwrap();
    let pings: Vec<u8> = (0..200_000).flat_map(|_| frame(8, &[7; 8])).collect();
    let sender = std::thread::spawn(move || {
        let mut sent = 0;
        while sent < pings.len() {
            match sending.write(&pings[sent..(sent + (64 << 10)).min(pings.len())]) {
                Ok(written) if written > 0 => sent += written,
                _ => return sent,
            }
        }
        sent
    });
    std::thread::sleep(Duration::from_millis(500));
    let began = Instant::now();
    store.close().unwrap();
    let took = began.elapsed();
    assert!(took < Duration::from_secs(4), "the blocked write held the close {took:?}");
    let _ = sender.join();
}

/// Windows' own is beside the DACL's code, in `windows_tests.rs`.
// contract: SEC-07
#[cfg(unix)]
#[test]
fn a_cli_endpoint_is_the_users_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let store = with_cli(dir.path());
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.path().join("server")), 0o700);
    assert_eq!(mode(&dir.path().join("server").join("SERVE")), 0o600);
    let socket = published(dir.path())["endpoints"][0].as_str().unwrap().trim_start_matches("unix://").to_owned();
    assert!(Path::new(&socket).starts_with(dir.path().join("server")), "the socket is inside server/: {socket}");
    store.close().unwrap();
}

#[test]
fn a_fifth_connection_is_turned_away_while_four_are_served() {
    let dir = tempfile::tempdir().unwrap();
    let store = with_cli(dir.path());
    let served: Vec<_> = (0..super::MOST).map(|_| welcomed(dir.path(), dial(dir.path()))).collect();
    let refused = Client::hello(dial(dir.path()), "test", Some([0; 16])).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Limit, "{refused}");
    assert!(refused.to_string().contains("4 command line connections"), "{refused}");
    drop(served);
    let deadline = Instant::now() + Duration::from_secs(5);
    let again = loop {
        match Client::hello(dial(dir.path()), "test", Some([0; 16])) {
            Ok((client, _)) => break client,
            Err(error) if Instant::now() < deadline => {
                assert_eq!(error.kind(), ErrorKind::Limit, "{error}");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{error}"),
        }
    };
    drop(again);
    store.close().unwrap();
}

/// A frame as docs/wire.md spells it, on stream 0: its header, then its body.
fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend_from_slice(&[kind, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(body);
    bytes
}
