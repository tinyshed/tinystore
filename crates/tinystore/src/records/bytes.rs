//! The bytes of a stored row: counts and integers of variable width, text
//! behind its length, a reader that bounds every length before it takes it,
//! and the CRC-32 every row ends with.

use crate::{Error, Result};

/// Appends a count, seven bits a byte, the lowest first.
pub(super) fn put_count(out: &mut Vec<u8>, mut count: u64) {
    while count >= 0x80 {
        out.push(count as u8 | 0x80);
        count >>= 7;
    }
    out.push(count as u8);
}

/// Appends an integer of either sign, small ones of either sign in one byte.
pub(super) fn put_int(out: &mut Vec<u8>, value: i64) {
    put_count(out, ((value << 1) ^ (value >> 63)) as u64);
}

pub(super) fn put_text(out: &mut Vec<u8>, text: &str) {
    put_count(out, text.len() as u64);
    out.extend_from_slice(text.as_bytes());
}

/// A reader of a row's bytes. A length is bounded before anything is taken
/// for it, and the first thing that does not read stops every read after it:
/// `finish` then says which invariant broke.
pub(super) struct Reader<'a> {
    data: &'a [u8],
    broken: Option<&'static str>,
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Reader { data, broken: None }
    }

    pub(super) fn fail(&mut self, what: &'static str) {
        self.broken.get_or_insert(what);
        self.data = &[];
    }

    pub(super) fn byte(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }

    pub(super) fn take(&mut self, length: usize) -> &'a [u8] {
        if length > self.data.len() {
            self.fail("a row ends inside a value");
            return &[];
        }
        let (taken, rest) = self.data.split_at(length);
        self.data = rest;
        taken
    }

    /// Everything not read yet.
    pub(super) fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }

    /// A count of at most `most`.
    pub(super) fn count(&mut self, most: usize) -> usize {
        let mut count = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte();
            count |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return match usize::try_from(count) {
                    Ok(count) if count <= most => count,
                    _ => {
                        self.fail("a count past its bound");
                        0
                    }
                };
            }
        }
        self.fail("a count of more than ten bytes");
        0
    }

    pub(super) fn int(&mut self) -> i64 {
        let folded = self.count(usize::MAX) as u64;
        (folded >> 1) as i64 ^ -((folded & 1) as i64)
    }

    /// Text of at most `most` bytes.
    pub(super) fn text(&mut self, most: usize) -> &'a str {
        let length = self.count(most);
        match std::str::from_utf8(self.take(length)) {
            Ok(text) => text,
            Err(_) => {
                self.fail("text that is not UTF-8");
                ""
            }
        }
    }

    /// Nothing broke, and nothing is left unread.
    pub(super) fn finish(self) -> Result<()> {
        match self.broken {
            Some(what) => Err(Error::corrupt(what)),
            None if self.data.is_empty() => Ok(()),
            None => Err(Error::corrupt("bytes after a row's last value")),
        }
    }
}

/// The CRC-32 of zlib and Ethernet, a byte at a time.
pub(super) fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0_u32; 256];
        let mut byte = 0;
        while byte < 256 {
            let mut crc = byte as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
                bit += 1;
            }
            table[byte] = crc;
            byte += 1;
        }
        table
    };
    !data.iter().fold(!0_u32, |crc, &byte| TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_reads_back_and_nothing_is_left() {
        let mut out = Vec::new();
        for value in [0, 1, -1, 63, -64, 64, i64::MAX, i64::MIN] {
            put_int(&mut out, value);
        }
        put_count(&mut out, 300);
        put_text(&mut out, "caf\u{e9}");
        let mut reader = Reader::new(&out);
        for value in [0, 1, -1, 63, -64, 64, i64::MAX, i64::MIN] {
            assert_eq!(reader.int(), value);
        }
        assert_eq!(reader.count(300), 300);
        assert_eq!(reader.text(16), "caf\u{e9}");
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
    }

    #[test]
    fn the_checksum_is_zlibs() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
