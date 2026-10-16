//! Integers of variable width, text behind its length, and a reader of a
//! stored row that bounds every length before it takes it.

use crate::{Error, Result};

/// Appends a count, seven bits a byte, the lowest first.
pub(crate) fn put_count(out: &mut Vec<u8>, mut count: u64) {
    while count >= 0x80 {
        out.push(count as u8 | 0x80);
        count >>= 7;
    }
    out.push(count as u8);
}

/// Appends an integer of either sign, small ones of either sign in one byte.
#[cfg_attr(
    all(not(test), not(any(feature = "records", feature = "metrics"))),
    expect(dead_code, reason = "records and metrics use it")
)]
pub(crate) fn put_int(out: &mut Vec<u8>, value: i64) {
    put_count(out, fold(value));
}

#[cfg_attr(all(not(test), not(feature = "records")), expect(dead_code, reason = "only records uses it"))]
pub(crate) fn put_text(out: &mut Vec<u8>, text: &str) {
    put_count(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

/// ZigZag: 0, -1, 1, -2 … become 0, 1, 2, 3 …, so that a small magnitude of
/// either sign is a small count.
#[cfg_attr(
    all(not(test), not(any(feature = "records", feature = "metrics"))),
    expect(dead_code, reason = "records and metrics use it")
)]
pub(crate) const fn fold(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

#[cfg_attr(
    all(not(test), not(any(feature = "records", feature = "metrics"))),
    expect(dead_code, reason = "records and metrics use it")
)]
pub(crate) const fn unfold(value: u64) -> i64 {
    (value >> 1) as i64 ^ -((value & 1) as i64)
}

/// A reader of a row's bytes. A length is bounded before anything is taken
/// for it, and the first thing that does not read stops every read after it:
/// `finish` then says which invariant broke.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    broken: Option<&'static str>,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Reader { data, broken: None }
    }

    pub(crate) fn fail(&mut self, what: &'static str) {
        self.broken.get_or_insert(what);
        self.data = &[];
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }

    pub(crate) fn take(&mut self, length: usize) -> &'a [u8] {
        if length > self.data.len() {
            self.fail("a row ends inside a value");
            return &[];
        }
        let (taken, rest) = self.data.split_at(length);
        self.data = rest;
        taken
    }

    /// Bytes behind their count, `most` at most.
    #[cfg_attr(
        not(any(feature = "records", feature = "metrics")),
        expect(dead_code, reason = "records and metrics use it")
    )]
    pub(crate) fn counted(&mut self, most: usize) -> &'a [u8] {
        let length = self.count(most);
        self.take(length)
    }

    /// Everything not read yet.
    #[cfg_attr(
        not(any(feature = "records", feature = "metrics")),
        expect(dead_code, reason = "records and metrics use it")
    )]
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }

    /// What is not read yet, left to read.
    #[cfg_attr(
        not(any(feature = "metrics", feature = "search")),
        expect(dead_code, reason = "metrics and search use it")
    )]
    pub(crate) fn remaining(&self) -> &'a [u8] {
        self.data
    }

    /// A count of any size.
    pub(crate) fn uvarint(&mut self) -> u64 {
        let mut count = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte();
            if shift == 63 && byte > 1 {
                break;
            }
            count |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return count;
            }
        }
        self.fail("a count of more than 64 bits");
        0
    }

    /// A count of at most `most`.
    pub(crate) fn count(&mut self, most: usize) -> usize {
        let count = self.uvarint();
        match usize::try_from(count) {
            Ok(count) if count <= most => count,
            _ => {
                self.fail("a count past its bound");
                0
            }
        }
    }

    #[cfg_attr(
        all(not(test), not(any(feature = "records", feature = "metrics"))),
        expect(dead_code, reason = "records and metrics use it")
    )]
    pub(crate) fn int(&mut self) -> i64 {
        unfold(self.uvarint())
    }

    /// Eight bytes, the lowest first.
    #[cfg_attr(all(not(test), not(feature = "metrics")), expect(dead_code, reason = "only metrics uses it"))]
    pub(crate) fn word(&mut self) -> u64 {
        self.take(8).try_into().map_or(0, u64::from_le_bytes)
    }

    /// Text of at most `most` bytes.
    #[cfg_attr(all(not(test), not(feature = "records")), expect(dead_code, reason = "only records uses it"))]
    pub(crate) fn text(&mut self, most: usize) -> &'a str {
        let length = self.count(most);
        match std::str::from_utf8(self.take(length)) {
            Ok(text) => text,
            Err(_) => {
                self.fail("text that is not UTF-8");
                ""
            }
        }
    }

    /// What broke so far, for a reader that hands what is left to another.
    #[cfg_attr(not(feature = "metrics"), expect(dead_code, reason = "only metrics uses it"))]
    pub(crate) fn check(&self) -> Result<()> {
        self.broken.map_or(Ok(()), |what| Err(Error::corrupt(what)))
    }

    /// Nothing broke, and nothing is left unread.
    pub(crate) fn finish(self) -> Result<()> {
        match self.broken {
            Some(what) => Err(Error::corrupt(what)),
            None if self.data.is_empty() => Ok(()),
            None => Err(Error::corrupt("bytes after a row's last value")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_reads_back_and_nothing_is_left() {
        let mut out = Vec::new();
        let integers = [0, 1, -1, 63, -64, 64, 1 << 40, i64::MAX, i64::MIN];
        integers.iter().for_each(|&value| put_int(&mut out, value));
        put_count(&mut out, 300);
        put_count(&mut out, u64::MAX);
        put_text(&mut out, "caf\u{e9}");
        out.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());
        let mut reader = Reader::new(&out);
        for value in integers {
            assert_eq!(reader.int(), value);
        }
        assert_eq!(reader.count(300), 300);
        assert_eq!(reader.uvarint(), u64::MAX);
        assert_eq!(reader.text(16), "caf\u{e9}");
        assert_eq!(reader.word(), 0x0102_0304_0506_0708);
        reader.finish().unwrap();
    }

    #[test]
    fn a_length_past_its_bound_or_its_bytes_names_what_broke() {
        let mut out = Vec::new();
        put_text(&mut out, "four");
        let mut bounded = Reader::new(&out);
        bounded.text(3);
        assert_eq!(bounded.finish().unwrap_err().to_string(), "a count past its bound: corrupt");
        let mut short = Reader::new(&out[..3]);
        short.text(16);
        assert_eq!(short.finish().unwrap_err().to_string(), "a row ends inside a value: corrupt");
        let mut left = Reader::new(&out);
        left.byte();
        assert_eq!(left.finish().unwrap_err().to_string(), "bytes after a row's last value: corrupt");
        let mut cut = Reader::new(&[0x80; 3]);
        cut.uvarint();
        assert_eq!(cut.finish().unwrap_err().to_string(), "a row ends inside a value: corrupt");
        let mut wide = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02]);
        wide.uvarint();
        assert_eq!(wide.finish().unwrap_err().to_string(), "a count of more than 64 bits: corrupt");
    }
}
