//! A key's postings in a short piece, and what a document's row keeps of
//! its keys and lengths. A longer piece keeps them in blocks, as
//! `packed.rs` lays them out.
//!
//! ```text
//! pieces (segment, key) → postings
//!
//! postings        (doc − the segment's first, tf, len) a posting               varints
//! inline          (doc − doc before, tf, len) a posting, a key's best           varints
//! a doc's keys    (key − key before) a key it holds                             varints
//! a doc's lengths (part, length) a part it has words in                         varints
//! ```
//!
//! A piece's documents count from its segment's first rather than from the
//! one before, so that a set appends to a piece of the open segment by
//! joining bytes, in SQL, without reading the piece.

use crate::Result;
use crate::compress::varint::{Reader, put_count};

/// Postings a block holds at most.
pub(crate) const BLOCK: usize = 128;
/// Postings a key keeps in its own row at most, rather than in blocks.
pub(crate) const INLINE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Posting {
    pub(crate) doc: u32,
    pub(crate) tf: u32,
    pub(crate) len: u32,
}

/// A key's postings as its own row keeps them: lengths and all.
pub(crate) fn pack_inline(postings: &[Posting]) -> Vec<u8> {
    let mut out = Vec::with_capacity(postings.len() * 4);
    let mut before = 0;
    for posting in postings {
        put_count(&mut out, u64::from(posting.doc - before));
        put_count(&mut out, u64::from(posting.tf));
        put_count(&mut out, u64::from(posting.len));
        before = posting.doc;
    }
    out
}

pub(crate) fn unpack_inline(bytes: &[u8]) -> Result<Vec<Posting>> {
    let mut reader = Reader::new(bytes);
    let mut out = Vec::with_capacity(INLINE);
    let mut doc = 0;
    while !reader.remaining().is_empty() {
        doc = after(&mut reader, doc, "a key's postings out of order");
        let tf = frequency(&mut reader);
        out.push(Posting { doc, tf, len: number(&mut reader) });
    }
    reader.finish()?;
    Ok(out)
}

/// A piece's postings, its segment's documents from `first`.
pub(crate) fn pack_piece(postings: &[Posting], first: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(postings.len() * 4);
    for posting in postings {
        put_count(&mut out, u64::from(posting.doc - first));
        put_count(&mut out, u64::from(posting.tf));
        put_count(&mut out, u64::from(posting.len));
    }
    out
}

pub(crate) fn unpack_piece(bytes: &[u8], first: u32) -> Result<Vec<Posting>> {
    let mut reader = Reader::new(bytes);
    let mut out = Vec::with_capacity(INLINE);
    while !reader.remaining().is_empty() {
        let doc = first.saturating_add(number(&mut reader));
        if out.last().is_some_and(|last: &Posting| last.doc >= doc) {
            reader.fail("a piece's postings out of order");
        }
        let tf = frequency(&mut reader);
        out.push(Posting { doc, tf, len: number(&mut reader) });
    }
    reader.finish()?;
    Ok(out)
}

/// The ids of the keys a document holds, ascending.
pub(crate) fn encode_keys(keys: &[i64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(keys.len() * 2);
    let mut before = 0;
    for &key in keys {
        put_count(&mut out, key.abs_diff(before));
        before = key;
    }
    out
}

pub(crate) fn decode_keys(bytes: &[u8]) -> Result<Vec<i64>> {
    let mut reader = Reader::new(bytes);
    let mut keys = Vec::with_capacity(bytes.len());
    let mut key = 0_i64;
    while !reader.remaining().is_empty() {
        let step = reader.uvarint();
        if step == 0 || step > i64::MAX as u64 {
            reader.fail("a document's keys out of order");
        }
        key = key.saturating_add(step as i64);
        keys.push(key);
    }
    reader.finish()?;
    Ok(keys)
}

/// How many words a document has in each part, those with none left out.
pub(crate) fn encode_lengths(lengths: &[(u32, u32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lengths.len() * 3);
    for &(part, len) in lengths {
        put_count(&mut out, u64::from(part));
        put_count(&mut out, u64::from(len));
    }
    out
}

pub(crate) fn decode_lengths(bytes: &[u8]) -> Result<Vec<(u32, u32)>> {
    let mut reader = Reader::new(bytes);
    let mut lengths = Vec::new();
    while !reader.remaining().is_empty() {
        lengths.push((number(&mut reader), number(&mut reader)));
    }
    reader.finish()?;
    Ok(lengths)
}

/// A document's number after `before`, the one before it, or 0 for the
/// first: numbers begin at 1 and only grow.
pub(super) fn after(reader: &mut Reader<'_>, before: u32, broken: &'static str) -> u32 {
    let step = number(reader);
    if step == 0 || before.checked_add(step).is_none() {
        reader.fail(broken);
    }
    before.saturating_add(step)
}

pub(super) fn frequency(reader: &mut Reader<'_>) -> u32 {
    let tf = number(reader);
    if tf == 0 {
        reader.fail("a posting of no occurrences");
    }
    tf
}

pub(super) fn number(reader: &mut Reader<'_>) -> u32 {
    u32::try_from(reader.uvarint()).unwrap_or_else(|_| {
        reader.fail("a number past 32 bits");
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posting(doc: u32, tf: u32, len: u32) -> Posting {
        Posting { doc, tf, len }
    }

    #[test]
    fn a_short_list_and_a_documents_keys_and_lengths_read_back_as_written() {
        let postings = [posting(3, 1, 9), posting(300, 4, 120), posting(70_000, 2, 5)];
        assert_eq!(unpack_inline(&pack_inline(&postings)).unwrap(), postings);
        let (front, back) = postings.split_at(1);
        let joined = [pack_piece(front, 3), pack_piece(back, 3)].concat();
        assert_eq!(unpack_piece(&joined, 3).unwrap(), postings, "a piece appended to as bytes");
        assert_eq!(decode_keys(&encode_keys(&[2, 9, 10, 4000])).unwrap(), [2, 9, 10, 4000]);
        assert_eq!(decode_lengths(&encode_lengths(&[(0, 12), (1, 3)])).unwrap(), [(0, 12), (1, 3)]);
    }

    #[test]
    fn bytes_that_break_an_invariant_say_which() {
        let twice = [posting(5, 1, 1), posting(5, 1, 1)];
        let broken = |error: crate::Error| error.to_string();
        assert_eq!(broken(unpack_inline(&pack_inline(&twice)).unwrap_err()), "a key's postings out of order: corrupt");
        assert_eq!(broken(decode_keys(&[3, 0]).unwrap_err()), "a document's keys out of order: corrupt");
        assert_eq!(
            broken(unpack_piece(&pack_piece(&twice, 5), 5).unwrap_err()),
            "a piece's postings out of order: corrupt"
        );
    }
}
