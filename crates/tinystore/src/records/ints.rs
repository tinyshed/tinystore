//! An integer column, written the cheapest exact way of those priced.
//!
//! Directly: its values, the steps from each to the next, or how far each
//! lies off a steady step; either way less the smallest, divided by what
//! divides them all, and then each in the bits the largest needs or as a rice
//! code, whichever takes fewer bytes.
//!
//! ```text
//! times, a millisecond's clock in ns    1000000000   1002000000   1005000000
//! the steps                                          2000000      3000000
//! less the smallest, 2000000                         0            1000000
//! divided by 1000000                                 0            1          a bit each
//! ```
//!
//! Or, of eight values or more, as a dictionary of the few distinct ones, or
//! as the value seven in eight share with the others apart:
//!
//! ```text
//! 200 404 200 500 200 200 200 404        → dictionary 200 404 500, ids 0 1 0 2 0 0 0 1
//! 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1000000  → 0, and 1000000 at place 15
//! ```
//!
//! Every step and every difference wraps around as its inverse does, so any
//! sixty-four bits come back.

use std::collections::HashMap;

use crate::compress::bits::{BitReader, BitWriter, RiceCost, pack, packed_len, read_rice, unpack, write_rice};
use crate::compress::varint::{Reader, put_count, put_int};

// what the integers written are
const VALUES: u8 = 0;
const STEPS: u8 = 1;
const TREND: u8 = 2;
const DICTIONARY: u8 = 3;
const DOMINANT: u8 = 4;

// how they are packed
const WIDTH: u8 = 0;
const RICE: u8 = 1;

/// The values a column needs before a steady step is looked for in it.
const TREND_VALUES: usize = 16;
/// The values a column needs before a dictionary or a dominant value is.
const COUNTED_VALUES: usize = 8;
/// The distinct values a dictionary holds at most.
const MOST_DISTINCT: usize = 256;

/// Appends integers less their smallest and divided by what divides them
/// all, in one width or as rice codes.
fn put_reduced(out: &mut Vec<u8>, values: impl Iterator<Item = i64> + Clone) {
    let base = values.clone().min().unwrap_or(0);
    let mut quotients: Vec<u64> = values.map(|value| value.wrapping_sub(base) as u64).collect();
    let divisor = quotients.iter().fold(0, |common, value| gcd(common, *value)).max(1);
    quotients.iter_mut().for_each(|value| *value /= divisor);
    put_int(out, base);
    put_count(out, divisor);
    let width = 64 - quotients.iter().max().copied().unwrap_or(0).leading_zeros();
    let (k, bits) = RiceCost::of(&quotients).cheapest();
    let coded = bits.div_ceil(8) as usize;
    // a rice code says its length first, which a width does not need
    if coded + count_len(coded) < packed_len(quotients.len(), width) {
        let mut writer = BitWriter::onto(Vec::with_capacity(coded));
        quotients.iter().for_each(|value| write_rice(&mut writer, *value, k));
        let code = writer.finish();
        out.extend_from_slice(&[RICE, k as u8]);
        put_count(out, code.len() as u64);
        out.extend_from_slice(&code);
    } else {
        out.extend_from_slice(&[WIDTH, width as u8]);
        pack(out, quotients.into_iter(), width);
    }
}

/// `count` integers as `put_reduced` wrote them.
fn read_reduced(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    let base = reader.int();
    let divisor = reader.count(usize::MAX) as u64;
    let (packer, parameter) = (reader.byte(), u32::from(reader.byte()));
    let quotients = match packer {
        WIDTH if parameter <= 64 => {
            let packed = reader.take(packed_len(count, parameter));
            if packed.len() != packed_len(count, parameter) {
                return Vec::new();
            }
            unpack(packed, count, parameter)
        }
        RICE if parameter < 64 => {
            let mut code = BitReader::of(reader.counted(usize::MAX >> 1));
            let quotients = (0..count).map(|_| read_rice(&mut code, parameter)).collect();
            if code.short() {
                reader.fail("an integer column's rice code ends inside a value");
            }
            quotients
        }
        _ => {
            reader.fail("an integer column's packer");
            return Vec::new();
        }
    };
    quotients.into_iter().map(|quotient| base.wrapping_add(quotient.wrapping_mul(divisor) as i64)).collect()
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The bytes a count takes.
fn count_len(count: usize) -> usize {
    let mut written = Vec::new();
    put_count(&mut written, count as u64);
    written.len()
}

/// The steady step a column's values rise or fall by, when it has one: the
/// slope of the line least far from them, rounded.
fn trend_step(values: &[i64]) -> Option<i64> {
    let count = values.len() as f64;
    let from_first = |value: &i64| value.wrapping_sub(values[0]) as f64;
    let mean_place = (count - 1.0) / 2.0;
    let mean_value: f64 = values.iter().map(|value| from_first(value) / count).sum();
    let (mut covariance, mut variance) = (0.0, 0.0);
    for (place, value) in values.iter().enumerate() {
        let off = place as f64 - mean_place;
        covariance += off * (from_first(value) - mean_value);
        variance += off * off;
    }
    let step = (covariance / variance).round();
    (step.is_finite() && step != 0.0 && step.abs() <= 2_f64.powi(62)).then_some(step as i64)
}

/// Appends a column of integers; of none, nothing.
pub(super) fn put_ints(out: &mut Vec<u8>, values: &[i64]) {
    let start = out.len();
    put_direct(out, values);
    let Some(counts) = (values.len() >= COUNTED_VALUES).then(|| counted(values)).flatten() else { return };
    for other in [dictionary(values, &counts), dominant(values, &counts)].into_iter().flatten() {
        if other.len() < out.len() - start {
            out.truncate(start);
            out.extend_from_slice(&other);
        }
    }
}

/// Appends a column of integers written directly, the one way a column
/// nested in another is; of none, nothing.
pub(super) fn put_direct(out: &mut Vec<u8>, values: &[i64]) {
    let Some(first) = values.first().copied() else { return };
    let mut smallest = vec![VALUES];
    put_reduced(&mut smallest, values.iter().copied());
    let mut consider = |written: Vec<u8>| {
        if written.len() < smallest.len() {
            smallest = written;
        }
    };
    if values.len() > 1 {
        let mut steps = vec![STEPS];
        put_int(&mut steps, first);
        put_reduced(&mut steps, values.windows(2).map(|pair| pair[1].wrapping_sub(pair[0])));
        consider(steps);
    }
    if let Some(step) = (values.len() >= TREND_VALUES).then(|| trend_step(values)).flatten() {
        let mut trend = vec![TREND];
        put_int(&mut trend, first);
        put_int(&mut trend, step);
        let on_the_line = |place: usize| first.wrapping_add(step.wrapping_mul(place as i64));
        put_reduced(&mut trend, values.iter().enumerate().map(|(place, value)| value.wrapping_sub(on_the_line(place))));
        consider(trend);
    }
    out.extend_from_slice(&smallest);
}

/// How many times each value stands, while they are few enough for a
/// dictionary.
fn counted(values: &[i64]) -> Option<HashMap<i64, usize>> {
    let mut counts = HashMap::new();
    for value in values {
        *counts.entry(*value).or_insert(0) += 1;
        if counts.len() > MOST_DISTINCT {
            return None;
        }
    }
    Some(counts)
}

/// The distinct values once, sorted, and which of them each value is, when
/// a quarter of the values or fewer are distinct.
fn dictionary(values: &[i64], counts: &HashMap<i64, usize>) -> Option<Vec<u8>> {
    if counts.len() * 4 > values.len() {
        return None;
    }
    let mut words: Vec<i64> = counts.keys().copied().collect();
    words.sort_unstable();
    let ids: Vec<i64> = values.iter().map(|value| words.binary_search(value).unwrap_or(0) as i64).collect();
    let mut out = vec![DICTIONARY];
    put_count(&mut out, words.len() as u64);
    put_direct(&mut out, &words);
    put_direct(&mut out, &ids);
    Some(out)
}

/// The value seven in eight share, the smallest of a tie, once, and where
/// the others stand with what they are.
fn dominant(values: &[i64], counts: &HashMap<i64, usize>) -> Option<Vec<u8>> {
    let most = |(value, count): (&i64, &usize)| (*count, std::cmp::Reverse(*value));
    let (frequency, std::cmp::Reverse(dominant)) = counts.iter().map(most).max()?;
    if frequency * 8 < values.len() * 7 || frequency == values.len() {
        return None;
    }
    let others = values.iter().enumerate().filter(|(_, value)| **value != dominant);
    let (places, others): (Vec<i64>, Vec<i64>) = others.map(|(place, value)| (place as i64, *value)).unzip();
    let mut out = vec![DOMINANT];
    put_int(&mut out, dominant);
    put_count(&mut out, places.len() as u64);
    put_direct(&mut out, &places);
    put_direct(&mut out, &others);
    Some(out)
}

/// A column of `count` integers; of none, without reading anything.
pub(super) fn read_ints(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    if count == 0 {
        return Vec::new();
    }
    match reader.byte() {
        DICTIONARY => read_dictionary(reader, count),
        DOMINANT => read_dominant(reader, count),
        kind => read_written(reader, kind, count),
    }
}

/// A column of `count` integers `put_direct` wrote; of none, without reading
/// anything.
pub(super) fn read_direct(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    if count == 0 {
        return Vec::new();
    }
    let kind = reader.byte();
    read_written(reader, kind, count)
}

/// The integers of a column written directly, past its first byte.
fn read_written(reader: &mut Reader<'_>, kind: u8, count: usize) -> Vec<i64> {
    match kind {
        VALUES => read_reduced(reader, count),
        STEPS => {
            let mut value = reader.int();
            let steps = read_reduced(reader, count - 1);
            let mut values = vec![value];
            for step in steps {
                value = value.wrapping_add(step);
                values.push(value);
            }
            values
        }
        TREND => {
            let (first, step) = (reader.int(), reader.int());
            let off = read_reduced(reader, count);
            let on_the_line = |place: usize| first.wrapping_add(step.wrapping_mul(place as i64));
            off.into_iter().enumerate().map(|(place, off)| on_the_line(place).wrapping_add(off)).collect()
        }
        _ => {
            reader.fail("an integer column's kind");
            Vec::new()
        }
    }
}

fn read_dictionary(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    let distinct = reader.count(MOST_DISTINCT.min(count));
    let words = read_direct(reader, distinct);
    let ids = read_direct(reader, count);
    let named = ids.iter().map(|id| usize::try_from(*id).ok().and_then(|id| words.get(id)).copied());
    named.collect::<Option<Vec<i64>>>().unwrap_or_else(|| {
        reader.fail("an integer column's id past its dictionary");
        Vec::new()
    })
}

fn read_dominant(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    let dominant = reader.int();
    let apart = reader.count(count);
    let (places, others) = (read_direct(reader, apart), read_direct(reader, apart));
    let mut values = vec![dominant; count];
    // each place past the one before and inside the column, so none is set twice
    let mut free = 0;
    for (place, other) in places.iter().zip(others) {
        let Some(place) = usize::try_from(*place).ok().filter(|place| (free..count).contains(place)) else {
            reader.fail("an integer column's other value out of its place");
            return Vec::new();
        };
        (values[place], free) = (other, place + 1);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        put_ints(&mut out, values);
        let mut reader = Reader::new(&out);
        assert_eq!(read_ints(&mut reader, values.len()), values);
        reader.finish().unwrap();
        out
    }

    fn written_directly(values: &[i64]) -> Vec<u8> {
        let mut out = Vec::new();
        put_direct(&mut out, values);
        let mut reader = Reader::new(&out);
        assert_eq!(read_direct(&mut reader, values.len()), values);
        reader.finish().unwrap();
        out
    }

    /// Numbers that differ from one place to the next, the same every run.
    fn scattered(count: usize, spread: u32) -> Vec<i64> {
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let next = |_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> (64 - spread)) as i64
        };
        (0..count).map(next).collect()
    }

    #[test]
    fn any_integers_come_back() {
        written(&[]);
        written(&[7]);
        written(&[i64::MIN, i64::MAX, 0, -1, 1]);
        written(&[i64::MAX, i64::MIN]);
        written(&[5, 5, 5, 5]);
        written(&[-3, 9, -27, 81, -243]);
        for spread in [1, 7, 20, 40, 63, 64] {
            written(&scattered(100, spread));
            let mut sorted = scattered(100, spread);
            sorted.sort_unstable();
            written(&sorted);
        }
        let extremes: Vec<i64> = (0..40).map(|n| if n % 2 == 0 { i64::MIN + n } else { i64::MAX - n }).collect();
        written(&extremes);
        let mostly: Vec<i64> = (0..64).map(|n| if n % 9 == 4 { i64::MIN } else { i64::MAX }).collect();
        written(&mostly);
    }

    #[test]
    fn times_of_a_millisecond_clock_take_a_few_bits_each() {
        let start = 1_800_000_000_000_000_000_i64;
        let times: Vec<i64> = (0..1024).map(|n| start + (n * 7 % 5 + n * 3) * 1_000_000).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        // steps of 0 to 7 ms: three bits each, where a time whole takes sixty-one
        assert!(written(&sorted).len() < 1024 * 3 / 8 + 32, "{}", written(&sorted).len());
    }

    #[test]
    fn a_column_of_one_value_takes_no_bit_a_value() {
        assert!(written(&[42; 1024]).len() <= 5);
        assert!(written(&(0..1024).collect::<Vec<i64>>()).len() <= 7);
    }

    #[test]
    fn small_values_among_a_few_wide_ones_take_a_rice_code() {
        let mut gaps = vec![0_i64; 1000];
        for (place, wide) in [(17, 40_000_001), (230, 39_999_999), (555, 40_000_003), (931, 39_999_991)] {
            gaps[place] = wide;
        }
        // a bit a zero and an escape a wide one, where one width for all is twenty-six bits each
        let column = written_directly(&gaps);
        assert!(column.len() < 1000 / 8 + 4 * 12 + 16, "{}", column.len());
        assert_eq!(column[..4], [VALUES, 0, 1, RICE]);
    }

    #[test]
    fn values_near_a_steady_step_are_kept_as_how_far_off_it_they_lie() {
        let off = scattered(1024, 3);
        let steady: Vec<i64> = (0..1024).map(|place| 1_800_000_000 + place as i64 * 1000 + off[place]).collect();
        assert_eq!(trend_step(&steady), Some(1000));
        assert_eq!(trend_step(&[5; 32]), None);
        let column = written(&steady);
        assert_eq!(column[0], TREND);
        // three bits off the line each, where a step to the next takes four
        assert!(column.len() <= 1024 * 3 / 8 + 16, "{}", column.len());
    }

    #[test]
    fn a_few_distinct_values_take_a_dictionary() {
        let statuses = [200, 404, 500, 302, 201, 301, 403, 503];
        let column: Vec<i64> = scattered(1024, 3).iter().map(|pick| statuses[*pick as usize]).collect();
        // three bits an id, where 200 to 503 less 200 take nine
        let dictionary = written(&column);
        assert_eq!(dictionary[0], DICTIONARY);
        assert!(dictionary.len() <= 1024 * 3 / 8 + 32, "{}", dictionary.len());
        let distinct: Vec<i64> = (0..64).map(|n| n * n * 1000).collect();
        assert_ne!(written(&distinct)[0], DICTIONARY);
    }

    #[test]
    fn the_value_seven_in_eight_share_is_kept_once_and_the_others_apart() {
        let mut column = vec![200_i64; 1024];
        for (place, other) in scattered(40, 10).iter().enumerate() {
            column[place * 25 + 3] = 1000 + other;
        }
        let dominant = written(&column);
        assert_eq!(dominant[0], DOMINANT);
        // forty places and forty values, where a dictionary's ids alone take six bits each
        assert!(dominant.len() <= 40 * 3 + 24, "{}", dominant.len());
        // one in eight apart is too many
        let mut fewer = vec![200_i64; 64];
        fewer[..9].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_ne!(written(&fewer)[0], DOMINANT);
    }

    #[test]
    fn a_column_cut_short_or_of_a_kind_nobody_wrote_is_said() {
        let mut gaps = vec![0_i64; 64];
        gaps[9] = 1_000_001;
        gaps[40] = 999_999;
        let statuses: Vec<i64> = (0..64).map(|n| [200, 404, 500][n % 3]).collect();
        for values in [vec![1, 2, 4, 8, 16, 32, 64, 128, 256], gaps, scattered(40, 30), statuses] {
            let column = written(&values);
            for cut in 0..column.len() {
                let mut reader = Reader::new(&column[..cut]);
                read_ints(&mut reader, values.len());
                assert!(reader.finish().is_err(), "cut at {cut} of {}", column.len());
            }
        }
        for unknown in
            [&[9, 0, 1, 0][..], &[VALUES, 0, 1, 7, 0], &[VALUES, 0, 1, WIDTH, 65], &[VALUES, 0, 1, RICE, 64, 0]]
        {
            let mut reader = Reader::new(unknown);
            read_ints(&mut reader, 1);
            assert!(reader.finish().is_err(), "{unknown:?}");
        }
    }

    #[test]
    fn a_dictionary_or_a_dominant_value_read_otherwise_than_written_is_refused() {
        let refused = |column: &[u8], count: usize| {
            let mut reader = Reader::new(column);
            read_ints(&mut reader, count);
            assert!(reader.finish().is_err(), "{column:?}");
        };
        let mut nested = vec![DICTIONARY, 1];
        put_direct(&mut nested, &[7]);
        put_direct(&mut nested, &[0; 8]);
        // a dictionary inside a dictionary: a nested column is always direct
        let mut twice = vec![DICTIONARY, 1];
        twice.extend_from_slice(&nested);
        put_direct(&mut twice, &[0; 8]);
        refused(&twice, 8);
        let mut past = vec![DICTIONARY, 1];
        put_direct(&mut past, &[7]);
        put_direct(&mut past, &[0, 0, 1, 0, 0, 0, 0, 0]);
        refused(&past, 8);
        for places in [[5, 3], [5, 8], [2, 2], [-1, 4]] {
            let mut placed = vec![DOMINANT, 0, 2];
            put_direct(&mut placed, &places);
            put_direct(&mut placed, &[1, 2]);
            refused(&placed, 8);
        }
    }
}
