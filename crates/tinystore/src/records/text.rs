//! A text column: its distinct values once and an id a value when at most
//! half of them are distinct, its values one after another otherwise; either
//! way one zstd pass over the text, and none where zstd would add bytes. A
//! column of byte strings, a uuid's sixteen each, is written the same way.
//!
//! ```text
//! "GET /notes" "GET /notes" "POST /buy" "GET /notes"   → dictionary: 2 values, ids 0 0 1 0
//! "a1f3" "09bc" "77e0" "5d21"                          → lines "a1f3\n09bc\n77e0\n5d21\n"
//! "one\ntwo" "three"                                   → lengths 7 5, then the bytes
//! ```

use std::collections::HashMap;

use super::bytes::{Reader, put_count};
use super::head::Codec;
use super::ints::{put_ints, read_ints};
use crate::Result;

const VALUES: u8 = 0;
const DICTIONARY: u8 = 1;

const LINES: u8 = 0;
const LENGTHS: u8 = 1;

const STORED: u8 = 0;
const SQUEEZED: u8 = 1;

/// Below this a blob is stored as it is: zstd's frame would be most of it.
const WORTH_SQUEEZING: usize = 32;
/// Past this many distinct values a column of which half are distinct is
/// written whole.
const SMALL_DICTIONARY: usize = 16;

/// What reads columns: the zstd context, and the text it may still expand,
/// so that a block's frames together never ask for more than a block holds.
pub(super) struct Decoder<'c> {
    pub(super) codec: &'c mut Codec,
    pub(super) left: usize,
}

/// Appends a column of text; of none, nothing.
pub(super) fn put_texts(out: &mut Vec<u8>, values: &[&str], codec: &mut Codec) -> Result<()> {
    put_bytes(out, &values.iter().map(|value| value.as_bytes()).collect::<Vec<_>>(), codec)
}

/// Appends a column of byte strings; of none, nothing.
pub(super) fn put_bytes(out: &mut Vec<u8>, values: &[&[u8]], codec: &mut Codec) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let mut ids = HashMap::new();
    let mut distinct = Vec::new();
    for value in values {
        if !ids.contains_key(value) {
            if distinct.len() * 2 >= values.len() && distinct.len() > SMALL_DICTIONARY {
                out.push(VALUES);
                return put_whole(out, values, codec);
            }
            ids.insert(*value, distinct.len() as i64);
            distinct.push(*value);
        }
    }
    out.push(DICTIONARY);
    put_count(out, distinct.len() as u64);
    put_whole(out, &distinct, codec)?;
    put_ints(out, &values.iter().map(|value| ids[value]).collect::<Vec<_>>());
    Ok(())
}

/// Values one after another: a line each when none holds a line break, their
/// lengths and then their bytes otherwise.
fn put_whole(out: &mut Vec<u8>, values: &[&[u8]], codec: &mut Codec) -> Result<()> {
    if values.iter().all(|value| !value.contains(&b'\n')) {
        out.push(LINES);
        let mut blob = Vec::with_capacity(values.iter().map(|value| value.len() + 1).sum());
        values.iter().for_each(|value| {
            blob.extend_from_slice(value);
            blob.push(b'\n');
        });
        return put_blob(out, &blob, codec);
    }
    out.push(LENGTHS);
    put_ints(out, &values.iter().map(|value| value.len() as i64).collect::<Vec<_>>());
    put_blob(out, &values.concat(), codec)
}

/// Bytes through zstd, or as they are where that is no shorter.
pub(super) fn put_blob(out: &mut Vec<u8>, plain: &[u8], codec: &mut Codec) -> Result<()> {
    let squeezed = if plain.len() < WORTH_SQUEEZING { Vec::new() } else { codec.squeeze_packed(plain)? };
    if squeezed.is_empty() || squeezed.len() + 4 >= plain.len() {
        out.push(STORED);
        put_count(out, plain.len() as u64);
        out.extend_from_slice(plain);
        return Ok(());
    }
    out.push(SQUEEZED);
    put_count(out, plain.len() as u64);
    put_count(out, squeezed.len() as u64);
    out.extend_from_slice(&squeezed);
    Ok(())
}

/// The bytes `put_blob` wrote, within what the decoder may still expand.
pub(super) fn read_blob(reader: &mut Reader<'_>, decoder: &mut Decoder<'_>) -> Vec<u8> {
    let squeezed = match reader.byte() {
        STORED => false,
        SQUEEZED => true,
        _ => {
            reader.fail("a blob's kind");
            return Vec::new();
        }
    };
    let length = reader.count(decoder.left);
    decoder.left -= length.min(decoder.left);
    if !squeezed {
        return reader.take(length).to_vec();
    }
    let frame_length = reader.count(usize::MAX >> 1);
    let frame = reader.take(frame_length);
    match decoder.codec.expand(frame, length) {
        Ok(plain) if plain.len() == length => plain,
        _ => {
            reader.fail("a blob's frame");
            Vec::new()
        }
    }
}

/// A column of `count` texts; of none, without reading anything.
pub(super) fn read_texts(reader: &mut Reader<'_>, count: usize, decoder: &mut Decoder<'_>) -> Vec<String> {
    read_column(reader, count, decoder, |value| String::from_utf8(value).ok())
}

/// A column of `count` byte strings; of none, without reading anything.
pub(super) fn read_bytes(reader: &mut Reader<'_>, count: usize, decoder: &mut Decoder<'_>) -> Vec<Vec<u8>> {
    read_column(reader, count, decoder, Some)
}

/// A column's values, each made what `make` makes of its bytes, which a
/// dictionary's values are made once.
fn read_column<T: Clone>(
    reader: &mut Reader<'_>,
    count: usize,
    decoder: &mut Decoder<'_>,
    make: fn(Vec<u8>) -> Option<T>,
) -> Vec<T> {
    if count == 0 {
        return Vec::new();
    }
    match reader.byte() {
        VALUES => read_whole(reader, count, decoder, make),
        DICTIONARY => {
            let distinct = reader.count(count);
            let values = read_whole(reader, distinct, decoder, make);
            let ids = read_ints(reader, count);
            let named = ids.iter().map(|id| usize::try_from(*id).ok().and_then(|id| values.get(id)).cloned());
            named.collect::<Option<Vec<T>>>().unwrap_or_else(|| {
                reader.fail("a text column's id past its dictionary");
                Vec::new()
            })
        }
        _ => {
            reader.fail("a text column's kind");
            Vec::new()
        }
    }
}

fn read_whole<T>(
    reader: &mut Reader<'_>,
    count: usize,
    decoder: &mut Decoder<'_>,
    make: fn(Vec<u8>) -> Option<T>,
) -> Vec<T> {
    let layout = reader.byte();
    let lengths = if layout == LENGTHS { read_ints(reader, count) } else { Vec::new() };
    let blob = read_blob(reader, decoder);
    let values: Option<Vec<Vec<u8>>> = match layout {
        LINES => {
            // whole lines end with a break, past which a split finds nothing
            let mut lines: Vec<&[u8]> = blob.split(|byte| *byte == b'\n').collect();
            (lines.pop() == Some(&[][..]) && lines.len() == count)
                .then(|| lines.into_iter().map(<[u8]>::to_vec).collect())
        }
        LENGTHS => {
            let mut from = 0_usize;
            let cut = |length: &i64| {
                let to = from.checked_add(usize::try_from(*length).ok()?)?;
                let value = blob.get(from..to)?.to_vec();
                from = to;
                Some(value)
            };
            let values: Option<Vec<Vec<u8>>> = lengths.iter().map(cut).collect();
            values.filter(|values| values.len() == count && from == blob.len())
        }
        _ => None,
    };
    let Some(values) = values else {
        reader.fail("a text column whose values are not its count");
        return Vec::new();
    };
    values.into_iter().map(make).collect::<Option<Vec<T>>>().unwrap_or_else(|| {
        reader.fail("text that is not UTF-8");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(values: &[&str]) -> Vec<u8> {
        let mut codec = Codec::default();
        let mut out = Vec::new();
        put_texts(&mut out, values, &mut codec).unwrap();
        let mut reader = Reader::new(&out);
        let read = read_texts(&mut reader, values.len(), &mut Decoder { codec: &mut codec, left: 1 << 20 });
        reader.finish().unwrap();
        assert_eq!(read, values);
        out
    }

    #[test]
    fn any_texts_come_back() {
        written(&[]);
        written(&[""]);
        written(&["", "", ""]);
        written(&["one\ntwo", "three", "", "caf\u{e9}\n"]);
        written(&["GET /notes", "GET /notes", "POST /buy", "GET /notes"]);
        let distinct: Vec<String> = (0..200).map(|n| format!("request {n} of a long enough line to squeeze")).collect();
        written(&distinct.iter().map(String::as_str).collect::<Vec<_>>());
    }

    #[test]
    fn byte_strings_come_back_whatever_bytes_they_hold() {
        let mut codec = Codec::default();
        let values: Vec<Vec<u8>> = (0..64_u8).map(|n| vec![n, b'\n', 0xff, n.wrapping_mul(37)]).collect();
        let values: Vec<&[u8]> = values.iter().map(Vec::as_slice).chain([&[][..], &[0xc3]]).collect();
        let mut out = Vec::new();
        put_bytes(&mut out, &values, &mut codec).unwrap();
        let mut reader = Reader::new(&out);
        let read = read_bytes(&mut reader, values.len(), &mut Decoder { codec: &mut codec, left: 1 << 20 });
        reader.finish().unwrap();
        assert_eq!(read, values);
        // the same bytes read as text are refused, a byte that starts a letter and ends nothing among them
        let mut reader = Reader::new(&out);
        read_texts(&mut reader, values.len(), &mut Decoder { codec: &mut codec, left: 1 << 20 });
        assert_eq!(reader.finish().unwrap_err().to_string(), "text that is not UTF-8: corrupt");
    }

    #[test]
    fn lines_short_of_their_count_or_their_last_break_are_refused() {
        let mut codec = Codec::default();
        for (blob, count) in [(&b"a\nb"[..], 1), (b"a\nb", 2), (b"a\nb\n", 3), (b"", 1)] {
            let mut column = vec![VALUES, LINES, STORED];
            put_count(&mut column, blob.len() as u64);
            column.extend_from_slice(blob);
            let mut reader = Reader::new(&column);
            read_texts(&mut reader, count, &mut Decoder { codec: &mut codec, left: 1 << 20 });
            assert!(reader.finish().is_err(), "{blob:?}");
        }
    }

    #[test]
    fn values_that_repeat_are_written_once() {
        let routes = ["GET /notes", "POST /buy", "GET /login"];
        let repeated: Vec<&str> = (0..1024).map(|n| routes[n * 7 % 3]).collect();
        // two bits an id, and the three routes once
        assert!(written(&repeated).len() < 1024 / 4 + 64, "{}", written(&repeated).len());
        assert_eq!(written(&repeated)[0], DICTIONARY);
        let distinct: Vec<String> = (0..64).map(|n| format!("{n:x}")).collect();
        assert_eq!(written(&distinct.iter().map(String::as_str).collect::<Vec<_>>())[0], VALUES);
    }

    #[test]
    fn text_past_what_a_decoder_may_expand_is_refused_before_it_is_expanded() {
        let mut codec = Codec::default();
        let long = "x".repeat(4096);
        let mut out = Vec::new();
        put_texts(&mut out, &[&long], &mut codec).unwrap();
        let mut reader = Reader::new(&out);
        read_texts(&mut reader, 1, &mut Decoder { codec: &mut codec, left: 4000 });
        assert_eq!(reader.finish().unwrap_err().to_string(), "a count past its bound: corrupt");
    }

    #[test]
    fn what_a_decoder_expands_is_counted_across_its_columns() {
        let mut codec = Codec::default();
        let long = "x".repeat(3000);
        let mut column = Vec::new();
        put_texts(&mut column, &[&long], &mut codec).unwrap();
        let mut decoder = Decoder { codec: &mut codec, left: 4000 };
        let mut first = Reader::new(&column);
        assert_eq!(read_texts(&mut first, 1, &mut decoder), [long.as_str()]);
        first.finish().unwrap();
        let mut second = Reader::new(&column);
        read_texts(&mut second, 1, &mut decoder);
        assert_eq!(second.finish().unwrap_err().to_string(), "a count past its bound: corrupt");
    }

    #[test]
    fn a_column_whose_bytes_changed_is_said_and_never_more() {
        let mut codec = Codec::default();
        let lines: Vec<String> = (0..40).map(|n| format!("line {n} of text\nwith a break")).collect();
        let values: Vec<&str> = lines.iter().map(String::as_str).collect();
        for column in [written(&values), written(&["a", "b", "a", "a", "b"])] {
            for at in 0..column.len() {
                let mut changed = column.clone();
                changed[at] ^= 0x21;
                for bytes in [&changed[..], &column[..at]] {
                    let mut reader = Reader::new(bytes);
                    let count = if column.len() > 60 { 40 } else { 5 };
                    read_texts(&mut reader, count, &mut Decoder { codec: &mut codec, left: 1 << 20 });
                    // a changed byte of the text itself reads as other text: the row's checksum is what catches it
                    let _ = reader.finish();
                }
            }
        }
    }
}
