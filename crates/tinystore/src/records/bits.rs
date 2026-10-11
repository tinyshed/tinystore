//! Bits: integers of one width packed one after another, the lowest bit
//! first, and rice codes, which give a small integer few bits and any integer
//! some.

/// The bytes `count` values of `width` bits take.
pub(super) fn packed_len(count: usize, width: u32) -> usize {
    (count * width as usize).div_ceil(8)
}

/// Appends `values`, each in `width` bits; a value wider than that loses its
/// high bits, which a caller's width never lets happen.
pub(super) fn pack(out: &mut Vec<u8>, values: impl Iterator<Item = u64>, width: u32) {
    let mut writer = BitWriter::onto(std::mem::take(out));
    values.for_each(|value| writer.write(value, width));
    *out = writer.finish();
}

/// `count` values of `width` bits, 64 at most, from `data`, which holds the
/// `packed_len` of them.
pub(super) fn unpack(data: &[u8], count: usize, width: u32) -> Vec<u64> {
    let mut reader = BitReader::of(data);
    (0..count).map(|_| reader.read(width)).collect()
}

/// A writer of bits onto bytes, the lowest bit of a byte first.
pub(super) struct BitWriter {
    out: Vec<u8>,
    held: u128,
    bits: u32,
}

impl BitWriter {
    pub(super) fn onto(out: Vec<u8>) -> BitWriter {
        BitWriter { out, held: 0, bits: 0 }
    }

    /// Appends the low `width` bits of `value`, 64 at most.
    pub(super) fn write(&mut self, value: u64, width: u32) {
        let low = if width >= 64 { value } else { value & ((1_u64 << width) - 1) };
        self.held |= u128::from(low) << self.bits;
        self.bits += width;
        while self.bits >= 8 {
            self.out.push(self.held as u8);
            self.held >>= 8;
            self.bits -= 8;
        }
    }

    /// The bytes written, the last one's unused bits zero.
    pub(super) fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.out.push(self.held as u8);
        }
        self.out
    }
}

/// A reader of the bits a writer wrote. Past the end it reads zeros and
/// says so in `short`.
pub(super) struct BitReader<'a> {
    bytes: std::slice::Iter<'a, u8>,
    held: u128,
    bits: u32,
    pub(super) short: bool,
}

impl<'a> BitReader<'a> {
    pub(super) fn of(data: &'a [u8]) -> BitReader<'a> {
        BitReader { bytes: data.iter(), held: 0, bits: 0, short: false }
    }

    /// The next `width` bits, 64 at most.
    pub(super) fn read(&mut self, width: u32) -> u64 {
        while self.bits < width {
            let Some(byte) = self.bytes.next() else {
                self.short = true;
                self.bits = width;
                break;
            };
            self.held |= u128::from(*byte) << self.bits;
            self.bits += 8;
        }
        let value = if width >= 64 { self.held as u64 } else { self.held as u64 & ((1_u64 << width) - 1) };
        self.held >>= width;
        self.bits -= width;
        value
    }
}

/// A quotient this large or larger is written as the escape and 64 raw bits.
const RICE_ESCAPE: u32 = 32;

/// Appends the rice code of `value` at parameter `k`, 63 at most: `value >>
/// k` in unary and the low `k` bits as they are.
///
/// ```text
/// k=2   5 → 1 0 01     0 → 0 00     13 → 111 0 01
/// ```
pub(super) fn write_rice(writer: &mut BitWriter, value: u64, k: u32) {
    let quotient = value >> k;
    if quotient < u64::from(RICE_ESCAPE) {
        writer.write((1_u64 << quotient) - 1, quotient as u32);
        writer.write(0, 1);
        writer.write(value, k);
    } else {
        writer.write(u64::MAX, RICE_ESCAPE);
        writer.write(value, 64);
    }
}

pub(super) fn read_rice(reader: &mut BitReader<'_>, k: u32) -> u64 {
    let mut quotient = 0;
    while quotient < RICE_ESCAPE && reader.read(1) == 1 {
        quotient += 1;
    }
    if quotient == RICE_ESCAPE { reader.read(64) } else { u64::from(quotient) << k | reader.read(k) }
}

/// Integers counted by their length in bits, with the sums that price a
/// rice code at every parameter exactly, in one pass over them.
pub(super) struct RiceCost {
    counts: [u64; 65],
    /// By a length and how many bits above a parameter it stands, one to
    /// five: the sum of the values of that length shifted down to that many
    /// bits, which is the sum of their quotients.
    quotients: [[u64; 6]; 65],
    longest: usize,
}

impl RiceCost {
    pub(super) fn of(values: &[u64]) -> RiceCost {
        let mut cost = RiceCost { counts: [0; 65], quotients: [[0; 6]; 65], longest: 0 };
        for value in values {
            let length = (64 - value.leading_zeros()) as usize;
            cost.counts[length] += 1;
            cost.longest = cost.longest.max(length);
            for above in 1..=length.min(5) {
                cost.quotients[length][above] += value >> (length - above);
            }
        }
        cost
    }

    /// The bits the values take at parameter `k`.
    fn bits(&self, k: usize) -> u64 {
        let of_length = |length: usize| match self.counts[length] {
            0 => 0,
            count if length <= k => count * (1 + k as u64),
            count if length <= k + 5 => self.quotients[length][length - k] + count * (1 + k as u64),
            count => count * u64::from(RICE_ESCAPE + 64),
        };
        (0..=self.longest).map(of_length).sum()
    }

    /// The cheapest parameter and the bits the values take at it. The mean
    /// is a poor guide, since a few wide values raise it for all:
    ///
    /// ```text
    /// 14 zeros and 1000000 twice   k 0: 14×1 + 2×96 = 206 bits
    ///                              k 16, near log2 of the mean: 14×17 + 2×(15+17) = 302 bits
    /// ```
    pub(super) fn cheapest(&self) -> (u32, u64) {
        // a parameter past the longest value only adds bits
        let tried = (0..=self.longest.min(63)).map(|k| (k as u32, self.bits(k)));
        tried.min_by_key(|(_, bits)| *bits).unwrap_or((0, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_of_every_width_come_back_in_the_bytes_their_bits_take() {
        for width in [0, 1, 3, 7, 8, 13, 31, 32, 63, 64] {
            let most = if width == 64 { u64::MAX } else { (1_u64 << width) - 1 };
            let values: Vec<u64> = (0..37_u64).map(|n| n.wrapping_mul(0x9e37_79b9_7f4a_7c15) & most).collect();
            let mut packed = Vec::new();
            pack(&mut packed, values.iter().copied(), width);
            assert_eq!(packed.len(), packed_len(values.len(), width), "width {width}");
            assert_eq!(unpack(&packed, values.len(), width), values, "width {width}");
        }
    }

    #[test]
    fn a_rice_code_gives_back_any_value_at_any_parameter_in_the_bits_its_cost_says() {
        let values: Vec<u64> = [0, 1, 5, 13, 31, 32, 1000, 1_000_000, u64::from(u32::MAX), u64::MAX, 1 << 63]
            .into_iter()
            .chain((0..40).map(|n: u64| n.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (n % 64)))
            .collect();
        let cost = RiceCost::of(&values);
        for k in 0..64 {
            let mut writer = BitWriter::onto(Vec::new());
            values.iter().for_each(|value| write_rice(&mut writer, *value, k));
            let coded = writer.finish();
            assert_eq!(coded.len() as u64, cost.bits(k as usize).div_ceil(8), "k {k}");
            let mut reader = BitReader::of(&coded);
            let read: Vec<u64> = (0..values.len()).map(|_| read_rice(&mut reader, k)).collect();
            assert!(!reader.short && read == values, "k {k}");
        }
    }

    #[test]
    fn the_cheapest_parameter_is_priced_and_not_guessed_from_the_mean() {
        let mut values = vec![0_u64; 14];
        values.extend([1_000_000, 1_000_000]);
        assert_eq!(RiceCost::of(&values).cheapest(), (0, 206));
        assert_eq!(RiceCost::of(&values).bits(16), 302);
        assert_eq!(RiceCost::of(&[]).cheapest(), (0, 0));
        let steady: Vec<u64> = (0..100).map(|n| 1000 + n % 7).collect();
        assert_eq!(RiceCost::of(&steady).cheapest().0, 9);
    }

    #[test]
    fn bits_read_past_their_end_are_zeros_and_said() {
        let mut reader = BitReader::of(&[0xff]);
        assert_eq!((reader.read(8), reader.short), (0xff, false));
        assert_eq!((reader.read(3), reader.short), (0, true));
        let mut reader = BitReader::of(&[0xff; 3]);
        read_rice(&mut reader, 0);
        assert!(reader.short);
    }
}
