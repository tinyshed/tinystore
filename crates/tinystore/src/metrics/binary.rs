//! What metrics adds to the bytes records keeps too: a float as the shortest
//! token that gives its bits back, and the distance between two times across
//! the whole range of an `i64`.

use crate::compress::varint::{Reader, fold, put_count, unfold};

/// `end − start` as an unsigned number, exact across the whole signed range.
pub(super) const fn distance(start: i64, end: i64) -> u64 {
    (end as u64).wrapping_sub(start as u64)
}

/// `start + span`, which the caller has checked stays an `i64`.
pub(super) const fn advance(start: i64, span: u64) -> i64 {
    (start as u64).wrapping_add(span) as i64
}

/// Appends a float as the shortest token that reads back as its bits: a whole
/// number, or a whole number of hundredths, as a varint; anything else as its
/// eight bytes behind a marker.
///
/// ```text
/// 7       → token 14·4         one byte
/// 0.25    → token 50·4 + 1     two bytes
/// -0, NaN → token 2, 8 bytes
/// ```
pub(super) fn put_float(out: &mut Vec<u8>, value: f64) {
    for (tag, factor) in [(0, 1.0), (1, 100.0)] {
        let product = (value * factor).round();
        if !product.is_finite() || product.abs() >= TOKEN_LIMIT {
            continue;
        }
        let integer = product as i64;
        if (integer as f64 / factor).to_bits() == value.to_bits() {
            return put_count(out, (fold(integer) << 2) | tag);
        }
    }
    out.push(RAW_FLOAT);
    out.extend_from_slice(&value.to_bits().to_le_bytes());
}

/// A float as [`put_float`] wrote it.
pub(super) fn read_float(reader: &mut Reader<'_>) -> f64 {
    let token = reader.uvarint();
    if token == u64::from(RAW_FLOAT) {
        return f64::from_bits(reader.word());
    }
    let integer = unfold(token >> 2) as f64;
    match token & 3 {
        0 => integer,
        1 => integer / 100.0,
        _ => {
            reader.fail("a float's token");
            0.0
        }
    }
}

/// A float token's integer stays under 2⁶⁰, so that folded and shifted it
/// still fits a word.
const TOKEN_LIMIT: f64 = (1_u64 << 60) as f64;
/// The token of a float kept as its eight bytes.
const RAW_FLOAT: u8 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float_takes_the_shortest_token_that_gives_its_bits_back() {
        let cases: [(f64, usize); 8] = [
            (7.0, 1),
            (-0.25, 2),
            (-17.5, 2),
            (1e300, 9),
            (-0.0, 9),
            (f64::NAN, 9),
            (f64::from_bits(0x7ff0_0000_0000_0001), 9),
            (f64::INFINITY, 9),
        ];
        for (value, length) in cases {
            let mut out = Vec::new();
            put_float(&mut out, value);
            assert_eq!(out.len(), length, "{value}");
            let mut reader = Reader::new(&out);
            assert_eq!(read_float(&mut reader).to_bits(), value.to_bits(), "{value}");
            reader.finish().unwrap();
        }
    }

    #[test]
    fn a_distance_spans_the_whole_range_of_times() {
        assert_eq!(distance(i64::MIN, i64::MAX), u64::MAX);
        assert_eq!(advance(i64::MIN, u64::MAX), i64::MAX);
        assert_eq!(distance(-5, 5), 10);
    }
}
