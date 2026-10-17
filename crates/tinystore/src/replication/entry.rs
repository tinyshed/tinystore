//! One entry of a file's log, as its bytes lie in a segment: one layout, one
//! parser.
//!
//! ```text
//! u32 length of the body           little-endian, as every number here
//! body:
//!   u8   kind                      a commit, the log's beginning, or a gap
//!   u64  sequence                  the file's entries counted from 1
//!   u64  epoch                     raised each time the file's home changes
//!   [16] previous                  the check of the entry before, zeros first
//!   u32 × 4 position               the WAL's generation and frame it came from
//!   u32  page size, u32 size       the file's pages after the commit
//!   u32  count, then each page as  u32 page number, then its bytes
//! [16] check                       SHA-256 of the body, its first 16 bytes
//! ```
//!
//! The check covers the previous entry's check, so the entries chain: an
//! entry is known by its sequence and check wherever it came from, and two
//! entries of one sequence with different checks are two homes' writes.

use sha2::{Digest, Sha256};

use crate::sqlite::Position;
use crate::{Error, ErrorKind, Result};

/// What every segment starts with, and the version of the layout.
pub(crate) const MAGIC: &[u8; 8] = b"TSLOG001";

const FIXED: usize = 1 + 8 + 8 + 16 + 16 + 4 + 4 + 4;
const CHECK: usize = 16;
/// An entry's body at most: a commit of a quarter of a million pages.
const LARGEST: usize = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Commit = 0,
    /// The log starts keeping the file as it stood; a copy starts from a
    /// snapshot.
    Begin = 1,
    /// The file was written beside the store's writer: a copy starts again
    /// from a snapshot.
    Gap = 2,
}

/// Where a file's log stands: the entry last kept or applied, by its
/// sequence, epoch and check. The point of no entry is all zeros.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Point {
    pub(crate) sequence: u64,
    pub(crate) epoch: u64,
    pub(crate) check: [u8; CHECK],
}

/// An entry read back, its pages borrowed from the bytes it was read from.
#[derive(Debug)]
pub(crate) struct Entry<'a> {
    pub(crate) kind: Kind,
    pub(crate) sequence: u64,
    pub(crate) epoch: u64,
    pub(crate) previous: [u8; CHECK],
    pub(crate) check: [u8; CHECK],
    pub(crate) position: Position,
    pub(crate) page_size: u32,
    pub(crate) size: u32,
    pub(crate) pages: Vec<(u32, &'a [u8])>,
}

impl Entry<'_> {
    /// The point the log stands at once it keeps this entry.
    pub(crate) fn point(&self) -> Point {
        Point { sequence: self.sequence, epoch: self.epoch, check: self.check }
    }

    /// Whether this entry is the one after `point`.
    pub(crate) fn follows(&self, point: &Point) -> bool {
        self.sequence == point.sequence + 1 && self.previous == point.check && self.epoch >= point.epoch
    }
}

/// What an entry says besides its pages, which [`encode`] writes.
pub(crate) struct Head {
    pub(crate) kind: Kind,
    pub(crate) sequence: u64,
    pub(crate) epoch: u64,
    pub(crate) previous: [u8; CHECK],
    pub(crate) position: Position,
    pub(crate) page_size: u32,
    pub(crate) size: u32,
}

/// The entry's bytes, length to check, and its check.
pub(crate) fn encode(head: &Head, pages: &[(u32, &[u8])]) -> (Vec<u8>, [u8; CHECK]) {
    let pages_bytes: usize = pages.iter().map(|(_, page)| 4 + page.len()).sum();
    let mut bytes = Vec::with_capacity(4 + FIXED + pages_bytes + CHECK);
    bytes.extend_from_slice(&u32::try_from(FIXED + pages_bytes).unwrap_or(u32::MAX).to_le_bytes());
    bytes.push(head.kind as u8);
    bytes.extend_from_slice(&head.sequence.to_le_bytes());
    bytes.extend_from_slice(&head.epoch.to_le_bytes());
    bytes.extend_from_slice(&head.previous);
    let Position { checkpoints, salts: [first, second], frame } = head.position;
    for word in [checkpoints, first, second, frame, head.page_size, head.size] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&u32::try_from(pages.len()).unwrap_or(u32::MAX).to_le_bytes());
    for (number, page) in pages {
        bytes.extend_from_slice(&number.to_le_bytes());
        bytes.extend_from_slice(page);
    }
    let check = check_of(&bytes[4..]);
    bytes.extend_from_slice(&check);
    (bytes, check)
}

/// Reads the entry at the start of `bytes`, and how many bytes it took.
/// `Ok(None)` is bytes that end before an entry does: a segment's torn tail.
pub(crate) fn decode(bytes: &[u8]) -> Result<Option<(Entry<'_>, usize)>> {
    let Some(length) = bytes.get(..4) else {
        return Ok(None);
    };
    let length = u32::from_le_bytes(length.try_into().expect("four bytes")) as usize;
    if !(FIXED..=LARGEST).contains(&length) {
        return Err(corrupt("an entry's length is outside what an entry may be"));
    }
    let Some(body) = bytes.get(4..4 + length) else {
        return Ok(None);
    };
    let Some(check) = bytes.get(4 + length..4 + length + CHECK) else {
        return Ok(None);
    };
    if check_of(body) != check {
        return Err(corrupt("an entry's check is not its body's"));
    }
    Ok(Some((parse(body, check.try_into().expect("sixteen bytes"))?, 4 + length + CHECK)))
}

fn parse(body: &[u8], check: [u8; CHECK]) -> Result<Entry<'_>> {
    let mut at = Cursor { bytes: body, at: 0 };
    let kind = match at.byte() {
        0 => Kind::Commit,
        1 => Kind::Begin,
        2 => Kind::Gap,
        _ => return Err(corrupt("an entry's kind is none the log writes")),
    };
    let sequence = at.long();
    let epoch = at.long();
    let previous = at.check();
    let position = Position { checkpoints: at.word(), salts: [at.word(), at.word()], frame: at.word() };
    let page_size = at.word();
    let size = at.word();
    let count = at.word() as usize;
    if body.len() - FIXED != count * (4 + page_size as usize) {
        return Err(corrupt("an entry's pages do not fill its body"));
    }
    let pages = (0..count).map(|_| (at.word(), at.take(page_size as usize))).collect();
    Ok(Entry { kind, sequence, epoch, previous, check, position, page_size, size, pages })
}

fn check_of(body: &[u8]) -> [u8; CHECK] {
    let digest = Sha256::digest(body);
    digest[..CHECK].try_into().expect("a digest is longer than a check")
}

fn corrupt(what: &str) -> Error {
    Error::new(ErrorKind::Corrupt, what)
}

/// Reads a body whose length [`parse`] checked first.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let taken = &self.bytes[self.at..self.at + n];
        self.at += n;
        taken
    }

    fn byte(&mut self) -> u8 {
        self.take(1)[0]
    }

    fn word(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().expect("four bytes"))
    }

    fn long(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().expect("eight bytes"))
    }

    fn check(&mut self) -> [u8; CHECK] {
        self.take(CHECK).try_into().expect("sixteen bytes")
    }
}
