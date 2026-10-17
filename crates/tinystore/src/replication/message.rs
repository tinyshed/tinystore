//! What two nodes say over a link, as its bytes lie: one layout, one parser.
//!
//! ```text
//! u32 length of what follows       little-endian, as every number here
//! u8  kind
//! the kind's fields, in order:     u8, u32, u64 as they are; text and bytes
//!                                  as a u32 length, then them; a list as a
//!                                  u32 count, then each item
//! ```

use std::io::Read;

use super::entry::Point;
use crate::{Error, ErrorKind, Result};

/// The link's version, which a node's hello names.
pub(crate) const VERSION: u32 = 1;
/// A message at most: an entry of the largest commit and its name.
const LARGEST: usize = (1 << 30) + (1 << 20);

/// A message of the link.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// The first a node says to the one it takes files from.
    Hello {
        version: u32,
        token: String,
    },
    /// The files a node serves: its own and its copies that are whole.
    Welcome {
        files: Vec<String>,
    },
    Refused {
        why: String,
    },
    List,
    Listed {
        files: Vec<String>,
    },
    /// Sends the file's entries after `at`, or a snapshot when `at` is the
    /// point of no entry or the server no longer holds its next.
    Follow {
        file: String,
        at: Point,
    },
    SnapshotBegin {
        file: String,
        page_size: u32,
        at: Point,
    },
    SnapshotPages {
        file: String,
        pages: Vec<(u32, Vec<u8>)>,
    },
    SnapshotEnd {
        file: String,
        ready_at: u64,
    },
    /// An entry, as its home wrote it.
    Entry {
        file: String,
        bytes: Vec<u8>,
    },
    /// The server cannot send the file now; the follower asks again later.
    Failed {
        file: String,
        kind: ErrorKind,
        why: String,
    },
    /// A write for the files' home, run by the server or passed on toward it.
    Write {
        id: u64,
        op: Vec<u8>,
    },
    /// A write's outcome, and the entry of each file its copy waits for
    /// before the write answers.
    Written {
        id: u64,
        outcome: std::result::Result<Vec<u8>, (ErrorKind, String)>,
        waits: Vec<(String, u64)>,
    },
    /// Said when nothing else was for a while, so a dead link is noticed.
    Ping,
    /// Sends a file blobs keeps beside blobs.db, by its path in the store.
    Fetch {
        path: String,
    },
    /// A piece of a file fetched, from `offset`; `last` ends it.
    Object {
        path: String,
        offset: u64,
        bytes: Vec<u8>,
        last: bool,
    },
}

impl Message {
    /// The message's bytes, its length first.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Out(vec![0; 4]);
        match self {
            Message::Hello { version, token } => {
                out.byte(1);
                out.word(*version);
                out.text(token);
            }
            Message::Welcome { files } => {
                out.byte(2);
                out.texts(files);
            }
            Message::Refused { why } => {
                out.byte(3);
                out.text(why);
            }
            Message::List => out.byte(4),
            Message::Listed { files } => {
                out.byte(5);
                out.texts(files);
            }
            Message::Follow { file, at } => {
                out.byte(6);
                out.text(file);
                out.point(at);
            }
            Message::SnapshotBegin { file, page_size, at } => {
                out.byte(7);
                out.text(file);
                out.word(*page_size);
                out.point(at);
            }
            Message::SnapshotPages { file, pages } => {
                out.byte(8);
                out.text(file);
                out.count(pages.len());
                for (number, page) in pages {
                    out.word(*number);
                    out.bytes(page);
                }
            }
            Message::SnapshotEnd { file, ready_at } => {
                out.byte(9);
                out.text(file);
                out.long(*ready_at);
            }
            Message::Entry { file, bytes } => {
                out.byte(10);
                out.text(file);
                out.bytes(bytes);
            }
            Message::Failed { file, kind, why } => {
                out.byte(11);
                out.text(file);
                out.text(kind.as_str());
                out.text(why);
            }
            Message::Write { id, op } => {
                out.byte(12);
                out.long(*id);
                out.bytes(op);
            }
            Message::Written { id, outcome, waits } => {
                out.byte(13);
                out.long(*id);
                match outcome {
                    Ok(answer) => {
                        out.byte(0);
                        out.bytes(answer);
                    }
                    Err((kind, why)) => {
                        out.byte(1);
                        out.text(kind.as_str());
                        out.text(why);
                    }
                }
                out.count(waits.len());
                for (file, sequence) in waits {
                    out.text(file);
                    out.long(*sequence);
                }
            }
            Message::Ping => out.byte(14),
            Message::Fetch { path } => {
                out.byte(15);
                out.text(path);
            }
            Message::Object { path, offset, bytes, last } => {
                out.byte(16);
                out.text(path);
                out.long(*offset);
                out.bytes(bytes);
                out.byte(u8::from(*last));
            }
        }
        let length = u32::try_from(out.0.len() - 4).unwrap_or(u32::MAX);
        out.0[..4].copy_from_slice(&length.to_le_bytes());
        out.0
    }

    /// Reads one message from `from`; `None` when the link ended cleanly
    /// before one began.
    #[cfg(test)]
    pub(crate) fn read(from: &mut impl Read) -> Result<Option<Message>> {
        let mut length = [0; 4];
        match from.read_exact(&mut length) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(Error::io("a link", error)),
        }
        let length = u32::from_le_bytes(length) as usize;
        if length == 0 || length > LARGEST {
            return Err(corrupt("a message's length is outside what a message may be"));
        }
        let mut body = vec![0; length];
        from.read_exact(&mut body).map_err(|error| Error::io("a link", error))?;
        Self::decode(&body).map(Some)
    }

    fn decode(body: &[u8]) -> Result<Message> {
        let mut at = In { bytes: body, at: 0 };
        let message = match at.byte()? {
            1 => Message::Hello { version: at.word()?, token: at.text()? },
            2 => Message::Welcome { files: at.texts()? },
            3 => Message::Refused { why: at.text()? },
            4 => Message::List,
            5 => Message::Listed { files: at.texts()? },
            6 => Message::Follow { file: at.text()?, at: at.point()? },
            7 => Message::SnapshotBegin { file: at.text()?, page_size: at.word()?, at: at.point()? },
            8 => {
                let file = at.text()?;
                let count = at.count()?;
                let mut pages = Vec::with_capacity(count.min(1 << 16));
                for _ in 0..count {
                    pages.push((at.word()?, at.bytes()?.to_vec()));
                }
                Message::SnapshotPages { file, pages }
            }
            9 => Message::SnapshotEnd { file: at.text()?, ready_at: at.long()? },
            10 => Message::Entry { file: at.text()?, bytes: at.bytes()?.to_vec() },
            11 => Message::Failed { file: at.text()?, kind: kind_of(&at.text()?), why: at.text()? },
            12 => Message::Write { id: at.long()?, op: at.bytes()?.to_vec() },
            13 => {
                let id = at.long()?;
                let outcome = match at.byte()? {
                    0 => Ok(at.bytes()?.to_vec()),
                    1 => Err((kind_of(&at.text()?), at.text()?)),
                    _ => return Err(corrupt("a write's outcome is neither an answer nor an error")),
                };
                let count = at.count()?;
                let mut waits = Vec::with_capacity(count.min(1 << 10));
                for _ in 0..count {
                    waits.push((at.text()?, at.long()?));
                }
                Message::Written { id, outcome, waits }
            }
            14 => Message::Ping,
            15 => Message::Fetch { path: at.text()? },
            16 => Message::Object {
                path: at.text()?,
                offset: at.long()?,
                bytes: at.bytes()?.to_vec(),
                last: at.byte()? == 1,
            },
            _ => return Err(corrupt("a message of a kind the link does not have")),
        };
        if at.at != body.len() {
            return Err(corrupt("a message holds more than its fields"));
        }
        Ok(message)
    }
}

/// Messages read from a link as their bytes arrive, so that a read that
/// stops at a timeout halfway through one loses nothing of it.
#[derive(Default)]
pub(crate) struct Frames {
    bytes: Vec<u8>,
    start: usize,
}

impl Frames {
    /// Reads what `from` has now; `Ok(false)` once the link ended. A timeout
    /// is an error of kind `WouldBlock` or `TimedOut`, as the system says it.
    pub(crate) fn fill(&mut self, from: &mut impl Read) -> std::io::Result<bool> {
        if self.start > 0 && self.start == self.bytes.len() {
            self.bytes.clear();
            self.start = 0;
        }
        let mut chunk = [0; 64 << 10];
        let read = from.read(&mut chunk)?;
        self.bytes.extend_from_slice(&chunk[..read]);
        Ok(read > 0)
    }

    /// The next whole message read, if one is.
    pub(crate) fn next(&mut self) -> Result<Option<Message>> {
        let held = &self.bytes[self.start..];
        let Some(length) = held.get(..4) else {
            return Ok(None);
        };
        let length = u32::from_le_bytes(length.try_into().expect("four bytes")) as usize;
        if length == 0 || length > LARGEST {
            return Err(corrupt("a message's length is outside what a message may be"));
        }
        let Some(body) = held.get(4..4 + length) else {
            return Ok(None);
        };
        let message = Message::decode(body)?;
        self.start += 4 + length;
        if self.start > (1 << 20) && self.start * 2 > self.bytes.len() {
            self.bytes.drain(..self.start);
            self.start = 0;
        }
        Ok(Some(message))
    }
}

/// Whether `error` is a read that waited its timeout out.
pub(crate) fn timed_out(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

/// Whether `name` is a file a link may name: a path inside a store's
/// directory, in parts of lower-case letters, digits, `_`, `-` and `.`, none
/// of them `.` or `..`.
pub(crate) fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b))
        })
}

/// The error kind `name` spells, as [`ErrorKind::as_str`] writes it.
pub(crate) fn kind_of(name: &str) -> ErrorKind {
    const KINDS: [ErrorKind; 16] = [
        ErrorKind::Invalid,
        ErrorKind::NotFound,
        ErrorKind::Conflict,
        ErrorKind::TooOld,
        ErrorKind::TooNew,
        ErrorKind::Limit,
        ErrorKind::Closed,
        ErrorKind::Corrupt,
        ErrorKind::InUse,
        ErrorKind::Suspended,
        ErrorKind::Unavailable,
        ErrorKind::Cancelled,
        ErrorKind::OutcomeUnknown,
        ErrorKind::Lost,
        ErrorKind::Io,
        ErrorKind::Internal,
    ];
    KINDS.into_iter().find(|kind| kind.as_str() == name).unwrap_or(ErrorKind::Internal)
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a link: {what}"))
}

/// The bytes being written, a field at a time.
pub(crate) struct Out(pub(crate) Vec<u8>);

impl Out {
    pub(crate) fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    pub(crate) fn word(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn long(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    pub(crate) fn count(&mut self, count: usize) {
        self.word(u32::try_from(count).unwrap_or(u32::MAX));
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.count(bytes.len());
        self.0.extend_from_slice(bytes);
    }

    pub(crate) fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    fn texts(&mut self, texts: &[String]) {
        self.count(texts.len());
        for text in texts {
            self.text(text);
        }
    }

    fn point(&mut self, point: &Point) {
        self.long(point.sequence);
        self.long(point.epoch);
        self.0.extend_from_slice(&point.check);
    }
}

/// The bytes being read, a field at a time, each checked against what is left.
pub(crate) struct In<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> In<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let taken = self
            .bytes
            .get(self.at..self.at.saturating_add(n))
            .ok_or_else(|| corrupt("a field runs past its message"))?;
        self.at += n;
        Ok(taken)
    }

    pub(crate) fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn word(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }

    pub(crate) fn long(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }

    pub(crate) fn count(&mut self) -> Result<usize> {
        Ok(self.word()? as usize)
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8]> {
        let length = self.count()?;
        self.take(length)
    }

    pub(crate) fn text(&mut self) -> Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| corrupt("a text is not UTF-8"))
    }

    fn texts(&mut self) -> Result<Vec<String>> {
        let count = self.count()?;
        let mut texts = Vec::with_capacity(count.min(1 << 12));
        for _ in 0..count {
            texts.push(self.text()?);
        }
        Ok(texts)
    }

    fn point(&mut self) -> Result<Point> {
        Ok(Point {
            sequence: self.long()?,
            epoch: self.long()?,
            check: self.take(16)?.try_into().expect("sixteen bytes"),
        })
    }

    pub(crate) fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_reads_back_as_it_was_written() {
        let at = Point { sequence: 7, epoch: 2, check: [3; 16] };
        let messages = [
            Message::Hello { version: VERSION, token: "t".into() },
            Message::Welcome { files: vec!["kv.db".into(), "sql/app.db".into()] },
            Message::Refused { why: "no".into() },
            Message::List,
            Message::Listed { files: vec![] },
            Message::Follow { file: "kv.db".into(), at },
            Message::SnapshotBegin { file: "kv.db".into(), page_size: 4096, at },
            Message::SnapshotPages { file: "kv.db".into(), pages: vec![(1, vec![1, 2]), (2, vec![3])] },
            Message::SnapshotEnd { file: "kv.db".into(), ready_at: 9 },
            Message::Entry { file: "kv.db".into(), bytes: vec![9; 40] },
            Message::Failed { file: "kv.db".into(), kind: ErrorKind::Unavailable, why: "later".into() },
            Message::Write { id: 4, op: vec![1, 2, 3] },
            Message::Written { id: 4, outcome: Ok(vec![5]), waits: vec![("kv.db".into(), 12)] },
            Message::Written { id: 5, outcome: Err((ErrorKind::Conflict, "taken".into())), waits: vec![] },
            Message::Ping,
            Message::Fetch { path: "blobs/objects/1/1a3f".into() },
            Message::Object { path: "blobs/objects/1/1a3f".into(), offset: 4, bytes: vec![7; 3], last: true },
        ];
        for message in messages {
            let bytes = message.encode();
            let read = Message::read(&mut bytes.as_slice()).unwrap().unwrap();
            assert_eq!(read, message);
        }
        let mut cut = Message::Ping.encode();
        cut.push(0);
        cut[0] = 2;
        assert_eq!(Message::read(&mut cut.as_slice()).unwrap_err().kind(), ErrorKind::Corrupt);
    }
}
