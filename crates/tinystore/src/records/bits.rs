//! Integers of one width, packed bit after bit, the lowest first.

/// The bytes `count` values of `width` bits take.
pub(super) fn packed_len(count: usize, width: u32) -> usize {
    (count * width as usize).div_ceil(8)
}

/// Appends `values`, each in `width` bits; a value wider than that loses its
/// high bits, which a caller's width never lets happen.
pub(super) fn pack(out: &mut Vec<u8>, values: impl Iterator<Item = u64>, width: u32) {
    let (mut held, mut bits) = (0_u128, 0_u32);
    for value in values {
        held |= u128::from(value) << bits;
        bits += width;
        while bits >= 8 {
            out.push(held as u8);
            held >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push(held as u8);
    }
}

/// `count` values of `width` bits, 64 at most, from `data`, which holds the
/// `packed_len` of them.
pub(super) fn unpack(data: &[u8], count: usize, width: u32) -> Vec<u64> {
    let mask = if width >= 64 { u64::MAX } else { (1_u64 << width) - 1 };
    let (mut held, mut bits, mut bytes) = (0_u128, 0_u32, data.iter());
    let mut next = || {
        while bits < width {
            held |= u128::from(bytes.next().copied().unwrap_or(0)) << bits;
            bits += 8;
        }
        let value = held as u64 & mask;
        held >>= width;
        bits -= width;
        value
    };
    (0..count).map(|_| next()).collect()
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
}
