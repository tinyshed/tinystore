//! A block's values, apart from its times, which its clock keeps, and from
//! its first value, which its directory does. A constant block keeps no body;
//! any other takes the shortest of three:
//!
//! | Way      | Keeps                                                                          |
//! |----------|--------------------------------------------------------------------------------|
//! | ordinary | the codec's stream of the values                                               |
//! | changes  | where a value changes and by how much: whole numbers, hundredths or raw bits   |
//! | grid     | the integers a scale of ten makes of them, through the codec, and each value's |
//! |          | residual, which gives its bits back                                            |
//!
//! ```text
//! body = way | its bytes | crc-32 of the block's extent, first value and clock, and the body
//! ```
//!
//! A series keeps a hint of the scale its grid won with: the first block that
//! changes tries every scale, later ones that one alone, and one that found
//! none tries none again.

use super::clock::Times;
use super::codec::{self, POWERS_OF_TEN, Point};
use super::directory::MOST_BODY as MOST_BASE;
use super::residuals;
use crate::compress::checksum::Crc32;
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, put_count, put_int};
use crate::{Error, Result};

const ORDINARY: u8 = 0;
const CHANGES: u8 = 1;
const GRID: u8 = 2;

/// The hint of a series whose first changing block has not tried a grid.
pub(super) const UNTRIED: i64 = -2;
/// The hint of a series no grid won for.
const NO_GRID: i64 = -1;
/// The scales a grid is tried at: a 64-bit float holds 15 to 17 digits.
const MOST_GRID_SCALE: usize = 15;

/// The shortest body of a block's values, sealed, and the hint the next block
/// tries; none for a constant block.
pub(super) fn encode(zstd: &mut Zstd, points: &[Point], times: &Times, hint: i64) -> Result<(Option<Vec<u8>>, i64)> {
    let Some(changes) = changes(points) else { return Ok((None, hint)) };
    let mut best = ordinary(zstd, points)?;
    if changes.len() < best.len() {
        best = changes;
    }
    let mut chosen = if hint == UNTRIED { NO_GRID } else { hint };
    let (lowest, highest) = match hint {
        UNTRIED => (0, MOST_GRID_SCALE),
        NO_GRID => (1, 0),
        scale => (scale as usize, scale as usize),
    };
    for scale in lowest..=highest {
        if let Some(grid) = grid(zstd, points, scale)?
            && grid.len() < best.len()
        {
            (best, chosen) = (grid, scale as i64);
        }
    }
    let sum = checksum(points, times, &best);
    best.extend_from_slice(&sum.to_le_bytes());
    Ok((Some(best), chosen))
}

/// Decodes a block's values into `run`, whose times its clock filled, after
/// checking its body against the block; a block without a body holds its
/// first value throughout.
pub(super) fn decode(zstd: &mut Zstd, first: f64, times: &Times, body: &[u8], run: &mut [Point]) -> Result<()> {
    if body.is_empty() {
        run.iter_mut().for_each(|point| point.value = first);
        return Ok(());
    }
    if body.len() < 5 {
        return Err(Error::corrupt("a block's values' length"));
    }
    let (stored, sum) = body.split_at(body.len() - 4);
    if checksum_of(times, first, stored).to_le_bytes() != sum {
        return Err(Error::corrupt("a block's values' checksum"));
    }
    match stored[0] {
        ORDINARY => codec::decode_values_alone(zstd, first, &stored[1..], run),
        CHANGES => read_changes(first, &stored[1..], run),
        GRID => read_grid(zstd, first, &stored[1..], run),
        _ => Err(Error::corrupt("a block's values' way")),
    }
}

/// Binds a body to its block's extent, first value and clock, so that one
/// block's values never read as another's.
fn checksum(points: &[Point], times: &Times, body: &[u8]) -> u32 {
    checksum_of(times, points[0].value, body)
}

fn checksum_of(times: &Times, first: f64, body: &[u8]) -> u32 {
    let key = codec::Head { start: times.start, end: times.end, count: times.count, first }.key();
    Crc32::new().update(&key).update(&times.steps).update(body).finish()
}

fn ordinary(zstd: &mut Zstd, points: &[Point]) -> Result<Vec<u8>> {
    let values: Vec<f64> = points.iter().map(|point| point.value).collect();
    let stream = codec::encode_values_alone(zstd, &values)?;
    Ok([&[ORDINARY][..], &stream].concat())
}

/// Where a block's value changes and by how much, the shortest of whole
/// numbers, hundredths and raw bits; none for a constant block.
///
/// ```text
/// 5 5 5 7 7 9    whole numbers    at 3 by +2, 2 later by +2    1 0 | 3 4 | 2 4
/// ```
fn changes(points: &[Point]) -> Option<Vec<u8>> {
    let first = points[0].value.to_bits();
    if points.iter().all(|point| point.value.to_bits() == first) {
        return None;
    }
    (0..3).filter_map(|kind| change_candidate(points, kind)).min_by_key(Vec::len)
}

/// Changes kept as whole numbers, 0, hundredths, 1, or raw bits, 2; none
/// when a value is not one the kind can give back.
fn change_candidate(points: &[Point], kind: u8) -> Option<Vec<u8>> {
    let integers = change_integers(points, kind)?;
    let mut out = vec![CHANGES, kind];
    let mut previous = 0;
    for index in 1..points.len() {
        if points[index].value.to_bits() == points[index - 1].value.to_bits() {
            continue;
        }
        put_count(&mut out, (index - previous) as u64);
        previous = index;
        if kind == 2 {
            out.extend_from_slice(&points[index].value.to_bits().to_le_bytes());
        } else {
            put_int(&mut out, integers[index] - integers[index - 1]);
        }
    }
    Some(out)
}

/// The values counted in a kind's unit, when each divides back to its bits;
/// raw bits need none.
fn change_integers(points: &[Point], kind: u8) -> Option<Vec<i64>> {
    if kind == 2 {
        return Some(vec![0; points.len()]);
    }
    let factor = change_factor(kind);
    points.iter().map(|point| change_integer(point.value, factor)).collect()
}

fn change_integer(value: f64, factor: f64) -> Option<i64> {
    let scaled = (value * factor).round();
    if !scaled.is_finite() || scaled.abs() >= TWO_TO_60 {
        return None;
    }
    let integer = scaled as i64;
    ((integer as f64 / factor).to_bits() == value.to_bits()).then_some(integer)
}

const TWO_TO_60: f64 = (1_u64 << 60) as f64;

fn change_factor(kind: u8) -> f64 {
    if kind == 1 { 100.0 } else { 1.0 }
}

fn read_changes(first: f64, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    let mut reader = Reader::new(bytes);
    let kind = reader.byte();
    if kind > 2 {
        return Err(Error::corrupt("a block's changes' kind"));
    }
    let mut integer = if kind == 2 {
        0
    } else {
        change_integer(first, change_factor(kind))
            .ok_or_else(|| Error::corrupt("a block's first value as its changes"))?
    };
    let mut value = first;
    let count = run.len();
    let mut next = reader.count(count - 1);
    if next == 0 {
        reader.fail("a block's first change");
    }
    for (index, point) in run.iter_mut().enumerate() {
        if index == next && reader.check().is_ok() {
            if kind == 2 {
                value = f64::from_bits(reader.word());
            } else {
                integer = integer
                    .checked_add(reader.int())
                    .filter(|integer| integer.unsigned_abs() < 1 << 60)
                    .ok_or_else(|| Error::corrupt("a block's change past its range"))?;
                value = integer as f64 / change_factor(kind);
            }
            next = if reader.remaining().is_empty() {
                usize::MAX
            } else {
                match reader.count(count - 1 - index) {
                    0 => return Err(Error::corrupt("a block's change does not move on")),
                    gap => index + gap,
                }
            };
        }
        point.value = value;
    }
    reader.finish().map_err(|error| error.within("a block's changes"))
}

/// Orders floats as their bits read as integers: the grid's residuals are
/// counted in floats between two values.
fn ordered(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 == 1 { !bits } else { bits ^ (1 << 63) }
}

fn from_ordered(word: u64) -> f64 {
    if word >> 63 == 0 { f64::from_bits(!word) } else { f64::from_bits(word ^ (1 << 63)) }
}

/// The values on the grid of `scale`, their integers through the codec, and
/// each one's residual; none when a value has no point on it.
fn grid(zstd: &mut Zstd, points: &[Point], scale: usize) -> Result<Option<Vec<u8>>> {
    let factor = POWERS_OF_TEN[scale];
    let mut quantized = Vec::with_capacity(points.len());
    let mut residuals = Vec::with_capacity(points.len() - 1);
    for (index, point) in points.iter().enumerate() {
        let on_grid = (point.value * factor).round();
        if !on_grid.is_finite() || on_grid.abs() > TWO_TO_53 {
            return Ok(None);
        }
        quantized.push(on_grid);
        if index > 0 {
            residuals.push(ordered(point.value).wrapping_sub(ordered(on_grid / factor)) as i64);
        }
    }
    let base = codec::encode_values_alone(zstd, &quantized)?;
    let mut out = vec![GRID, scale as u8];
    put_count(&mut out, base.len() as u64 + 1);
    out.push(ORDINARY);
    out.extend_from_slice(&base);
    out.extend_from_slice(&residuals::encode(zstd, &residuals)?);
    Ok(Some(out))
}

const TWO_TO_53: f64 = (1_u64 << 53) as f64;

/// Decodes the grid's integers as values on it, then adds each one's
/// residual back in the order of floats, which restores every bit.
fn read_grid(zstd: &mut Zstd, first: f64, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    let mut reader = Reader::new(bytes);
    let scale = usize::from(reader.byte());
    if scale > MOST_GRID_SCALE {
        return Err(Error::corrupt("a grid's scale"));
    }
    let base = reader.counted(MOST_BASE);
    reader.check()?;
    let factor = POWERS_OF_TEN[scale];
    let seed = (first * factor).round();
    if !seed.is_finite() || seed.abs() > TWO_TO_53 {
        return Err(Error::corrupt("a grid's first value"));
    }
    if base.first() != Some(&ORDINARY) {
        return Err(Error::corrupt("a grid's integers"));
    }
    codec::decode_values_alone(zstd, seed, &base[1..], run)?;
    let residuals = residuals::decode(zstd, reader.rest(), run.len() - 1)?;
    run[0].value = first;
    for (point, residual) in run[1..].iter_mut().zip(residuals) {
        point.value = from_ordered(ordered(point.value / factor).wrapping_add(residual as u64));
    }
    Ok(())
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod tests;
