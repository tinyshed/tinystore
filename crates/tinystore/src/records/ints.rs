//! An integer column: its values, or the steps from each to the next,
//! whichever takes fewer bytes; either way less the smallest, divided by what
//! divides them all, each in the bits the largest needs.
//!
//! ```text
//! times, a millisecond's clock in ns    1000000000   1002000000   1005000000
//! the steps                                          2000000      3000000
//! less the smallest, 2000000                         0            1000000
//! divided by 1000000                                 0            1          a bit each
//! ```
//!
//! Every step and every difference wraps around as its inverse does, so any
//! sixty-four bits come back.

use super::bits::{pack, packed_len, unpack};
use super::bytes::{Reader, put_count, put_int};

const VALUES: u8 = 0;
const STEPS: u8 = 1;

/// Integers less their smallest, divided by what divides them all.
struct Reduced {
    base: i64,
    divisor: u64,
    width: u32,
    quotients: Vec<u64>,
}

impl Reduced {
    fn of(values: impl Iterator<Item = i64> + Clone) -> Reduced {
        let base = values.clone().min().unwrap_or(0);
        let mut quotients: Vec<u64> = values.map(|value| value.wrapping_sub(base) as u64).collect();
        let divisor = quotients.iter().fold(0, |common, value| gcd(common, *value)).max(1);
        quotients.iter_mut().for_each(|value| *value /= divisor);
        let width = 64 - quotients.iter().max().copied().unwrap_or(0).leading_zeros();
        Reduced { base, divisor, width, quotients }
    }

    /// The bytes it is written in.
    fn len(&self) -> usize {
        let mut head = Vec::new();
        self.put_head(&mut head);
        head.len() + packed_len(self.quotients.len(), self.width)
    }

    fn put_head(&self, out: &mut Vec<u8>) {
        put_int(out, self.base);
        put_count(out, self.divisor);
        out.push(self.width as u8);
    }

    fn put(&self, out: &mut Vec<u8>) {
        self.put_head(out);
        pack(out, self.quotients.iter().copied(), self.width);
    }

    /// `count` integers as `put` wrote them.
    fn read(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
        let base = reader.int();
        let divisor = reader.count(usize::MAX) as u64;
        let width = u32::from(reader.byte());
        if width > 64 {
            reader.fail("an integer column's width");
            return Vec::new();
        }
        let packed = reader.take(packed_len(count, width));
        if packed.len() != packed_len(count, width) {
            return Vec::new();
        }
        let quotients = unpack(packed, count, width);
        quotients.into_iter().map(|quotient| base.wrapping_add(quotient.wrapping_mul(divisor) as i64)).collect()
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Appends a column of integers; of none, nothing.
pub(super) fn put_ints(out: &mut Vec<u8>, values: &[i64]) {
    let Some(first) = values.first() else { return };
    let whole = Reduced::of(values.iter().copied());
    let steps = Reduced::of(values.windows(2).map(|pair| pair[1].wrapping_sub(pair[0])));
    let mut first_width = Vec::new();
    put_int(&mut first_width, *first);
    if values.len() > 1 && first_width.len() + steps.len() < whole.len() {
        out.push(STEPS);
        put_int(out, *first);
        steps.put(out);
    } else {
        out.push(VALUES);
        whole.put(out);
    }
}

/// A column of `count` integers; of none, without reading anything.
pub(super) fn read_ints(reader: &mut Reader<'_>, count: usize) -> Vec<i64> {
    if count == 0 {
        return Vec::new();
    }
    match reader.byte() {
        VALUES => Reduced::read(reader, count),
        STEPS => {
            let mut value = reader.int();
            let steps = Reduced::read(reader, count - 1);
            let mut values = vec![value];
            for step in steps {
                value = value.wrapping_add(step);
                values.push(value);
            }
            values
        }
        _ => {
            reader.fail("an integer column's kind");
            Vec::new()
        }
    }
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

    #[test]
    fn any_integers_come_back() {
        written(&[]);
        written(&[7]);
        written(&[i64::MIN, i64::MAX, 0, -1, 1]);
        written(&[i64::MAX, i64::MIN]);
        written(&[5, 5, 5, 5]);
        written(&[-3, 9, -27, 81, -243]);
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
        assert!(written(&[42; 1024]).len() <= 4);
        assert!(written(&(0..1024).collect::<Vec<i64>>()).len() <= 6);
    }

    #[test]
    fn a_column_cut_short_or_of_a_kind_nobody_wrote_is_said() {
        let out = written(&[1, 2, 4, 8, 16, 32, 64, 128, 256]);
        for cut in 0..out.len() {
            let mut reader = Reader::new(&out[..cut]);
            read_ints(&mut reader, 9);
            assert!(reader.finish().is_err(), "cut at {cut}");
        }
        let mut reader = Reader::new(&[9, 0, 1, 0]);
        read_ints(&mut reader, 1);
        assert_eq!(reader.finish().unwrap_err().to_string(), "an integer column's kind: corrupt");
    }
}
