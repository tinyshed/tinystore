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
use std::time::Duration;

use super::entry::Point;
use super::witness::Holder;
use crate::{Error, ErrorKind, Result};

/// The link's version, which a node's hello names: a node refuses a link of
/// another. It stays 1 until v0.1.0, since nothing before it is compatible,
/// and moves after it with every change of a message.
pub(crate) const VERSION: u32 = 1;
/// A message at most: an entry of the largest commit and its name.
const LARGEST: usize = (1 << 30) + (1 << 20);

/// One file's ask in a judged node's lease: a renewal of the lease of a file
/// it writes at `epoch`, or a stand for one it copies, at `position`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseAsk {
    pub(crate) file: String,
    pub(crate) epoch: u64,
    pub(crate) position: u64,
    pub(crate) holding: bool,
    /// The copies a synchronous home's commits wait for.
    pub(crate) synchronous: Vec<String>,
}

/// The holder of a file's lease, as the witness answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeaseHolder {
    pub(crate) file: String,
    pub(crate) holder: Holder,
}

/// A message of the link.
/// A write the home refused, as the link carries it: its kind, what it says
/// and its facts by name, so that a program on a copy reads the constraint or
/// the bound it would read at the home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) kind: ErrorKind,
    pub(crate) why: String,
    pub(crate) facts: Vec<(String, String)>,
}

impl Refusal {
    pub(crate) fn of(error: &Error) -> Refusal {
        let facts = error.facts().iter().map(|(name, value)| (name.to_string(), value.clone())).collect();
        Refusal { kind: error.kind(), why: error.what().to_owned(), facts }
    }

    fn write<'a>(&self, out: &'a mut Out) -> &'a mut Out {
        out.text(self.kind.as_str()).text(&self.why).list(&self.facts, |out, (name, value)| {
            out.text(name).text(value);
        })
    }

    fn read(at: &mut In<'_>) -> Result<Refusal> {
        let (kind, why) = (kind_of(&at.text()?), at.text()?);
        Ok(Refusal { kind, why, facts: at.list(16, |at| Ok((at.text()?, at.text()?)))? })
    }

    pub(crate) fn into_error(self) -> Error {
        let error = Error::new(self.kind, self.why);
        self.facts.into_iter().fold(error, |error, (name, value)| error.naming(name, value))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// The first a node says to the one it takes files from.
    Hello {
        version: u32,
        token: String,
        /// The node's id, which it keeps in its `log/node`.
        node: String,
        /// Where other nodes reach it; empty for a node that serves nothing.
        address: String,
    },
    /// The node that answers, by its id, and the files it serves: its own and
    /// its copies that are whole.
    Welcome {
        node: String,
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
        outcome: std::result::Result<Vec<u8>, Refusal>,
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
    /// The follower's copy of `file` holds its entries up to `sequence`.
    Applied {
        file: String,
        sequence: u64,
    },
    /// A judged node's asks of its witness, all its files' in one message.
    Lease {
        node: String,
        address: String,
        asks: Vec<LeaseAsk>,
    },
    /// The holder of each file's lease the asks named, as the witness answers.
    Leased {
        holders: Vec<LeaseHolder>,
    },
    /// Asks for the point the node holds `file` at and the hash of its pages.
    Verify {
        file: String,
    },
    /// The point and the SHA-256 of `file`'s pages there; no hash, and why,
    /// when the node could not read them.
    Verified {
        file: String,
        at: Point,
        sum: Vec<u8>,
        why: String,
    },
}

impl Message {
    /// The message's bytes, its length first.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Out(vec![0; 4]);
        self.write(&mut out);
        let length = u32::try_from(out.0.len() - 4).unwrap_or(u32::MAX);
        out.0[..4].copy_from_slice(&length.to_le_bytes());
        out.0
    }

    /// Writes the message's kind, then its fields in order.
    fn write(&self, out: &mut Out) {
        match self {
            Message::Hello { version, token, node, address } => {
                out.byte(1).word(*version).text(token).text(node).text(address)
            }
            Message::Welcome { node, files } => out.byte(2).text(node).texts(files),
            Message::Refused { why } => out.byte(3).text(why),
            Message::List => out.byte(4),
            Message::Listed { files } => out.byte(5).texts(files),
            Message::Follow { file, at } => out.byte(6).text(file).point(at),
            Message::SnapshotBegin { file, page_size, at } => out.byte(7).text(file).word(*page_size).point(at),
            Message::SnapshotPages { file, pages } => out.byte(8).text(file).list(pages, |out, (number, page)| {
                out.word(*number).bytes(page);
            }),
            Message::SnapshotEnd { file, ready_at } => out.byte(9).text(file).long(*ready_at),
            Message::Entry { file, bytes } => out.byte(10).text(file).bytes(bytes),
            Message::Failed { file, kind, why } => out.byte(11).text(file).text(kind.as_str()).text(why),
            Message::Write { id, op } => out.byte(12).long(*id).bytes(op),
            Message::Written { id, outcome, waits } => {
                match outcome {
                    Ok(answer) => out.byte(13).long(*id).byte(0).bytes(answer),
                    Err(refusal) => refusal.write(out.byte(13).long(*id).byte(1)),
                };
                out.list(waits, |out, (file, sequence)| {
                    out.text(file).long(*sequence);
                })
            }
            Message::Ping => out.byte(14),
            Message::Fetch { path } => out.byte(15).text(path),
            Message::Object { path, offset, bytes, last } => {
                out.byte(16).text(path).long(*offset).bytes(bytes).byte(u8::from(*last))
            }
            Message::Applied { file, sequence } => out.byte(17).text(file).long(*sequence),
            Message::Verify { file } => out.byte(18).text(file),
            Message::Verified { file, at, sum, why } => out.byte(19).text(file).point(at).bytes(sum).text(why),
            Message::Lease { node, address, asks } => out.byte(20).text(node).text(address).list(asks, |out, ask| {
                out.text(&ask.file).long(ask.epoch).long(ask.position);
                out.byte(u8::from(ask.holding)).texts(&ask.synchronous);
            }),
            Message::Leased { holders } => out.byte(21).list(holders, |out, held| {
                out.text(&held.file).text(&held.holder.node).text(&held.holder.address).long(held.holder.epoch);
            }),
        };
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
        let message = Self::read_fields(&mut at)?;
        if at.at != body.len() {
            return Err(corrupt("a message holds more than its fields"));
        }
        Ok(message)
    }

    /// Reads a message's kind, then its fields in order.
    fn read_fields(at: &mut In<'_>) -> Result<Message> {
        Ok(match at.byte()? {
            1 => Message::Hello { version: at.word()?, token: at.text()?, node: at.text()?, address: at.text()? },
            2 => Message::Welcome { node: at.text()?, files: at.texts()? },
            3 => Message::Refused { why: at.text()? },
            4 => Message::List,
            5 => Message::Listed { files: at.texts()? },
            6 => Message::Follow { file: at.text()?, at: at.point()? },
            7 => Message::SnapshotBegin { file: at.text()?, page_size: at.word()?, at: at.point()? },
            8 => Message::SnapshotPages {
                file: at.text()?,
                pages: at.list(1 << 16, |at| Ok((at.word()?, at.bytes()?.to_vec())))?,
            },
            9 => Message::SnapshotEnd { file: at.text()?, ready_at: at.long()? },
            10 => Message::Entry { file: at.text()?, bytes: at.bytes()?.to_vec() },
            11 => Message::Failed { file: at.text()?, kind: kind_of(&at.text()?), why: at.text()? },
            12 => Message::Write { id: at.long()?, op: at.bytes()?.to_vec() },
            13 => Message::Written {
                id: at.long()?,
                outcome: match at.byte()? {
                    0 => Ok(at.bytes()?.to_vec()),
                    1 => Err(Refusal::read(at)?),
                    _ => return Err(corrupt("a write's outcome is neither an answer nor an error")),
                },
                waits: at.list(1 << 10, |at| Ok((at.text()?, at.long()?)))?,
            },
            14 => Message::Ping,
            15 => Message::Fetch { path: at.text()? },
            16 => Message::Object {
                path: at.text()?,
                offset: at.long()?,
                bytes: at.bytes()?.to_vec(),
                last: at.byte()? == 1,
            },
            17 => Message::Applied { file: at.text()?, sequence: at.long()? },
            18 => Message::Verify { file: at.text()? },
            19 => Message::Verified { file: at.text()?, at: at.point()?, sum: at.bytes()?.to_vec(), why: at.text()? },
            20 => Message::Lease {
                node: at.text()?,
                address: at.text()?,
                asks: at.list(1 << 12, |at| {
                    let (file, epoch, position, holding) = (at.text()?, at.long()?, at.long()?, at.byte()? == 1);
                    Ok(LeaseAsk { file, epoch, position, holding, synchronous: at.texts()? })
                })?,
            },
            21 => Message::Leased {
                holders: at.list(1 << 12, |at| {
                    let file = at.text()?;
                    Ok(LeaseHolder {
                        file,
                        holder: Holder { node: at.text()?, address: at.text()?, epoch: at.long()? },
                    })
                })?,
            },
            _ => return Err(corrupt("a message of a kind the link does not have")),
        })
    }
}

/// Messages read from a link as their bytes arrive, so that a read that
/// stops at a timeout halfway through one loses nothing of it.
#[derive(Default)]
pub(crate) struct Frames {
    bytes: Vec<u8>,
    start: usize,
    /// The link's stream, once what arrives is packed.
    unpacker: Option<super::pack::Unpacker>,
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
        match &mut self.unpacker {
            Some(unpacker) => unpacker.unpack(&chunk[..read], &mut self.bytes)?,
            None => self.bytes.extend_from_slice(&chunk[..read]),
        }
        Ok(read > 0)
    }

    /// Reads what arrives from here on through the link's stream, what
    /// arrived already past the last message read included.
    pub(crate) fn unpack_from_here(&mut self) -> Result<()> {
        let arrived = self.bytes.split_off(self.start);
        (self.bytes, self.start) = (Vec::new(), 0);
        let mut unpacker = super::pack::Unpacker::new().map_err(|error| Error::io("a link's stream", error))?;
        unpacker.unpack(&arrived, &mut self.bytes).map_err(|error| Error::io("a link's stream", error))?;
        self.unpacker = Some(unpacker);
        Ok(())
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
    KINDS.into_iter().find(|kind| kind.as_str() == name).unwrap_or(ErrorKind::Internal)
}

/// Every error kind, each of which a link spells by its name.
const KINDS: [ErrorKind; 17] = [
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
    ErrorKind::CommittedLocally,
    ErrorKind::Lost,
    ErrorKind::Io,
    ErrorKind::Internal,
];

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, format!("a link: {what}"))
}

/// The bytes being written, a field at a time.
pub(crate) struct Out(pub(crate) Vec<u8>);

impl Out {
    pub(crate) fn byte(&mut self, value: u8) -> &mut Out {
        self.0.push(value);
        self
    }

    pub(crate) fn word(&mut self, value: u32) -> &mut Out {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn long(&mut self, value: u64) -> &mut Out {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn count(&mut self, count: usize) -> &mut Out {
        self.word(u32::try_from(count).unwrap_or(u32::MAX))
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> &mut Out {
        self.count(bytes.len()).0.extend_from_slice(bytes);
        self
    }

    pub(crate) fn text(&mut self, text: &str) -> &mut Out {
        self.bytes(text.as_bytes())
    }

    /// A byte saying whether `text` is there, then it, or an empty text.
    pub(crate) fn maybe_text(&mut self, text: Option<&str>) -> &mut Out {
        self.byte(u8::from(text.is_some())).text(text.unwrap_or(""))
    }

    /// A byte saying whether `value` is there, then it, or zero.
    pub(crate) fn maybe_long(&mut self, value: Option<u64>) -> &mut Out {
        self.byte(u8::from(value.is_some())).long(value.unwrap_or(0))
    }

    /// A span as its milliseconds.
    pub(crate) fn span(&mut self, span: Duration) -> &mut Out {
        self.long(crate::clock::millis(span) as u64)
    }

    pub(crate) fn texts(&mut self, texts: &[String]) -> &mut Out {
        self.list(texts, |out, text| {
            out.text(text);
        })
    }

    /// Writes how many `items` there are, then each with `each`.
    pub(crate) fn list<T>(&mut self, items: &[T], mut each: impl FnMut(&mut Out, &T)) -> &mut Out {
        self.count(items.len());
        for item in items {
            each(self, item);
        }
        self
    }

    pub(crate) fn point(&mut self, point: &Point) -> &mut Out {
        self.long(point.sequence).long(point.epoch).0.extend_from_slice(&point.check);
        self
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

    pub(crate) fn maybe_text(&mut self) -> Result<Option<String>> {
        let there = self.byte()? == 1;
        let text = self.text()?;
        Ok(there.then_some(text))
    }

    pub(crate) fn maybe_long(&mut self) -> Result<Option<u64>> {
        let there = self.byte()? == 1;
        let value = self.long()?;
        Ok(there.then_some(value))
    }

    pub(crate) fn span(&mut self) -> Result<Duration> {
        Ok(Duration::from_millis(self.long()?))
    }

    pub(crate) fn texts(&mut self) -> Result<Vec<String>> {
        self.list(1 << 12, In::text)
    }

    /// Reads how many items follow, then each with `each`, holding room for
    /// `at_most` until they are read: a count is the sender's word.
    pub(crate) fn list<T>(&mut self, at_most: usize, mut each: impl FnMut(&mut In<'a>) -> Result<T>) -> Result<Vec<T>> {
        let count = self.count()?;
        let mut items = Vec::with_capacity(count.min(at_most));
        for _ in 0..count {
            items.push(each(self)?);
        }
        Ok(items)
    }

    pub(crate) fn point(&mut self) -> Result<Point> {
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
    fn every_error_kind_crosses_a_link_by_its_name() {
        // a kind added to ErrorKind and not to KINDS fails to compile here
        let listed = |kind: ErrorKind| match kind {
            ErrorKind::Invalid
            | ErrorKind::NotFound
            | ErrorKind::Conflict
            | ErrorKind::TooOld
            | ErrorKind::TooNew
            | ErrorKind::Limit
            | ErrorKind::Closed
            | ErrorKind::Corrupt
            | ErrorKind::InUse
            | ErrorKind::Suspended
            | ErrorKind::Unavailable
            | ErrorKind::Cancelled
            | ErrorKind::OutcomeUnknown
            | ErrorKind::CommittedLocally
            | ErrorKind::Lost
            | ErrorKind::Io
            | ErrorKind::Internal => KINDS.contains(&kind),
        };
        for kind in KINDS {
            assert!(listed(kind));
            assert_eq!(kind_of(kind.as_str()), kind, "{kind:?}");
        }
    }

    #[test]
    fn every_message_reads_back_as_it_was_written() {
        let at = Point { sequence: 7, epoch: 2, check: [3; 16] };
        let messages = [
            Message::Hello { version: VERSION, token: "t".into(), node: "9f86d081".into(), address: "eu:7444".into() },
            Message::Welcome { node: "9f86d081".into(), files: vec!["kv.db".into(), "sql/app.db".into()] },
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
            Message::Written {
                id: 5,
                outcome: Err(Refusal {
                    kind: ErrorKind::Conflict,
                    why: "taken".into(),
                    facts: vec![("constraint".into(), "unique".into()), ("columns".into(), "email".into())],
                }),
                waits: vec![],
            },
            Message::Ping,
            Message::Fetch { path: "blobs/objects/1/1a3f".into() },
            Message::Object { path: "blobs/objects/1/1a3f".into(), offset: 4, bytes: vec![7; 3], last: true },
            Message::Applied { file: "kv.db".into(), sequence: 42 },
            Message::Verify { file: "kv.db".into() },
            Message::Lease {
                node: "9f86d081".into(),
                address: "eu:7444".into(),
                asks: vec![
                    LeaseAsk {
                        file: "kv.db".into(),
                        epoch: 3,
                        position: 90,
                        holding: true,
                        synchronous: vec!["eu:7444".into()],
                    },
                    LeaseAsk { file: "jobs.db".into(), epoch: 1, position: 7, holding: false, synchronous: vec![] },
                ],
            },
            Message::Leased {
                holders: vec![LeaseHolder {
                    file: "kv.db".into(),
                    holder: Holder { node: "9f86d081".into(), address: "eu:7444".into(), epoch: 3 },
                }],
            },
            Message::Verified { file: "kv.db".into(), at, sum: vec![7; 32], why: String::new() },
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
