//! The bodies of records' fuzz targets, which `fuzz/` runs by hand on
//! nightly through cargo-fuzz, and a test runs on every platform CI runs on:
//! what a decoder does with any bytes past the version and the checksum that
//! guard them.

use serde_json::value::RawValue;

use super::block;
use super::bytes::crc32;
use super::event::{Kept, NewEvent};
use super::head::{self, Codec};
use super::segment::{self, Schema};
use crate::{ErrorKind, Result};

/// Reads any bytes as a head row, a segment's row and a block of a segment
/// of fields: each gives its events or fails `corrupt`, and none panics.
pub fn rows(bytes: &[u8]) {
    let mut codec = Codec::default();
    corrupt_or_read(head::parse(&sealed(head::VERSION, bytes), &mut codec));
    corrupt_or_read(Schema::parse(&sealed(segment::VERSION, bytes), &mut codec));
    let (schema, _) = Schema::of(&seeds());
    corrupt_or_read(block::parse(&schema, &sealed(block::VERSION, bytes), &mut codec));
}

/// Bytes framed as every row is, their version first and their checksum
/// last, so that a decoder reads past both.
fn sealed(version: u8, bytes: &[u8]) -> Vec<u8> {
    let mut row = Vec::with_capacity(bytes.len() + 5);
    row.push(version);
    row.extend_from_slice(bytes);
    let sum = crc32(&row);
    row.extend_from_slice(&sum.to_le_bytes());
    row
}

fn corrupt_or_read<T>(read: Result<T>) {
    if let Err(error) = read {
        assert_eq!(error.kind(), ErrorKind::Corrupt, "{error}");
    }
}

/// The events of the segment whose blocks `rows` reads: names, fields of
/// numbers and text, and an event with none.
fn seeds() -> Vec<Kept> {
    let spelled = |name: &str, at: i64, json: &str| match RawValue::from_string(json.to_owned()) {
        Ok(fields) => NewEvent::new(name).fields(&fields).kept(at),
        Err(_) => NewEvent::new(name).kept(at),
    };
    vec![
        spelled("request", 1, r#"{"route":"/a","ms":12,"user":"9f86d081884c7d65"}"#),
        spelled("request", 2, r#"{"route":"/b","ms":7.5,"ok":true}"#),
        spelled("signup", 3, "{}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::pack::made_of;
    use super::*;

    #[test]
    fn a_block_with_any_byte_changed_or_cut_gives_its_events_or_fails_corrupt() {
        let made = made_of(&seeds(), &mut Codec::default()).unwrap();
        let body = &made.blocks[0].body;
        let inner = &body[1..body.len() - 4];
        for at in 0..inner.len() {
            for flip in [0x01, 0x80, 0xff] {
                let mut changed = inner.to_vec();
                changed[at] ^= flip;
                rows(&changed);
            }
            rows(&inner[..at]);
        }
        let (schema, _) = Schema::of(&seeds());
        let read = block::parse(&schema, &sealed(block::VERSION, inner), &mut Codec::default()).unwrap();
        assert_eq!(read, seeds(), "sealed again, the block reads as it was made");
    }

    #[test]
    fn bytes_of_no_row_fail_corrupt() {
        for bytes in [&b""[..], b"\x00", b"\x01\x02\x03", &[0xff; 64], b"\x28\xb5\x2f\xfd\x00\x00"] {
            rows(bytes);
        }
    }
}
