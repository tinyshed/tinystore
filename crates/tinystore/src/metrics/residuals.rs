//! What a decimal grid leaves of each value: the distance from its point on
//! the grid to its own bits, counted in floats between them, so that the bits
//! come back whole. Most are zero; the rest are kept the shortest of seven
//! ways.
//!
//! | Way        | Keeps                                                          |
//! |------------|----------------------------------------------------------------|
//! | zero       | nothing: every value is on the grid                            |
//! | signs      | a bitmap of those off it, and a bit each, when all are ±1      |
//! | bitmap     | a bitmap of those off it, and each one at one width            |
//! | packed     | every residual at one width                                    |
//! | sparse     | how far after the last each one off the grid is, and its value |
//! | varints    | every residual as a varint                                     |
//! | compressed | those varints through zstd                                     |

use super::codec::MOST_SAMPLES;
use crate::compress::bits::{pack, packed_len, unpack};
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, fold, put_count, put_int, unfold};
use crate::{Error, Result};

const ZERO: u8 = 0;
const SIGNS: u8 = 1;
const BITMAP: u8 = 2;
const PACKED: u8 = 3;
const SPARSE: u8 = 4;
const VARINTS: u8 = 5;
const COMPRESSED: u8 = 6;

/// What residuals expand to at most through zstd: a varint each.
const MOST_PLAIN: usize = 10 * MOST_SAMPLES;

/// The shortest way to keep `residuals`.
pub(super) fn encode(zstd: &mut Zstd, residuals: &[i64]) -> Result<Vec<u8>> {
    let folded: Vec<u64> = residuals.iter().map(|residual| fold(*residual)).collect();
    let off: Vec<usize> = (0..residuals.len()).filter(|index| residuals[*index] != 0).collect();
    if off.is_empty() {
        return Ok(vec![ZERO]);
    }
    let mut best = vec![VARINTS];
    residuals.iter().for_each(|residual| put_int(&mut best, *residual));
    let mut candidates = Vec::with_capacity(5);
    let squeezed = zstd.squeeze(&best[1..])?;
    if squeezed.len() < best.len() - 1 {
        candidates.push([&[COMPRESSED][..], &squeezed].concat());
    }
    let union = folded.iter().fold(0, |union, value| union | value);
    let width = 64 - union.leading_zeros();
    let mut packed = vec![PACKED, width as u8];
    pack(&mut packed, folded.iter().copied(), width);
    candidates.push(packed);
    let bitmap = bitmap_of(&off, residuals.len());
    let mut widths = [&[BITMAP, width as u8][..], &bitmap].concat();
    pack(&mut widths, off.iter().map(|index| folded[*index]), width);
    candidates.push(widths);
    if off.iter().all(|index| residuals[*index].abs() == 1) {
        let mut signs = [&[SIGNS][..], &bitmap].concat();
        pack(&mut signs, off.iter().map(|index| u64::from(residuals[*index] < 0)), 1);
        candidates.push(signs);
    }
    let mut sparse = vec![SPARSE];
    let mut previous = 0;
    for &index in &off {
        put_count(&mut sparse, (index + 1 - previous) as u64);
        put_count(&mut sparse, folded[index]);
        previous = index + 1;
    }
    candidates.push(sparse);
    Ok(candidates.into_iter().fold(best, |best, candidate| if candidate.len() < best.len() { candidate } else { best }))
}

fn bitmap_of(off: &[usize], count: usize) -> Vec<u8> {
    let mut bitmap = vec![0_u8; count.div_ceil(8)];
    off.iter().for_each(|index| bitmap[index / 8] |= 1 << (index % 8));
    bitmap
}

/// `count` residuals read back.
pub(super) fn decode(zstd: &mut Zstd, bytes: &[u8], count: usize) -> Result<Vec<i64>> {
    if count >= MOST_SAMPLES || bytes.is_empty() {
        return Err(Error::corrupt("a grid's residuals' bounds"));
    }
    let (mode, rest) = (bytes[0], &bytes[1..]);
    let expanded;
    let (mode, rest) = if mode == COMPRESSED {
        expanded = zstd.expand(rest, MOST_PLAIN)?;
        (VARINTS, &expanded[..])
    } else {
        (mode, rest)
    };
    let mut reader = Reader::new(rest);
    let residuals = match mode {
        ZERO => vec![0; count],
        VARINTS => (0..count).map(|_| reader.int()).collect(),
        SPARSE => sparse(&mut reader, count)?,
        PACKED => {
            let width = u32::from(reader.byte());
            packed(&mut reader, count, width)?.into_iter().map(unfold).collect()
        }
        SIGNS | BITMAP => from_bitmap(&mut reader, count, mode)?,
        _ => return Err(Error::corrupt("a grid's residuals' way")),
    };
    reader.finish().map_err(|error| error.within("a grid's residuals"))?;
    Ok(residuals)
}

fn sparse(reader: &mut Reader<'_>, count: usize) -> Result<Vec<i64>> {
    let mut residuals = vec![0; count];
    let mut position = 0;
    while reader.check().is_ok() && !reader.remaining().is_empty() {
        let gap = reader.count(count - position);
        if gap == 0 {
            return Err(Error::corrupt("a grid's residual does not move on"));
        }
        position += gap;
        residuals[position - 1] = unfold(reader.uvarint());
    }
    Ok(residuals)
}

/// `count` values of `width` bits, exactly the bytes they take and their
/// padding zero.
fn packed(reader: &mut Reader<'_>, count: usize, width: u32) -> Result<Vec<u64>> {
    if width > 64 {
        return Err(Error::corrupt("a grid's residuals' width"));
    }
    let bytes = reader.take(packed_len(count, width));
    reader.check()?;
    let used = (count * width as usize) % 8;
    if used != 0 && bytes[bytes.len() - 1] >> used != 0 {
        return Err(Error::corrupt("a grid's residuals' padding"));
    }
    Ok(unpack(bytes, count, width))
}

fn from_bitmap(reader: &mut Reader<'_>, count: usize, mode: u8) -> Result<Vec<i64>> {
    let width = if mode == BITMAP { u32::from(reader.byte()) } else { 1 };
    let bitmap = reader.take(count.div_ceil(8));
    reader.check()?;
    if !count.is_multiple_of(8) && bitmap[bitmap.len() - 1] >> (count % 8) != 0 {
        return Err(Error::corrupt("a grid's residuals' bitmap padding"));
    }
    let off = bitmap.iter().map(|byte| byte.count_ones() as usize).sum();
    let values = packed(reader, off, width)?;
    let mut values = values.into_iter();
    Ok((0..count)
        .map(|index| match bitmap[index / 8] >> (index % 8) & 1 {
            0 => 0,
            _ => match (mode, values.next().unwrap_or(0)) {
                (BITMAP, value) => unfold(value),
                (_, 1) => -1,
                _ => 1,
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zstd() -> Zstd {
        Zstd::windowed(3, 13)
    }

    /// A fixed sequence no period of which zstd could fold.
    fn scrambled(index: usize) -> usize {
        (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(23) as usize
    }

    #[test]
    fn residuals_read_back_whichever_way_is_shortest() {
        let cases: Vec<(Vec<i64>, u8)> = vec![
            (vec![0; 239], ZERO),
            ((0..239).map(|index| [0, 0, 1, -1][scrambled(index) % 4]).collect(), SIGNS),
            ((0..239).map(|index| [0, 0, 0, 1, -2, 3, -3][scrambled(index) % 7]).collect(), BITMAP),
            ((0..239).map(|index| (scrambled(index) % 7) as i64 - 3).collect(), PACKED),
            ((0..239).map(|index| if index == 7 { 1 << 40 } else { 0 }).collect(), SPARSE),
            ((0..239).map(|index| [1 << 50, -(1 << 33), 5][index % 3]).collect(), COMPRESSED),
        ];
        for (residuals, way) in cases {
            let encoded = encode(&mut zstd(), &residuals).unwrap();
            assert_eq!(encoded[0], way, "{residuals:?}");
            assert_eq!(decode(&mut zstd(), &encoded, residuals.len()).unwrap(), residuals);
        }
    }

    #[test]
    fn residuals_that_do_not_read_whole_are_refused() {
        let residuals: Vec<i64> = (0..100).map(|index| (index % 3) as i64 - 1).collect();
        let encoded = encode(&mut zstd(), &residuals).unwrap();
        assert!(decode(&mut zstd(), &encoded[..encoded.len() - 1], 100).is_err());
        assert!(decode(&mut zstd(), &[encoded.clone(), vec![0]].concat(), 100).is_err());
        assert!(decode(&mut zstd(), &[SPARSE, 0, 2], 10).is_err(), "a gap of nothing");
        assert!(decode(&mut zstd(), &[PACKED, 65], 1).is_err(), "a width past 64");
        assert!(decode(&mut zstd(), &[9], 1).is_err());
    }
}
