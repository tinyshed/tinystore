//! The least of a client of the protocol that `status` and `stop` need: the
//! server `<dir>/server/SERVE` names, reached on its local endpoint, its
//! proof checked, and `server.stop` asked. Frames are written by hand, as
//! docs/wire.md spells them, since the library keeps its codecs to itself.
//!
//! ```text
//! HELLO    {1: 2, 2: "tinystore-cli/0.1.0", 6: <16 random bytes>}
//! WELCOME  {2: server's version, 11: HMAC-SHA256(challenge) keyed with SERVE's secret, …}
//! REQUEST  method 0x0001 server.stop, stream 1, END, {}
//! RESPONSE stream 1, END: {} or a failure
//! ```

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, Mac};
use sha2::Sha256;

const HELLO: u8 = 1;
const WELCOME: u8 = 2;
const REQUEST: u8 = 3;
const RESPONSE: u8 = 4;
const GOAWAY: u8 = 10;
const END: u8 = 1;
const ERROR: u8 = 2;
const SERVER_STOP: u16 = 0x0001;
/// The largest frame body this client reads: a WELCOME or a failure is far
/// smaller.
const MOST: usize = 1 << 20;

/// What `SERVE` says of the server that published it.
#[derive(Debug)]
pub(crate) struct Found {
    pub(crate) pid: u64,
    pub(crate) endpoints: Vec<String>,
    /// When it was written: when the server began listening.
    pub(crate) since: SystemTime,
    secret: Vec<u8>,
}

/// The server `<dir>/server/SERVE` names, or none when there is no `SERVE`.
pub(crate) fn found(dir: &Path) -> Result<Option<Found>, String> {
    let path = dir.join("server").join("SERVE");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let unreadable = |why: &str| format!("{}: {why}", path.display());
    let serve: serde_json::Value = serde_json::from_str(&text).map_err(|error| unreadable(&error.to_string()))?;
    let field = |name: &str| serve.get(name).ok_or_else(|| unreadable(&format!("no {name}")));
    let endpoints = field("endpoints")?.as_array().ok_or_else(|| unreadable("endpoints that are no list"))?;
    let since = std::fs::metadata(&path).and_then(|metadata| metadata.modified()).unwrap_or_else(|_| SystemTime::now());
    Ok(Some(Found {
        pid: field("pid")?.as_u64().unwrap_or_default(),
        endpoints: endpoints.iter().filter_map(|endpoint| endpoint.as_str().map(str::to_owned)).collect(),
        since,
        secret: base64url(field("secret")?.as_str().unwrap_or_default())
            .ok_or_else(|| unreadable("a secret that is no base64url"))?,
    }))
}

/// A byte stream to the server, both ways.
trait Stream: Read + Write {}

impl<T: Read + Write> Stream for T {}

/// A connection the server welcomed, its proof checked.
pub(crate) struct Connection {
    stream: Box<dyn Stream>,
    /// The server's version, as its WELCOME says.
    pub(crate) server: String,
}

impl Found {
    /// Reaches the server on its local endpoint and says HELLO: a server
    /// gone that left `SERVE` behind is an error, and so is a process on its
    /// endpoint that cannot prove it read `SERVE`.
    pub(crate) fn connect(&self) -> Result<Connection, String> {
        let endpoint = self.endpoints.first().ok_or("SERVE names no endpoint")?;
        let mut stream = dial(endpoint).map_err(|error| format!("{endpoint}: {error}"))?;
        let mut challenge = [0; 16];
        getrandom::fill(&mut challenge).map_err(|error| error.to_string())?;
        let client = format!("tinystore-cli/{}", env!("CARGO_PKG_VERSION"));
        let mut hello = vec![0x83, 0x01, 0x02, 0x02];
        hello.extend(str_of(&client));
        hello.extend([0x06, 0xc4, 0x10]);
        hello.extend(challenge);
        send(&mut stream, HELLO, 0, 0, 0, &hello).map_err(|error| error.to_string())?;
        let (kind, _, _, body) = receive(&mut stream).map_err(|error| format!("{endpoint}: {error}"))?;
        match kind {
            WELCOME => {}
            GOAWAY => {
                return Err(format!(
                    "the server would not take a connection: {}",
                    text_at(&body, 2).unwrap_or_default()
                ));
            }
            other => return Err(format!("a frame of kind {other} where WELCOME belongs")),
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes a key of any length");
        mac.update(&challenge);
        let proof = bin_at(&body, 11).ok_or("the server gave no proof")?;
        mac.verify_slice(&proof).map_err(|_| "a process on the endpoint did not prove it is the store's server")?;
        Ok(Connection { stream, server: text_at(&body, 2).unwrap_or_default() })
    }
}

impl Connection {
    /// Asks the server to stop, and returns once it agreed.
    pub(crate) fn stop(mut self) -> Result<(), String> {
        send(&mut self.stream, REQUEST, END, SERVER_STOP, 1, &[0x80]).map_err(|error| error.to_string())?;
        loop {
            let (kind, flags, stream, body) = receive(&mut self.stream).map_err(|error| error.to_string())?;
            match (kind, stream) {
                (RESPONSE, 1) if flags & ERROR != 0 => {
                    return Err(text_at(&body, 2).unwrap_or_else(|| "the server refused".to_owned()));
                }
                (RESPONSE, 1) => return Ok(()),
                (GOAWAY, _) => return Ok(()),
                _ => continue,
            }
        }
    }
}

#[cfg(unix)]
fn dial(endpoint: &str) -> io::Result<Box<dyn Stream>> {
    let path = endpoint.strip_prefix("unix://").ok_or_else(|| io::Error::other("not a local socket"))?;
    let stream = std::os::unix::net::UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    Ok(Box::new(stream))
}

/// Opens the server's named pipe, waiting a little while every instance is
/// busy with another client.
#[cfg(windows)]
fn dial(endpoint: &str) -> io::Result<Box<dyn Stream>> {
    let name = endpoint.strip_prefix("pipe:").ok_or_else(|| io::Error::other("not a named pipe"))?;
    let path = format!(r"\\.\pipe\{name}");
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(pipe) => return Ok(Box::new(pipe)),
            // ERROR_PIPE_BUSY: every instance has a client
            Err(error) if error.raw_os_error() == Some(231) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

fn send(stream: &mut dyn Stream, kind: u8, flags: u8, method: u16, number: u32, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::with_capacity(12 + body.len());
    frame.extend_from_slice(&u32::try_from(body.len()).map_err(io::Error::other)?.to_le_bytes());
    frame.extend_from_slice(&[kind, flags]);
    frame.extend_from_slice(&method.to_le_bytes());
    frame.extend_from_slice(&number.to_le_bytes());
    frame.extend_from_slice(body);
    stream.write_all(&frame)?;
    stream.flush()
}

/// The next frame: its kind, flags, stream and body.
fn receive(stream: &mut dyn Stream) -> io::Result<(u8, u8, u32, Vec<u8>)> {
    let mut header = [0; 12];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if length > MOST {
        return Err(io::Error::other(format!("a frame of {length} bytes")));
    }
    let number = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok((header[4], header[5], number, body))
}

/// A MessagePack str.
fn str_of(text: &str) -> Vec<u8> {
    let mut bytes = match text.len() {
        length @ 0..32 => vec![0xa0 | length as u8],
        length => vec![0xd9, u8::try_from(length).unwrap_or(u8::MAX)],
    };
    bytes.extend_from_slice(&text.as_bytes()[..text.len().min(255)]);
    bytes
}

/// The str at `key` of a message, a map of small integer keys.
fn text_at(body: &[u8], key: u64) -> Option<String> {
    let at = value_at(body, key)?;
    let (length, from) = match *body.get(at)? {
        byte @ 0xa0..=0xbf => (usize::from(byte & 0x1f), at + 1),
        0xd9 => (usize::from(*body.get(at + 1)?), at + 2),
        0xda => (usize::from(u16::from_be_bytes([*body.get(at + 1)?, *body.get(at + 2)?])), at + 3),
        _ => return None,
    };
    String::from_utf8(body.get(from..from + length)?.to_vec()).ok()
}

/// The bin at `key` of a message.
fn bin_at(body: &[u8], key: u64) -> Option<Vec<u8>> {
    let at = value_at(body, key)?;
    let (length, from) = match *body.get(at)? {
        0xc4 => (usize::from(*body.get(at + 1)?), at + 2),
        0xc5 => (usize::from(u16::from_be_bytes([*body.get(at + 1)?, *body.get(at + 2)?])), at + 3),
        _ => return None,
    };
    Some(body.get(from..from + length)?.to_vec())
}

/// Where the value of `key` begins in a message, its other values read past.
fn value_at(body: &[u8], key: u64) -> Option<usize> {
    let (count, mut at) = match *body.first()? {
        byte @ 0x80..=0x8f => (usize::from(byte & 0x0f), 1),
        0xde => (usize::from(u16::from_be_bytes([*body.get(1)?, *body.get(2)?])), 3),
        _ => return None,
    };
    for _ in 0..count {
        let found = match *body.get(at)? {
            byte @ 0x00..=0x7f => u64::from(byte),
            0xcc => u64::from(*body.get(at + 1)?),
            _ => return None,
        };
        at = past(body, at, 0)?;
        if found == key {
            return Some(at);
        }
        at = past(body, at, 0)?;
    }
    None
}

/// Where the value at `at` ends, read through to the next; `depth` keeps a
/// nested value from going on for ever.
fn past(body: &[u8], at: usize, depth: usize) -> Option<usize> {
    if depth > 8 {
        return None;
    }
    let byte = *body.get(at)?;
    let length = |bytes: usize| -> Option<usize> {
        let mut length = 0usize;
        for offset in 1..=bytes {
            length = length << 8 | usize::from(*body.get(at + offset)?);
        }
        Some(length)
    };
    let items = |count: usize, from: usize, per: usize| -> Option<usize> {
        let mut at = from;
        for _ in 0..count * per {
            at = past(body, at, depth + 1)?;
        }
        Some(at)
    };
    match byte {
        0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => Some(at + 1),
        0xcc | 0xd0 => Some(at + 2),
        0xcd | 0xd1 => Some(at + 3),
        0xce | 0xd2 => Some(at + 5),
        0xcf | 0xd3 | 0xcb => Some(at + 9),
        0xa0..=0xbf => Some(at + 1 + usize::from(byte & 0x1f)),
        0xd9 | 0xc4 => Some(at + 2 + length(1)?),
        0xda | 0xc5 => Some(at + 3 + length(2)?),
        0xdb | 0xc6 => Some(at + 5 + length(4)?),
        0x90..=0x9f => items(usize::from(byte & 0x0f), at + 1, 1),
        0xdc => items(length(2)?, at + 3, 1),
        0xdd => items(length(4)?, at + 5, 1),
        0x80..=0x8f => items(usize::from(byte & 0x0f), at + 1, 2),
        0xde => items(length(2)?, at + 3, 2),
        0xdf => items(length(4)?, at + 5, 2),
        _ => None,
    }
}

/// Base64url without padding, as `SERVE` spells its secret.
fn base64url(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut held = 0;
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push((bits >> held) as u8);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_messages_fields_are_found_past_the_others() {
        // {1: 2, 2: "0.1.0", 9: ["kv", "sql"], 11: bin 3}
        let body = [
            &[0x84, 0x01, 0x02, 0x02, 0xa5][..],
            b"0.1.0",
            &[0x09, 0x92, 0xa2, b'k', b'v', 0xa3, b's', b'q', b'l', 0x0b, 0xc4, 0x03, 7, 8, 9],
        ]
        .concat();
        assert_eq!(text_at(&body, 2).as_deref(), Some("0.1.0"));
        assert_eq!(bin_at(&body, 11), Some(vec![7, 8, 9]));
        assert_eq!(bin_at(&body, 12), None);
        assert_eq!(text_at(&body[..6], 2), None, "cut short");
    }

    #[test]
    fn a_secret_reads_as_serve_spells_it() {
        assert_eq!(base64url("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64url("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(base64url("a+b"), None);
    }
}
