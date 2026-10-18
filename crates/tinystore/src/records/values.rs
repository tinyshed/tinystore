//! A column of values, each kept as it was written: a field's values, the
//! JSON of each, or the messages of a block. When every value is quoted the
//! quotes come off, so that what is inside is typed: integers, as they are or
//! as how far behind their events' times they lie; uuids and hex as their
//! bytes; integers but for a few values, kept apart; text with the times it
//! spells cut out, when those pay; text otherwise. Either way each value
//! comes back byte for byte.
//!
//! ```text
//! 1920 1366 390 1920                        → integers
//! 1727300000121 at 1727300000123456789 ns   → an integer counting time: 2 ms behind its event
//! "9cbaf3d1-0c27-47bc-8fed-cb6e0763b9b2"    → a quoted uuid: sixteen bytes
//! "a1f3c2e4" "09bc77e0"                     → quoted hex: four bytes each
//! 7 12 null 9 3 5 8 1                       → integers, and null apart at its place
//! 1920 "1366" 1.0 01 -0                     → text: a string, a fraction, two spellings no integer has
//! "2026-09-23 00:47:32,100 GET /notes"      → stamped text: see `stamp`
//! ```

use super::head::Codec;
use super::ints::{put_direct, put_ints, read_direct, read_ints};
use super::stamp::{MOST_DIGITS, behind, put_stamped, read_stamped};
use super::text::{Decoder, Keep, put_bytes, put_texts, read_bytes, read_texts, texts_equal, wants};
use crate::Result;
use crate::compress::varint::{Reader, put_count};

// what a column's values are, in the low bits of its first byte
const TEXT: u8 = 0;
const INTEGERS: u8 = 1;
const STAMPED: u8 = 2;
const HEX: u8 = 3;
const UUID: u8 = 4;
const KIND: u8 = 0x0f;
// and what was done to them, in its high bits
const QUOTED: u8 = 1 << 4;
/// Integers but for a few values, which are kept apart with their places.
const EXCEPTIONS: u8 = 1 << 5;
/// Integers kept as how far behind their events' times they lie.
const TIMED: u8 = 1 << 6;

/// The units an integer may count time in, as the digits of a second it
/// has: seconds, milliseconds, microseconds and nanoseconds.
const TIME_UNITS: [u8; 4] = [0, 3, 6, 9];
/// The values a column needs before its integers are tried as times, or
/// kept with a few values that are not integers apart.
const TYPED_VALUES: usize = 8;
/// The values a unit of time is screened on.
const SCREENED: usize = 16;

/// The integer a value is, when writing it again spells the same value.
fn integer(json: &str) -> Option<i64> {
    json.parse::<i64>().ok().filter(|number| number.to_string() == json)
}

/// What is inside a value's quotes, when it has them.
use super::logfmt::unquote as unquoted;

/// Appends a column of values, `times` their events'; of none, nothing.
pub(super) fn put_values(out: &mut Vec<u8>, values: &[&str], times: &[i64], codec: &mut Codec) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let (quoted, inner) = match values.iter().map(|value| unquoted(value)).collect::<Option<Vec<&str>>>() {
        Some(inner) => (QUOTED, inner),
        None => (0, values.to_vec()),
    };
    if put_typed(out, quoted, &inner, times, codec)? {
        return Ok(());
    }
    out.push(quoted | STAMPED);
    if put_stamped(out, &inner, times, codec)? {
        return Ok(());
    }
    out.pop();
    out.push(quoted | TEXT);
    put_texts(out, &inner, codec)
}

/// Appends values that are all integers, all uuids or all hex of one width,
/// or unquoted integers but for one in eight at most; says whether it did.
fn put_typed(out: &mut Vec<u8>, quoted: u8, inner: &[&str], times: &[i64], codec: &mut Codec) -> Result<bool> {
    let integers: Vec<i64> = inner.iter().filter_map(|value| integer(value)).collect();
    if integers.len() == inner.len() {
        put_integers(out, quoted, &integers, times);
    } else if inner.iter().all(|value| is_uuid(value)) {
        out.push(quoted | UUID);
        put_hex(out, inner, codec)?;
    } else if inner.iter().all(|value| value.len() == inner[0].len() && is_hex(value)) {
        out.push(quoted | HEX);
        put_count(out, inner[0].len() as u64 / 2);
        put_hex(out, inner, codec)?;
    } else if quoted == 0 && inner.len() >= TYPED_VALUES && integers.len() * 8 >= inner.len() * 7 {
        out.push(INTEGERS | EXCEPTIONS);
        put_exceptions(out, inner, codec)?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// Appends integers as they are, or as how far behind their events' times
/// they lie in the unit that takes fewest bytes, when one takes fewer.
fn put_integers(out: &mut Vec<u8>, quoted: u8, integers: &[i64], times: &[i64]) {
    let mut cheapest = (quoted | INTEGERS, Vec::new());
    put_ints(&mut cheapest.1, integers);
    if integers.len() >= TYPED_VALUES {
        for digits in TIME_UNITS {
            let Some(behind) = counted_behind(integers, times, digits) else { continue };
            let mut timed = vec![digits];
            put_ints(&mut timed, &behind);
            if timed.len() < cheapest.1.len() {
                cheapest = (quoted | INTEGERS | TIMED, timed);
            }
        }
    }
    out.push(cheapest.0);
    out.extend_from_slice(&cheapest.1);
}

/// How far behind its event each integer lies, counted in a unit of
/// `digits`, when the integers count time in it: the first sixteen lie behind
/// their events within a quarter of how far apart they lie themselves.
fn counted_behind(integers: &[i64], times: &[i64], digits: u8) -> Option<Vec<i64>> {
    let spread = |values: &[i64]| {
        let (low, high) = (values.iter().min().copied().unwrap_or(0), values.iter().max().copied().unwrap_or(0));
        u128::from(high.abs_diff(low))
    };
    let all_behind = |count: usize| {
        integers[..count]
            .iter()
            .zip(times)
            .map(|(integer, at)| behind(*at, *integer, digits))
            .collect::<Option<Vec<_>>>()
    };
    let sample = SCREENED.min(integers.len());
    (4 * spread(&all_behind(sample)?) < spread(&integers[..sample])).then(|| all_behind(integers.len())).flatten()
}

/// Appends integers but for a few values: where those stand, they as text,
/// and then the integers.
fn put_exceptions(out: &mut Vec<u8>, inner: &[&str], codec: &mut Codec) -> Result<()> {
    let (mut places, mut texts, mut integers) = (Vec::new(), Vec::new(), Vec::new());
    for (place, value) in inner.iter().enumerate() {
        match integer(value) {
            Some(number) => integers.push(number),
            None => {
                places.push(place as i64);
                texts.push(*value);
            }
        }
    }
    put_count(out, places.len() as u64);
    put_direct(out, &places);
    put_texts(out, &texts, codec)?;
    put_ints(out, &integers);
    Ok(())
}

/// Lowercase hex of whole bytes, four of them at least.
fn is_hex(text: &str) -> bool {
    text.len() >= 8 && text.len().is_multiple_of(2) && text.bytes().all(is_hex_digit)
}

/// A uuid in lowercase hex, its dashes where a uuid has them.
fn is_uuid(text: &str) -> bool {
    let dash = |place: usize| matches!(place, 8 | 13 | 18 | 23);
    let spelled = |(place, digit): (usize, u8)| if dash(place) { digit == b'-' } else { is_hex_digit(digit) };
    text.len() == 36 && text.bytes().enumerate().all(spelled)
}

fn is_hex_digit(digit: u8) -> bool {
    matches!(digit, b'0'..=b'9' | b'a'..=b'f')
}

/// Appends hex values, a uuid's dashes skipped, as their bytes.
fn put_hex(out: &mut Vec<u8>, inner: &[&str], codec: &mut Codec) -> Result<()> {
    let value = |digit: u8| if digit <= b'9' { digit - b'0' } else { digit - b'a' + 10 };
    let bytes_of = |text: &&str| {
        let digits: Vec<u8> = text.bytes().filter(|digit| *digit != b'-').map(value).collect();
        digits.chunks(2).map(|pair| pair[0] << 4 | pair[1]).collect::<Vec<u8>>()
    };
    let bytes: Vec<Vec<u8>> = inner.iter().map(bytes_of).collect();
    put_bytes(out, &bytes.iter().map(Vec::as_slice).collect::<Vec<_>>(), codec)
}

/// A column of values, one a time of `times`, their events', those `keep`
/// drops left empty; of none, without reading anything.
pub(super) fn read_values(
    reader: &mut Reader<'_>,
    times: &[i64],
    decoder: &mut Decoder<'_>,
    keep: Keep<'_>,
) -> Vec<String> {
    if times.is_empty() {
        return Vec::new();
    }
    let kind = reader.byte();
    let values = read_inner(reader, kind, (times, keep), decoder);
    if kind & QUOTED == 0 {
        return values;
    }
    // the quotes put back are counted before they are added, as a stamp is
    if 2 * values.len() > decoder.left {
        reader.fail("quotes past what their block may expand to");
        return Vec::new();
    }
    decoder.left -= 2 * values.len();
    let quoted = |(place, value): (usize, String)| if wants(keep, place) { format!("\"{value}\"") } else { value };
    values.into_iter().enumerate().map(quoted).collect()
}

/// Which values of a column are written as `json`, compared as the column
/// keeps them: text by its bytes, a dictionary's distinct values once, and
/// integers as integers, so that no value is spelled to be compared. A
/// quoted column's values are its quotes' insides.
pub(super) fn values_equal(reader: &mut Reader<'_>, times: &[i64], decoder: &mut Decoder<'_>, json: &str) -> Vec<bool> {
    if times.is_empty() {
        return Vec::new();
    }
    let kind = reader.byte();
    let inner = if kind & QUOTED == 0 { Some(json) } else { unquoted(json) };
    match (kind & KIND, kind & !(KIND | QUOTED), inner) {
        (TEXT, 0, Some(inner)) => texts_equal(reader, times.len(), decoder, inner.as_bytes()),
        (INTEGERS, 0, Some(inner)) => {
            let integer = integer(inner);
            read_ints(reader, times.len()).iter().map(|value| Some(*value) == integer).collect()
        }
        // a JSON without quotes is no value of a quoted column, which is still read through
        _ => {
            read_inner(reader, kind, (times, None), decoder).iter().map(|value| inner == Some(value.as_str())).collect()
        }
    }
}

/// What is inside a column's values, by the kind its first byte says.
fn read_inner(
    reader: &mut Reader<'_>,
    kind: u8,
    (times, keep): (&[i64], Keep<'_>),
    decoder: &mut Decoder<'_>,
) -> Vec<String> {
    let count = times.len();
    match (kind & KIND, kind & !(KIND | QUOTED)) {
        (TEXT, 0) => read_texts(reader, count, decoder, keep),
        (INTEGERS, 0) => spelled(read_ints(reader, count).into_iter().map(Some), keep).unwrap_or_default(),
        (INTEGERS, TIMED) => read_timed(reader, times, keep),
        (INTEGERS, EXCEPTIONS) if kind & QUOTED == 0 => read_exceptions(reader, count, decoder),
        (STAMPED, 0) => read_stamped(reader, times, decoder, keep),
        (UUID, 0) => read_hex(reader, Fixed { count, width: 16, uuid: true }, decoder, keep),
        (HEX, 0) => {
            let width = reader.count(decoder.left);
            read_hex(reader, Fixed { count, width, uuid: false }, decoder, keep)
        }
        _ => {
            reader.fail("a value column's kind");
            Vec::new()
        }
    }
}

/// Integers as they are written, those `keep` drops left empty; none when one
/// is missing.
fn spelled(integers: impl Iterator<Item = Option<i64>>, keep: Keep<'_>) -> Option<Vec<String>> {
    let spell = |(place, integer): (usize, Option<i64>)| {
        if wants(keep, place) { integer.map(|integer| integer.to_string()) } else { Some(String::new()) }
    };
    integers.enumerate().map(spell).collect()
}

/// Integers kept as how far behind their events' times they lie.
fn read_timed(reader: &mut Reader<'_>, times: &[i64], keep: Keep<'_>) -> Vec<String> {
    let digits = reader.byte();
    let distances = read_ints(reader, times.len());
    if digits > MOST_DIGITS {
        reader.fail("the unit integers count time in");
        return Vec::new();
    }
    let integers = distances.iter().zip(times).map(|(distance, at)| behind(*at, *distance, digits));
    spelled(integers, keep).unwrap_or_else(|| {
        reader.fail("an integer counting time past sixty-four bits");
        Vec::new()
    })
}

/// Integers and, at their places, the few values that are not.
fn read_exceptions(reader: &mut Reader<'_>, count: usize, decoder: &mut Decoder<'_>) -> Vec<String> {
    let apart = reader.count(count);
    let places = read_direct(reader, apart);
    let texts = read_texts(reader, apart, decoder, None);
    let mut integers = read_ints(reader, count - apart).into_iter().map(|integer| integer.to_string());
    let mut values = Vec::with_capacity(count);
    for (place, text) in places.iter().zip(texts) {
        let Some(place) = usize::try_from(*place).ok().filter(|place| (values.len()..count).contains(place)) else {
            reader.fail("a value apart out of its place");
            return Vec::new();
        };
        values.extend(integers.by_ref().take(place - values.len()));
        values.push(text);
    }
    values.extend(integers);
    values
}

/// The values of a column of hex: how many, the bytes of each, and whether
/// they are uuids, spelled with their dashes.
struct Fixed {
    count: usize,
    width: usize,
    uuid: bool,
}

/// Bytes of a width each as lowercase hex, those `keep` drops left empty.
fn read_hex(reader: &mut Reader<'_>, fixed: Fixed, decoder: &mut Decoder<'_>, keep: Keep<'_>) -> Vec<String> {
    let Fixed { count, width, uuid } = fixed;
    let bytes = read_bytes(reader, count, decoder, keep);
    // what the digits add to the bytes is counted before they are spelled
    let spelled = count.saturating_mul(width.saturating_add(4));
    let wrong = bytes.iter().enumerate().any(|(place, bytes)| wants(keep, place) && bytes.len() != width);
    if width == 0 || spelled > decoder.left || wrong {
        reader.fail("hex values of a width other than their column's");
        return Vec::new();
    }
    decoder.left -= spelled;
    let spell = |(place, bytes): (usize, &Vec<u8>)| {
        let mut text = String::with_capacity(if wants(keep, place) { 2 * width + 4 } else { 0 });
        for (at, byte) in bytes.iter().enumerate() {
            if uuid && matches!(at, 4 | 6 | 8 | 10) {
                text.push('-');
            }
            text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            text.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        text
    };
    bytes.iter().enumerate().map(spell).collect()
}

/// What a column's first byte says it holds, for the tests that check a
/// block's columns.
#[cfg(test)]
pub(super) fn kind_of(first: u8) -> String {
    let kind = ["text", "integers", "stamped", "hex", "uuid"].get(usize::from(first & KIND)).unwrap_or(&"?");
    let flags = [(QUOTED, "quoted "), (EXCEPTIONS, "apart "), (TIMED, "timed ")];
    let said: String = flags.iter().filter(|(flag, _)| first & flag != 0).map(|(_, name)| *name).collect();
    format!("{said}{kind}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: i64 = 1_800_000_000_123_456_789;

    fn written_at(values: &[&str], times: &[i64]) -> Vec<u8> {
        let mut codec = Codec::default();
        let mut out = Vec::new();
        put_values(&mut out, values, times, &mut codec).unwrap();
        let mut reader = Reader::new(&out);
        let read = read_values(&mut reader, times, &mut Decoder { codec: &mut codec, left: 1 << 20 }, None);
        reader.finish().unwrap();
        assert_eq!(read, values);
        out
    }

    fn written(values: &[&str]) -> Vec<u8> {
        written_at(values, &vec![AT; values.len()])
    }

    fn strs(values: &[String]) -> Vec<&str> {
        values.iter().map(String::as_str).collect()
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
    fn quoted_values_are_typed_by_what_is_inside_and_come_back_quoted() {
        let uuids: Vec<String> = (0..64_u64)
            .map(|n| format!("\"9cbaf3d1-0c27-47bc-8fed-{:012x}\"", n.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 16))
            .collect();
        let column = written(&strs(&uuids));
        assert_eq!(column[0], QUOTED | UUID);
        // sixteen bytes each, where the text is thirty-eight
        assert!(column.len() <= 64 * 16 + 32, "{}", column.len());
        let hex: Vec<String> =
            (0..64_u64).map(|n| format!("\"{:016x}\"", n.wrapping_mul(0x9e37_79b9_7f4a_7c15))).collect();
        assert_eq!(written(&strs(&hex))[..2], [QUOTED | HEX, 8]);
        assert_eq!(written(&["\"154\"", "\"7\"", "\"-2\""])[0], QUOTED | INTEGERS);
        assert_eq!(written(&["\"GET\"", "\"POST\"", "\"\""])[0], QUOTED | TEXT);
        assert_eq!(written(&["\"GET\"", "POST"])[0], TEXT);
        for not_hex in ["\"9CBAF3D1-0C27-47BC-8FED-CB6E0763B9B2\"", "\"a1f3c2e\"", "\"a1f3c2e4a1\""] {
            assert_eq!(written(&["\"a1f3c2e4\"", not_hex])[0], QUOTED | TEXT, "{not_hex}");
        }
        assert_eq!(written(&["\"", "\""])[0], TEXT);
    }

    #[test]
    fn integers_that_count_time_are_kept_as_how_far_behind_their_events_they_lie() {
        let times: Vec<i64> = (0..256).map(|n| AT + n * 7_919_000_017).collect();
        let millis: Vec<String> =
            times.iter().enumerate().map(|(n, at)| (at / 1_000_000 - n as i64 % 3).to_string()).collect();
        let column = written_at(&strs(&millis), &times);
        assert_eq!(column[..2], [INTEGERS | TIMED, 3]);
        // two bits behind each, where a step from one millisecond to the next takes three
        assert!(column.len() <= 256 * 2 / 8 + 16, "{}", column.len());
        let seconds: Vec<String> = times.iter().map(|at| format!("\"{}\"", at / 1_000_000_000)).collect();
        assert_eq!(written_at(&strs(&seconds), &times)[..2], [QUOTED | INTEGERS | TIMED, 0]);
        let counts: Vec<String> = (0..256).map(|n| (n * 37 % 1000).to_string()).collect();
        assert_eq!(written_at(&strs(&counts), &times)[0], INTEGERS);
    }

    #[test]
    fn integers_but_for_a_few_keep_those_apart() {
        let mut values: Vec<&str> =
            vec!["7", "12", "9", "3", "5", "8", "1", "40", "2", "6", "11", "4", "13", "10", "0", "1"];
        values[2] = "null";
        values[15] = "1.5";
        assert_eq!(written(&values)[0], INTEGERS | EXCEPTIONS);
        values[7] = "\"40\"";
        assert_eq!(written(&values)[0], TEXT);
        let quoted: Vec<String> = values.iter().map(|value| format!("\"{value}\"")).collect();
        assert_eq!(written(&strs(&quoted))[0] & !QUOTED, TEXT);
    }

    #[test]
    fn text_that_spells_times_is_kept_with_them_cut_out() {
        let lines: Vec<String> = (0..16).map(|n| format!("2027-01-15 08:00:00,{n:03} GET /notes/{n}")).collect();
        assert_eq!(written(&strs(&lines))[0], STAMPED);
        let quoted: Vec<String> = lines.iter().map(|line| format!("\"{line}\"")).collect();
        assert_eq!(written(&strs(&quoted))[0], QUOTED | STAMPED);
        assert_eq!(written(&["2027-01-15 08:00:00 once", "and text"])[0], TEXT);
    }

    #[test]
    fn a_column_is_compared_as_it_keeps_its_values_and_none_is_spelled() {
        let equal = |values: &[&str], json: &str| {
            let mut codec = Codec::default();
            let (mut out, times) = (Vec::new(), vec![AT; values.len()]);
            put_values(&mut out, values, &times, &mut codec).unwrap();
            let mut reader = Reader::new(&out);
            let equal = values_equal(&mut reader, &times, &mut Decoder { codec: &mut codec, left: 1 << 20 }, json);
            reader.finish().unwrap();
            assert_eq!(equal, values.iter().map(|value| *value == json).collect::<Vec<_>>(), "{json}");
        };
        let routes: Vec<&str> = (0..40).map(|n| ["\"/notes\"", "\"/buy\""][n % 3 / 2]).collect();
        equal(&routes, "\"/buy\"");
        // a JSON without quotes is no value of a quoted column
        equal(&routes, "/buy");
        equal(&["7", "12", "7"], "7");
        equal(&["7", "12", "7"], "07");
        let lines: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        equal(&strs(&lines), "line 17");
        let uuids: Vec<String> = (0..16).map(|n| format!("\"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9{n:02x}\"")).collect();
        equal(&strs(&uuids), &uuids[5]);
        let mut apart: Vec<&str> = vec!["1"; 16];
        apart[3] = "null";
        equal(&apart, "null");
    }

    #[test]
    fn a_column_read_for_a_few_values_leaves_the_others_empty() {
        let lines: Vec<String> = (0..40).map(|n| format!("2027-01-15 08:00:00,{n:03} GET /notes/{n}")).collect();
        let uuids: Vec<String> = (0..40).map(|n| format!("\"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9{n:02x}\"")).collect();
        let numbers: Vec<String> = (0..40).map(|n| (n * 37).to_string()).collect();
        let keep: Vec<bool> = (0..40).map(|n| n % 7 == 2).collect();
        for values in [&lines, &uuids, &numbers] {
            let mut codec = Codec::default();
            let (mut out, times) = (Vec::new(), vec![AT; values.len()]);
            put_values(&mut out, &strs(values), &times, &mut codec).unwrap();
            let mut reader = Reader::new(&out);
            let read = read_values(&mut reader, &times, &mut Decoder { codec: &mut codec, left: 1 << 20 }, Some(&keep));
            reader.finish().unwrap();
            let expected = values.iter().zip(&keep).map(|(value, kept)| if *kept { value.as_str() } else { "" });
            assert_eq!(read, expected.collect::<Vec<_>>());
        }
    }

    #[test]
    fn a_counter_takes_no_bit_a_value() {
        let counted: Vec<String> = (1000..2024).map(|n| n.to_string()).collect();
        assert!(written(&strs(&counted)).len() <= 8);
    }

    #[test]
    fn a_column_of_a_kind_nobody_wrote_or_cut_short_is_refused() {
        let mut codec = Codec::default();
        let times: Vec<i64> = (0..16).map(|n| AT + n * 1_000_000_007).collect();
        let millis: Vec<String> = times.iter().map(|at| (at / 1_000_000).to_string()).collect();
        let uuids: Vec<String> = (0..16).map(|n| format!("\"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9{n:02x}\"")).collect();
        let mut apart: Vec<String> = (0..16).map(|n| n.to_string()).collect();
        apart[5] = "null".into();
        for values in [millis, uuids, apart] {
            let mut column = Vec::new();
            put_values(&mut column, &strs(&values), &times, &mut codec).unwrap();
            for cut in 0..column.len() {
                let mut reader = Reader::new(&column[..cut]);
                read_values(&mut reader, &times, &mut Decoder { codec: &mut codec, left: 1 << 20 }, None);
                assert!(reader.finish().is_err(), "cut at {cut} of {}", column.len());
            }
        }
        let refused = |column: &[u8], count: usize, left: usize| {
            let mut codec = Codec::default();
            let mut reader = Reader::new(column);
            read_values(&mut reader, &times[..count], &mut Decoder { codec: &mut codec, left }, None);
            assert!(reader.finish().is_err(), "{column:?}");
        };
        let mut timed = vec![INTEGERS | TIMED, 10];
        put_ints(&mut timed, &[0, 1]);
        refused(&timed, 2, 1 << 20);
        let mut far = vec![INTEGERS | TIMED, 3];
        put_ints(&mut far, &[i64::MIN, 0]);
        refused(&far, 2, 1 << 20);
        for kind in [QUOTED | INTEGERS | EXCEPTIONS, INTEGERS | TIMED | EXCEPTIONS, STAMPED | TIMED, 5, 1 << 7] {
            refused(&[kind, 0, 0, 0, 0, 0, 0], 1, 1 << 20);
        }
        let mut narrow = vec![UUID];
        put_bytes(&mut narrow, &[&[7; 15][..]], &mut Codec::default()).unwrap();
        refused(&narrow, 1, 1 << 20);
        let mut wide = vec![HEX, 0];
        put_bytes(&mut wide, &[&[][..]], &mut Codec::default()).unwrap();
        refused(&wide, 1, 1 << 20);
        let mut uuid = vec![QUOTED | UUID];
        put_bytes(&mut uuid, &[&[7; 16][..]], &mut Codec::default()).unwrap();
        // its line of seventeen bytes, the twenty its digits and dashes add, and two quotes, a column
        let read = |left: usize, columns: usize| {
            let mut codec = Codec::default();
            let mut decoder = Decoder { codec: &mut codec, left };
            (0..columns).all(|_| {
                let mut reader = Reader::new(&uuid);
                read_values(&mut reader, &times[..1], &mut decoder, None);
                reader.finish().is_ok()
            })
        };
        assert!(read(39, 1) && !read(38, 1) && read(78, 2) && !read(77, 2));
        // a place past the column, taken twice, or behind the one before
        for places in [&[4][..], &[1, 1], &[3, 1]] {
            let mut placed = vec![INTEGERS | EXCEPTIONS, places.len() as u8];
            put_direct(&mut placed, places);
            put_texts(&mut placed, &vec!["null"; places.len()], &mut Codec::default()).unwrap();
            put_ints(&mut placed, &vec![1; 4 - places.len()]);
            refused(&placed, 4, 1 << 20);
        }
    }
}
