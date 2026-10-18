//! Exact sums: every finite float is a whole number of units of 2⁻¹⁰⁷⁴, the
//! least subnormal, so a sum of them is an integer, kept in a fixed
//! accumulator of 35 words and rounded to a float once, rather than a big
//! integer made of each sample.
//!
//! ```text
//! 0.1 + 0.2       as floats 0.30000000000000004
//!                 exactly   0.3000000000000000166533453693773481063544750213623046875
//!                           rounded once: 0.30000000000000004
//! 1e308 + 1e308 − 1e308     as floats inf, exactly 1e308
//! ```
//!
//! The largest float is under 2¹⁰²⁴, 2²⁰⁹⁸ units, so 35 words of 64 bits hold
//! a sum of 2⁶³ of them, times the thousand a rate multiplies it by, with
//! room to spare.

use std::cmp::Ordering;

use crate::compress::varint::{Reader, put_count};

const WORDS: usize = 35;
/// Where the units' point lies: a unit is 2⁻¹⁰⁷⁴, so 1 is 2¹⁰⁷⁴ of them.
const POINT: u32 = 1074;

/// The bits a block's exact sum of at most 240 finite floats needs: 240 is
/// under 2⁸, and a float under 2²⁰⁹⁸ units.
pub(super) const BLOCK_BITS: u32 = 2106;

/// An integer of units of 2⁻¹⁰⁷⁴, in two's complement, its lowest word first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Exact([u64; WORDS]);

impl Default for Exact {
    fn default() -> Self {
        Exact::ZERO
    }
}

impl std::fmt::Debug for Exact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Exact({})", self.round())
    }
}

impl Exact {
    pub(super) const ZERO: Exact = Exact([0; WORDS]);

    /// A finite float's units; none for an infinity or a NaN.
    #[cfg(test)]
    pub(super) fn of(value: f64) -> Option<Exact> {
        let mut exact = Exact::ZERO;
        exact.add_float(value).then_some(exact)
    }

    /// Adds a float's units, or says it has none.
    pub(super) fn add_float(&mut self, value: f64) -> bool {
        let Some((mantissa, shift)) = units(value) else { return false };
        if value.is_sign_negative() {
            self.sub_shifted(mantissa, shift)
        } else {
            self.add_shifted(mantissa, shift)
        }
        true
    }

    /// Takes a float's units away, or says it has none.
    pub(super) fn sub_float(&mut self, value: f64) -> bool {
        self.add_float(-value)
    }

    /// A count's units: `count` × 2¹⁰⁷⁴.
    pub(super) fn of_whole(count: u64) -> Exact {
        let mut exact = Exact::ZERO;
        exact.add_shifted(count, POINT);
        exact
    }

    /// The whole number it is, when it is one from 0 to 2⁶⁴ − 1: a count.
    pub(super) fn whole(&self) -> Option<u64> {
        if self.is_negative() || low_bits_set(&self.0, POINT) {
            return None;
        }
        let whole = shift_right(&self.0, POINT);
        whole[1..].iter().all(|word| *word == 0).then_some(whole[0])
    }

    /// It times `factor`, for a count that leaves the room: a count of 2⁶⁴
    /// times 10¹⁷ is under 2¹²¹ units past the point, far below the sign.
    pub(super) fn times(&self, factor: u64) -> Exact {
        let mut words = self.magnitude();
        multiply(&mut words, factor);
        let product = Exact(words);
        debug_assert!(!product.is_negative(), "a product past the accumulator");
        if self.is_negative() {
            let mut negative = Exact::ZERO;
            negative.sub(&product);
            return negative;
        }
        product
    }

    pub(super) fn add(&mut self, other: &Exact) {
        let mut carry = false;
        for (word, more) in self.0.iter_mut().zip(other.0) {
            let (sum, over) = word.overflowing_add(more);
            let (sum, again) = sum.overflowing_add(u64::from(carry));
            (*word, carry) = (sum, over || again);
        }
    }

    pub(super) fn sub(&mut self, other: &Exact) {
        let mut borrow = false;
        for (word, less) in self.0.iter_mut().zip(other.0) {
            let (difference, under) = word.overflowing_sub(less);
            let (difference, again) = difference.overflowing_sub(u64::from(borrow));
            (*word, borrow) = (difference, under || again);
        }
    }

    /// `mantissa << shift` added: a mantissa of 64 bits at most, which
    /// shifted by under a word fits the 128 bits it is carried in.
    fn add_shifted(&mut self, mantissa: u64, shift: u32) {
        let (word, bits) = ((shift / 64) as usize, shift % 64);
        let wide = u128::from(mantissa) << bits;
        let (low, over) = self.0[word].overflowing_add(wide as u64);
        self.0[word] = low;
        let (high, mut carry) = self.0[word + 1].overflowing_add((wide >> 64) as u64);
        let (high, again) = high.overflowing_add(u64::from(over));
        self.0[word + 1] = high;
        carry |= again;
        for next in &mut self.0[word + 2..] {
            if !carry {
                break;
            }
            (*next, carry) = next.overflowing_add(1);
        }
    }

    fn sub_shifted(&mut self, mantissa: u64, shift: u32) {
        let (word, bits) = ((shift / 64) as usize, shift % 64);
        let wide = u128::from(mantissa) << bits;
        let (low, under) = self.0[word].overflowing_sub(wide as u64);
        self.0[word] = low;
        let (high, mut borrow) = self.0[word + 1].overflowing_sub((wide >> 64) as u64);
        let (high, again) = high.overflowing_sub(u64::from(under));
        self.0[word + 1] = high;
        borrow |= again;
        for next in &mut self.0[word + 2..] {
            if !borrow {
                break;
            }
            (*next, borrow) = next.overflowing_sub(1);
        }
    }

    pub(super) fn is_zero(&self) -> bool {
        self.0.iter().all(|word| *word == 0)
    }

    pub(super) fn is_negative(&self) -> bool {
        self.0[WORDS - 1] >> 63 == 1
    }

    /// Its absolute value, as words.
    fn magnitude(&self) -> [u64; WORDS] {
        let mut magnitude = *self;
        if self.is_negative() {
            magnitude = Exact::ZERO;
            magnitude.sub(self);
        }
        magnitude.0
    }

    /// The float nearest it, a tie to the even one: an infinity past the
    /// largest float.
    pub(super) fn round(&self) -> f64 {
        signed(self.is_negative(), to_float(&self.magnitude(), Remainder::None))
    }

    /// It divided by `divisor` and rounded once: a mean.
    pub(super) fn divided(&self, divisor: u64) -> f64 {
        self.scaled(1, divisor)
    }

    /// It times `factor` over `divisor`, rounded once: a rate, a thousand
    /// over a bucket's milliseconds.
    pub(super) fn scaled(&self, factor: u64, divisor: u64) -> f64 {
        if divisor == 0 {
            return f64::NAN;
        }
        let mut words = self.magnitude();
        multiply(&mut words, factor);
        let remainder = divide(&mut words, divisor);
        let rest = match (2 * u128::from(remainder)).cmp(&u128::from(divisor)) {
            _ if remainder == 0 => Remainder::None,
            Ordering::Less => Remainder::BelowHalf,
            Ordering::Equal => Remainder::Half,
            Ordering::Greater => Remainder::AboveHalf,
        };
        signed(self.is_negative(), to_float(&words, rest))
    }

    /// Appends it canonically: its magnitude's bytes after its trailing zero
    /// bits are taken out, behind their count and a token of how many were
    /// taken and its sign; zero is the count 0 alone.
    pub(super) fn put(&self, out: &mut Vec<u8>) {
        let magnitude = self.magnitude();
        let Some(low) = magnitude.iter().position(|word| *word != 0) else { return out.push(0) };
        let zeros = low as u32 * 64 + magnitude[low].trailing_zeros();
        let shifted = shift_right(&magnitude, zeros);
        let bits = bit_length(&shifted);
        let bytes = bits.div_ceil(8) as usize;
        put_count(out, bytes as u64);
        put_count(out, (u64::from(zeros) << 1) | u64::from(self.is_negative()));
        out.extend((0..bytes).rev().map(|place| (shifted[place / 8] >> (place % 8 * 8)) as u8));
    }

    /// Reads what [`Exact::put`] wrote, of at most `most_bits`, refusing any
    /// other spelling of the same number.
    pub(super) fn read(reader: &mut Reader<'_>, most_bits: u32) -> Exact {
        let bytes = reader.count(most_bits.div_ceil(8) as usize);
        if bytes == 0 {
            return Exact::ZERO;
        }
        let token = reader.uvarint();
        let zeros = token >> 1;
        let magnitude = reader.take(bytes);
        if magnitude.len() != bytes || zeros >= u64::from(most_bits) {
            reader.fail("an exact sum past its bound");
            return Exact::ZERO;
        }
        if magnitude[0] == 0 || magnitude[bytes - 1] & 1 == 0 {
            reader.fail("an exact sum not written as its one spelling");
            return Exact::ZERO;
        }
        let mut words = [0_u64; WORDS];
        for (place, byte) in magnitude.iter().rev().enumerate() {
            words[place / 8] |= u64::from(*byte) << (place % 8 * 8);
        }
        let bits = bit_length(&words) + zeros as u32;
        if bits > most_bits {
            reader.fail("an exact sum past its bound");
            return Exact::ZERO;
        }
        let mut exact = Exact(shift_left(&words, zeros as u32));
        if token & 1 == 1 {
            let positive = exact;
            exact = Exact::ZERO;
            exact.sub(&positive);
        }
        exact
    }
}

impl PartialOrd for Exact {
    fn partial_cmp(&self, other: &Exact) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Exact {
    fn cmp(&self, other: &Exact) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => self.0.iter().rev().cmp(other.0.iter().rev()),
        }
    }
}

/// A finite float's mantissa and where it stands in units: a normal float's
/// 53 bits shifted by its exponent less one, a subnormal's as they are.
fn units(value: f64) -> Option<(u64, u32)> {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as u32;
    let fraction = bits & ((1 << 52) - 1);
    match exponent {
        0x7ff => None,
        0 => Some((fraction, 0)),
        _ => Some((fraction | (1 << 52), exponent - 1)),
    }
}

/// What a quotient left below its last whole unit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Remainder {
    None,
    BelowHalf,
    Half,
    AboveHalf,
}

/// The magnitude `words` units, and a fraction of a unit `rest`, as the
/// nearest float, a tie to the even one.
///
/// A positive float's bits read as an integer grow with it, so its bits are
/// `shift << 52` plus its 53-bit mantissa: a mantissa that rounds up to 2⁵³
/// carries into the exponent by itself, and one past the largest exponent is
/// an infinity.
fn to_float(words: &[u64; WORDS], rest: Remainder) -> f64 {
    let length = bit_length(words);
    let shift = length.saturating_sub(53);
    let mantissa = shift_right(words, shift)[0];
    let up = if shift == 0 {
        match rest {
            Remainder::AboveHalf => true,
            Remainder::Half => mantissa & 1 == 1,
            Remainder::None | Remainder::BelowHalf => false,
        }
    } else {
        let dropped_high = bit(words, shift - 1);
        let dropped_low = low_bits_set(words, shift - 1) || rest != Remainder::None;
        dropped_high && (dropped_low || mantissa & 1 == 1)
    };
    let bits = (u64::from(shift) << 52) + mantissa + u64::from(up);
    if bits >= f64::INFINITY.to_bits() { f64::INFINITY } else { f64::from_bits(bits) }
}

fn signed(negative: bool, magnitude: f64) -> f64 {
    if negative { -magnitude } else { magnitude }
}

fn bit_length(words: &[u64; WORDS]) -> u32 {
    words.iter().rposition(|word| *word != 0).map_or(0, |top| top as u32 * 64 + 64 - words[top].leading_zeros())
}

fn bit(words: &[u64; WORDS], at: u32) -> bool {
    words[(at / 64) as usize] >> (at % 64) & 1 == 1
}

/// Whether any bit below `below` is set.
fn low_bits_set(words: &[u64; WORDS], below: u32) -> bool {
    let (whole, part) = ((below / 64) as usize, below % 64);
    words[..whole].iter().any(|word| *word != 0) || (part > 0 && words[whole] & ((1 << part) - 1) != 0)
}

fn shift_right(words: &[u64; WORDS], by: u32) -> [u64; WORDS] {
    let (whole, part) = ((by / 64) as usize, by % 64);
    let mut out = [0_u64; WORDS];
    for place in 0..WORDS.saturating_sub(whole) {
        let low = words[place + whole] >> part;
        let high = if part > 0 && place + whole + 1 < WORDS { words[place + whole + 1] << (64 - part) } else { 0 };
        out[place] = low | high;
    }
    out
}

fn shift_left(words: &[u64; WORDS], by: u32) -> [u64; WORDS] {
    let (whole, part) = ((by / 64) as usize, by % 64);
    let mut out = [0_u64; WORDS];
    for place in whole..WORDS {
        let high = words[place - whole] << part;
        let low = if part > 0 && place > whole { words[place - whole - 1] >> (64 - part) } else { 0 };
        out[place] = high | low;
    }
    out
}

fn multiply(words: &mut [u64; WORDS], factor: u64) {
    let mut carry = 0_u128;
    for word in words.iter_mut() {
        let product = u128::from(*word) * u128::from(factor) + carry;
        *word = product as u64;
        carry = product >> 64;
    }
}

/// Divides in place and gives the remainder.
fn divide(words: &mut [u64; WORDS], divisor: u64) -> u64 {
    let mut remainder = 0_u128;
    for word in words.iter_mut().rev() {
        let dividend = (remainder << 64) | u128::from(*word);
        *word = (dividend / u128::from(divisor)) as u64;
        remainder = dividend % u128::from(divisor);
    }
    remainder as u64
}

#[cfg(test)]
#[path = "exact_tests.rs"]
mod tests;
