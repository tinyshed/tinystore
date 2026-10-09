//! `tinystore serve` as its clients start it: frames written by hand, so that
//! the test speaks the protocol as an SDK does, not through the library.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use sha2::Sha256;

const BINARY: &str = env!("CARGO_BIN_EXE_tinystore");
const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const REQUEST: u8 = 3;
const RESPONSE: u8 = 4;
const END: u8 = 1;
const SERVER_STOP: u16 = 0x0001;

fn frame(kind: u8, flags: u8, method: u16, stream: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + body.len());
    bytes.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&[kind, flags]);
    bytes.extend_from_slice(&method.to_le_bytes());
    bytes.extend_from_slice(&stream.to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// A HELLO of protocol 2, `{1: 2}`, with a challenge when given, `{6: bin}`.
fn hello(challenge: Option<&[u8; 16]>) -> Vec<u8> {
    let body = match challenge {
        None => vec![0x81, 0x01, 0x02],
        Some(challenge) => [&[0x82, 0x01, 0x02, 0x06, 0xc4, 0x10][..], challenge].concat(),
    };
    frame(HELLO, 0, 0, 0, &body)
}

/// The next frame: its kind, flags, stream and body.
fn read_frame(reader: &mut impl Read) -> (u8, u8, u32, Vec<u8>) {
    let mut header = [0; 12];
    reader.read_exact(&mut header).unwrap();
    let length = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
    let stream = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (header[4], header[5], stream, body)
}

fn wait(child: &mut Child) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code().unwrap_or(-1);
        }
        assert!(Instant::now() < deadline, "the server did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_private_child_answers_hello_and_leaves_when_its_stdin_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(BINARY)
        .args(["serve", "--dir"])
        .arg(dir.path())
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&hello(None)).unwrap();
    let (kind, ..) = read_frame(child.stdout.as_mut().unwrap());
    assert_eq!(kind, WELCOME);
    drop(child.stdin.take());
    assert_eq!(wait(&mut child), 0, "the end of stdin asks it to leave");
}

#[test]
fn a_local_server_publishes_serve_proves_itself_stops_and_holds_its_directory_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = Command::new(BINARY)
        .args(["serve", "--dir"])
        .arg(dir.path())
        .args(["--local", "--idle", "0ms"])
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let serve = published(&dir.path().join("server").join("SERVE"));
    assert_eq!(serve["protocol"], 2);
    assert_eq!(serve["sidecar"], true);

    let held =
        Command::new(BINARY).args(["serve", "--dir"]).arg(dir.path()).arg("--local").stderr(Stdio::null()).status();
    assert_eq!(held.unwrap().code(), Some(3), "a second server finds the directory held");

    let mut client = connect(serve["endpoints"][0].as_str().unwrap());
    let challenge = *b"0123456789abcdef";
    client.write_all(&hello(Some(&challenge))).unwrap();
    let (kind, _, _, welcome) = read_frame(&mut client);
    assert_eq!(kind, WELCOME);
    let secret = base64url(serve["secret"].as_str().unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(&secret).unwrap();
    mac.update(&challenge);
    let proof = &welcome[welcome.len() - 35..];
    assert_eq!(&proof[..3], &[0x0b, 0xc4, 0x20], "the proof is WELCOME's last field, 32 bytes");
    assert_eq!(&proof[3..], &mac.finalize().into_bytes()[..]);

    client.write_all(&frame(REQUEST, END, SERVER_STOP, 1, &[0x80])).unwrap();
    let (kind, flags, stream, _) = read_frame(&mut client);
    assert_eq!((kind, flags & END, stream), (RESPONSE, END, 1));
    assert_eq!(wait(&mut server), 0);
    assert!(!dir.path().join("server").join("SERVE").exists(), "SERVE goes before the directory is let go");
}

/// `SERVE` once the server has written it.
fn published(path: &Path) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            return serde_json::from_str(&text).unwrap();
        }
        assert!(Instant::now() < deadline, "no SERVE");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn connect(endpoint: &str) -> std::os::unix::net::UnixStream {
    std::os::unix::net::UnixStream::connect(endpoint.strip_prefix("unix://").unwrap()).unwrap()
}

#[cfg(windows)]
fn connect(endpoint: &str) -> std::fs::File {
    let name = format!(r"\\.\pipe\{}", endpoint.strip_prefix("pipe:").unwrap());
    std::fs::OpenOptions::new().read(true).write(true).open(name).unwrap()
}

fn base64url(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let sextets: Vec<u32> =
        text.bytes().map(|byte| ALPHABET.iter().position(|letter| *letter == byte).unwrap() as u32).collect();
    let mut bytes = Vec::new();
    for chunk in sextets.chunks(4) {
        let joined = chunk.iter().enumerate().fold(0, |joined, (at, sextet)| joined | sextet << (18 - 6 * at));
        bytes.extend((0..chunk.len() - 1).map(|at| (joined >> (16 - 8 * at)) as u8));
    }
    bytes
}
