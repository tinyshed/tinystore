//! The bodies of records' fuzz targets, which `fuzz/` runs by hand on
//! nightly through cargo-fuzz, and tests run on seeds on every platform CI
//! runs on: what a decoder does with any bytes past the version and the
//! checksum that guard them, and what a writer of lines does with any text.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::value::RawValue;

use super::block;
use super::engine::Records;
use super::event::{Event, Kept, MAX_EVENT, NewEvent, weight};
use super::head::{self, Codec};
use super::lines::{LOGFMT, TEXT};
use super::logfmt::spell;
use super::segment::{self, Schema};
use crate::compress::checksum::crc32;
use crate::console::quoted;
use crate::{ErrorKind, Result, Store};

/// The streams `lines` writes, one an input.
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Writes any text through a writer of lines, in pieces of as many bytes as
/// its first byte says, and reads its stream back: every line one event
/// holds comes back as it was written, and a longer one is dropped and
/// counted.
pub fn lines(store: &Store, bytes: &[u8]) {
    let Some((&piece, text)) = bytes.split_first() else { return };
    let text = String::from_utf8_lossy(text);
    let stream = format!("fuzz-{}", WRITTEN.fetch_add(1, Ordering::Relaxed));
    let events = store.events(&stream).open().expect("a stream to write");
    let mut out = events.lines().expect("a writer of lines");
    for bytes in text.as_bytes().chunks(usize::from(piece).max(1)) {
        out.write_all(bytes).expect("a write of lines takes every byte");
    }
    let dropped = out.dropped();
    drop(out);
    Records::of(store).and_then(|records| records.flush_held(false)).expect("the lines written");
    let found = events.list().limit(10_000).page().expect("the lines read").events;
    let spelled: Vec<String> = found.iter().map(line_of).collect();
    let room = MAX_EVENT - weight(TEXT, None, &[], &[]);
    let lines = text.strip_suffix('\n').unwrap_or(&text);
    let kept: Vec<&str> = lines.split('\n').filter(|line| line.len() <= room).collect();
    let longer = lines.split('\n').count() - kept.len();
    assert_eq!((spelled.join("\n"), dropped), (kept.join("\n"), longer as u64), "{stream}");
}

/// The line an event of a writer of lines was made of, spelled again: its
/// message, a logfmt line's pairs, or a JSON line's fields.
pub(super) fn line_of(event: &Event) -> String {
    let fields: Vec<(String, String)> =
        event.fields.iter().map(|(key, json)| (key.to_owned(), json.to_owned())).collect();
    match event.name.as_str() {
        TEXT => event.message.clone().unwrap_or_default(),
        LOGFMT => spell(&fields),
        _ => format!(
            "{{{}}}",
            fields.iter().map(|(key, json)| format!("{}:{json}", quoted(key))).collect::<Vec<_>>().join(",")
        ),
    }
}

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
    fn any_text_cut_anywhere_comes_back_line_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let options = crate::Options { background: false, ..crate::Options::default() };
        let store = Store::open(dir.path(), options).unwrap();
        let seeds: [&[u8]; 6] = [
            b"\x01a\nb",
            b"\x03{\n\"a\": [1,\n2]}\nplain\r\n\n",
            b"\x02level=info msg=\"slow \\\"request\\\"\" ms=1200 ok=true\nlevel=warn  msg=x\na= b=\n",
            b"\x07Traceback (most recent call last):\n  File \"app.py\"\nValueError: bad input\nnext",
            b"\x05\xff\xfe no utf-8 \xc3\n\n\n",
            b"\x00",
        ];
        for seed in seeds {
            lines(&store, seed);
        }
    }

    #[test]
    fn bytes_of_no_row_fail_corrupt() {
        for bytes in [&b""[..], b"\x00", b"\x01\x02\x03", &[0xff; 64], b"\x28\xb5\x2f\xfd\x00\x00"] {
            rows(bytes);
        }
    }
}
