//! A column of values, each kept as it was written: a field's values, the
//! JSON of each, or the messages of a block. Integers when every value is one
//! written the one way an integer is; text with the times it spells cut out,
//! when those pay; text otherwise. Either way each value comes back byte for
//! byte.
//!
//! ```text
//! 1920 1366 390 1920                     → integers
//! 1920 "1366" 1.0 01 -0                  → text: a string, a fraction, two spellings no integer has
//! "2026-09-23 00:47:32,100 GET /notes"   → stamped text: see `stamp`
//! ```

use super::bytes::Reader;
use super::head::Codec;
use super::ints::{put_ints, read_ints};
use super::stamp::{put_stamped, read_stamped};
use super::text::{Decoder, put_texts, read_texts};
use crate::Result;

const TEXT: u8 = 0;
const INTEGERS: u8 = 1;
const STAMPED: u8 = 2;

/// The integer a value is, when writing it again spells the same value.
fn integer(json: &str) -> Option<i64> {
    json.parse::<i64>().ok().filter(|number| number.to_string() == json)
}

/// Appends a column of values, `times` their events'; of none, nothing.
pub(super) fn put_values(out: &mut Vec<u8>, values: &[&str], times: &[i64], codec: &mut Codec) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    if let Some(integers) = values.iter().map(|json| integer(json)).collect::<Option<Vec<i64>>>() {
        out.push(INTEGERS);
        put_ints(out, &integers);
        return Ok(());
    }
    out.push(STAMPED);
    if put_stamped(out, values, times, codec)? {
        return Ok(());
    }
    out.pop();
    out.push(TEXT);
    put_texts(out, values, codec)
}

/// A column of values, one a time of `times`, their events'; of none,
/// without reading anything.
pub(super) fn read_values(reader: &mut Reader<'_>, times: &[i64], decoder: &mut Decoder<'_>) -> Vec<String> {
    if times.is_empty() {
        return Vec::new();
    }
    match reader.byte() {
        TEXT => read_texts(reader, times.len(), decoder),
        INTEGERS => read_ints(reader, times.len()).iter().map(i64::to_string).collect(),
        STAMPED => read_stamped(reader, times, decoder),
        _ => {
            reader.fail("a value column's kind");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(values: &[&str]) -> Vec<u8> {
        let mut codec = Codec::default();
        let mut out = Vec::new();
        let times = vec![1_800_000_000_000_000_000; values.len()];
        put_values(&mut out, values, &times, &mut codec).unwrap();
        let mut reader = Reader::new(&out);
        let read = read_values(&mut reader, &times, &mut Decoder { codec: &mut codec, left: 1 << 20 });
        reader.finish().unwrap();
        assert_eq!(read, values);
        out
    }

    #[test]
    fn integers_are_kept_as_integers_and_every_value_as_it_was_spelled() {
        assert_eq!(written(&["1920", "1366", "-390", "0", "9223372036854775807"])[0], INTEGERS);
        for spelled in ["01", "-0", "1.0", "1e3", "+1", "\"42\"", "9223372036854775808", "null", " 1"] {
            assert_eq!(written(&["7", spelled])[0], TEXT, "{spelled}");
        }
        written(&[]);
        written(&[r#"{"x": [1, "y"]}"#, "true", "null", r#""text""#]);
    }

    #[test]
    fn text_that_spells_times_is_kept_with_them_cut_out() {
        let lines: Vec<String> = (0..16).map(|n| format!("2027-01-15 08:00:00,{n:03} GET /notes/{n}")).collect();
        let column = written(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(column[0], STAMPED);
        assert_eq!(written(&["2027-01-15 08:00:00 once", "and text"])[0], TEXT);
    }

    #[test]
    fn a_counter_takes_no_bit_a_value() {
        let counted: Vec<String> = (1000..2024).map(|n| n.to_string()).collect();
        assert!(written(&counted.iter().map(String::as_str).collect::<Vec<_>>()).len() <= 8);
    }
}
