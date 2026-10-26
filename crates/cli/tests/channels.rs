//! `tinystore channels` and `sql --write` as a shell runs them, through the
//! server that holds the directory: a message published to a subscription
//! listening there, and a statement written while a server holds the store.

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
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
fn a_message_published_from_a_shell_reaches_a_subscription_listening_now() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    Store::open(&data, Options::default()).unwrap().close().unwrap();
    let before = tinystore(&[&"channels", &data, &"publish", &"chat", &r#"{"n": 0}"#]);
    assert!(said(&before).contains("published to chat"), "{}", said(&before));
    let mut listening = Command::new(BINARY)
        .arg("channels")
        .arg(&data)
        .args(["subscribe", "chat", "--recursive", "--for", "30s"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(listening.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert!(line.contains("listening on chat"), "{line}");
    // a socket that timed its reads out once ended a subscription quiet for ten seconds
    #[cfg(unix)]
    std::thread::sleep(Duration::from_secs(11));
    for (message, under) in [(r#"{"n": 1}"#, None), (r#"{"n": 2}"#, Some("room-42"))] {
        let mut args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"channels", &data, &"publish", &"chat", &message];
        if let Some(under) = &under {
            args.extend([&"--under" as &dyn AsRef<std::ffi::OsStr>, under]);
        }
        let published = tinystore(&args);
        assert!(published.status.success(), "{}", said(&published));
    }
    let mut stdout = BufReader::new(listening.stdout.take().unwrap()).lines();
    let heard: Vec<String> = (0..2).map(|_| stdout.next().unwrap().unwrap()).collect();
    listening.kill().unwrap();
    listening.wait().unwrap();
    assert_eq!(heard, [r#"{"n":1}"#, r#"room-42  {"n":2}"#], "the message before it listened is gone");
    let lasting = Instant::now();
    let ended = tinystore(&[&"channels", &data, &"subscribe", &"chat", &"--for", &"1s"]);
    assert!(ended.status.success() && lasting.elapsed() < Duration::from_secs(20), "{}", said(&ended));
    stopped(&data);
}

// contract: CLI-05
#[test]
fn a_statement_writes_through_the_server_that_holds_the_store() {
    let dirs = tempfile::tempdir().unwrap();
    let data = dirs.path().join("data");
    let store = Store::open(&data, Options::default()).unwrap();
    store.database("app").open().unwrap().exec("create table users (id integer primary key, email text)").unwrap();
    store.close().unwrap();
    // a write of another command starts the server that holds the store
    let started = tinystore(&[&"kv", &data, &"set", &"flags", &"beta", &"on"]);
    assert!(started.status.success(), "{}", said(&started));
    let inserted =
        tinystore(&[&"sql", &data, &"app", &"insert into users (email) values ('ada@example.com')", &"--write"]);
    assert!(said(&inserted).contains("1 rows changed"), "{}", said(&inserted));
    let returning =
        tinystore(&[&"sql", &data, &"app", &"insert into users (email) values ('bob') returning id", &"--write"]);
    assert!(!returning.status.success() && said(&returning).contains("gives rows back"), "{}", said(&returning));
    let store = stopped(&data);
    let count: i64 = store.database("app").open().unwrap().scalar("select count(*) from users").unwrap();
    assert_eq!(count, 1);
}
