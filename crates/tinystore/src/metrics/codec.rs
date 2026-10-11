//! The codec of a run of samples: up to 240 times and values packed apart,
//! the body a head's chunk keeps, and a block's values in their ordinary
//! representation.
//!
//! ```text
//! body  = 1 | flags | the times' length, 2 bytes | times | values | crc-32 of the head and all before it
//! flags = how the times are kept | how the values are, << 2 | 1 << 5 when they went through zstd
//! ```
//!
//! A run's first and last times, its count and its first value are its
//! [`Head`], which whoever keeps the body keeps beside it: the body never
//! repeats them, and its checksum binds it to them.
//!
//! | Times        | Kept as                                                 |
//! |--------------|---------------------------------------------------------|
//! | evenly apart | nothing: the head's span over its count is the step     |
//! | deltas       | each step from the time before                          |
//! | changes      | the first step, then each step's change from the one before |
//!
//! | Values   | Kept as                                                                 |
//! |----------|-------------------------------------------------------------------------|
//! | constant | nothing: every one is the first                                         |
//! | integers | each one's difference from the one before, ZigZag, in Simple8b words    |
//! | decimals | the integers a power of ten makes of them, which give each one's bits back, so |
//! | XOR      | each one's bits against the bits before, as Gorilla keeps them          |
//! | raw      | eight bytes each                                                        |

use std::borrow::Cow;
use std::sync::{Mutex, PoisonError};

use super::binary::{advance, distance};
use crate::compress::bits::{BitReader, BitWriter};
use crate::compress::checksum::Crc32;
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, fold, put_count, put_int, unfold};
use crate::{Error, Result};

/// The samples a run holds at most.
pub(super) const MOST_SAMPLES: usize = 240;
/// What a body's stream holds at most, before zstd and after it.
const MOST_STREAM: usize = 8192;
const VERSION: u8 = 1;
const HEADER: usize = 4;
const CHECKSUM: usize = 4;

const TIMES_EVEN: u8 = 0;
const TIMES_DELTAS: u8 = 1;
const TIMES_CHANGES: u8 = 2;

const VALUES_RAW: u8 = 0;
const VALUES_CONSTANT: u8 = 1;
const VALUES_INTEGERS: u8 = 2;
const VALUES_XOR: u8 = 3;
const VALUES_DECIMALS: u8 = 4;

const UNDER_ZSTD: u8 = 1 << 5;
/// zstd is not tried on a stream shorter than this, where its frame alone
/// would cost what it saves.
const WORTH_ZSTD: usize = 48;

/// How a run's integers are packed: Simple8b words. The Go engine also
/// weighed huff0 against them, as 1; this one leaves it out until a
/// measurement says what it saves.
const DELTAS_IN_WORDS: u8 = 0;

/// The scales of ten a run of decimals is tried at: past nine, research's
/// fixtures gained no bytes and cost five times the encode.
const MOST_SCALE: usize = 9;
pub(super) const POWERS_OF_TEN: [f64; 16] =
    [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15];

/// A sample as the engine keeps it: unix milliseconds, and a value whose bits
/// are kept as they came.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Point {
    pub(super) at: i64,
    pub(super) value: f64,
}

/// What the row around a body holds: the body's first and last times, its
/// count and its first value.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Head {
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) count: usize,
    pub(super) first: f64,
}

impl Head {
    pub(super) fn of(points: &[Point]) -> Head {
        let (first, last) = (points[0], points[points.len() - 1]);
        Head { start: first.at, end: last.at, count: points.len(), first: first.value }
    }

    /// The bytes a body's checksum binds it to.
    pub(super) fn key(&self) -> [u8; 26] {
        let mut key = [0; 26];
        key[..8].copy_from_slice(&self.start.to_le_bytes());
        key[8..16].copy_from_slice(&self.end.to_le_bytes());
        key[16..18].copy_from_slice(&(self.count as u16).to_le_bytes());
        key[18..].copy_from_slice(&self.first.to_bits().to_le_bytes());
        key
    }

    /// The step between times evenly apart, which an even run does not write
    /// down.
    pub(super) fn step(&self) -> Option<u64> {
        let gaps = u64::try_from(self.count.checked_sub(1)?).ok().filter(|gaps| *gaps > 0)?;
        let span = distance(self.start, self.end);
        (span > 0 && span.is_multiple_of(gaps)).then_some(span / gaps)
    }

    /// The head is one a body can have: within its count, its times in order.
    fn check(&self) -> Result<()> {
        let one = self.count == 1 && self.end != self.start;
        if self.count == 0 || self.count > MOST_SAMPLES || self.end < self.start || one {
            return Err(Error::corrupt("a run's head"));
        }
        Ok(())
    }
}

/// zstd's window, 2¹³ bytes, which no object it squeezes passes.
const WINDOW_LOG: u32 = 13;
/// How hard a run is squeezed: zstd's default, the Go engine's.
const LEVEL: i32 = 3;

/// The zstd contexts the engine's calls borrow, so that a read does not make
/// its own: a few are kept, the rest let go when given back.
#[derive(Debug, Default)]
pub(super) struct Codecs {
    kept: Mutex<Vec<Zstd>>,
}

/// The contexts kept for the next calls.
const KEPT_CODECS: usize = 8;

impl Codecs {
    pub(super) fn take(&self) -> Zstd {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner).pop();
        kept.unwrap_or(Zstd::windowed(LEVEL, WINDOW_LOG))
    }

    pub(super) fn give_back(&self, zstd: Zstd) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() < KEPT_CODECS {
            kept.push(zstd);
        }
    }

    /// Lets every context go, as a store that stopped writing does.
    pub(super) fn rest(&self) {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

/// Packs a run of 1 to 240 points, their times strictly increasing, and gives
/// its head and its body.
pub(super) fn encode(zstd: &mut Zstd, points: &[Point]) -> Result<(Head, Vec<u8>)> {
    if points.is_empty() || points.len() > MOST_SAMPLES {
        return Err(Error::internal(format!("metrics: a run of {} samples, where 1 to 240 make one", points.len())));
    }
    if points.windows(2).any(|pair| pair[1].at <= pair[0].at) {
        return Err(Error::internal("metrics: a run's times do not increase"));
    }
    let head = Head::of(points);
    let (time_mode, times) = encode_times(&head, points);
    let (value_mode, values) = encode_values(points);
    let mut best = envelope(zstd, &head, Packed { time_mode, times: &times, value_mode, values: &values })?;
    // zstd sometimes keeps raw values shorter than a stream already packed
    let words = matches!(value_mode, VALUES_INTEGERS | VALUES_DECIMALS) && points.len() >= 32;
    if words || (value_mode == VALUES_XOR && points.len() >= 8) {
        let raw = raw_values(points);
        let other = envelope(zstd, &head, Packed { time_mode, times: &times, value_mode: VALUES_RAW, values: &raw })?;
        if other.len() < best.len() {
            best = other;
        }
    }
    Ok((head, best))
}

/// Decodes a body back onto `out`, after checking it against its head.
pub(super) fn decode(zstd: &mut Zstd, head: &Head, body: &[u8], out: &mut Vec<Point>) -> Result<()> {
    head.check()?;
    if body.len() < HEADER + CHECKSUM || body.len() > MOST_STREAM + HEADER + CHECKSUM {
        return Err(Error::corrupt("a run's body length"));
    }
    if body[0] != VERSION {
        return Err(Error::corrupt("a run's version"));
    }
    let (framed, sum) = body.split_at(body.len() - CHECKSUM);
    if Crc32::new().update(&head.key()).update(framed).finish().to_le_bytes() != sum {
        return Err(Error::corrupt("a run's checksum"));
    }
    let flags = framed[1];
    let stream = stream_of(zstd, flags, &framed[HEADER..])?;
    let times = usize::from(u16::from_le_bytes([framed[2], framed[3]]));
    let Some((time_bytes, value_bytes)) = stream.split_at_checked(times) else {
        return Err(Error::corrupt("a run's times past its stream"));
    };
    let base = out.len();
    out.resize(base + head.count, Point::default());
    let run = &mut out[base..];
    let decoded =
        decode_times(head, flags & 3, time_bytes, run).and_then(|()| decode_values(head, flags, value_bytes, run));
    if decoded.is_err() {
        out.truncate(base);
    }
    decoded
}

/// Packs values alone, for a block whose times its clock keeps and whose
/// first value its directory does: the stream [`encode`] writes for evenly
/// spaced samples, behind the byte that says how it is packed.
///
/// ```text
/// encode         1 | flags | 0 0 | stream | checksum
/// encode_values      flags |       stream
/// ```
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "a block's directory and values, which packing writes from the next slice")
)]
pub(super) fn encode_values_alone(zstd: &mut Zstd, values: &[f64]) -> Result<Vec<u8>> {
    let points: Vec<Point> = values.iter().zip(0..).map(|(&value, at)| Point { at, value }).collect();
    let (_, body) = encode(zstd, &points)?;
    let mut stream = Vec::with_capacity(body.len() - HEADER - CHECKSUM + 1);
    stream.push(body[1]);
    stream.extend_from_slice(&body[HEADER..body.len() - CHECKSUM]);
    Ok(stream)
}

/// Decodes what [`encode_values_alone`] wrote into the values of `run`, whose
/// times its caller fills. The stream has no checksum of its own: its caller
/// checked the bytes it was kept in.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "a block's directory and values, which packing writes from the next slice")
)]
pub(super) fn decode_values_alone(zstd: &mut Zstd, first: f64, stream: &[u8], run: &mut [Point]) -> Result<()> {
    if run.is_empty() || run.len() > MOST_SAMPLES {
        return Err(Error::corrupt("a block's count"));
    }
    let Some((&flags, payload)) = stream.split_first() else { return Err(Error::corrupt("a value stream's length")) };
    if flags & 3 != TIMES_EVEN {
        return Err(Error::corrupt("a value stream with times"));
    }
    let stream = stream_of(zstd, flags, payload)?;
    let last = i64::try_from(run.len() - 1).unwrap_or(0);
    decode_values(&Head { start: 0, end: last, count: run.len(), first }, flags, &stream, run)
}

/// A run's times and values as packed, with the modes they were packed in.
struct Packed<'a> {
    time_mode: u8,
    times: &'a [u8],
    value_mode: u8,
    values: &'a [u8],
}

fn envelope(zstd: &mut Zstd, head: &Head, packed: Packed<'_>) -> Result<Vec<u8>> {
    let mut stream = Vec::with_capacity(packed.times.len() + packed.values.len());
    stream.extend_from_slice(packed.times);
    stream.extend_from_slice(packed.values);
    let mut flags = packed.time_mode | (packed.value_mode << 2);
    if stream.len() >= WORTH_ZSTD {
        let squeezed = zstd.squeeze(&stream).map_err(|error| error.within("metrics"))?;
        if squeezed.len() < stream.len() {
            stream = squeezed;
            flags |= UNDER_ZSTD;
        }
    }
    let mut out = Vec::with_capacity(HEADER + stream.len() + CHECKSUM);
    out.extend_from_slice(&[VERSION, flags]);
    out.extend_from_slice(&(packed.times.len() as u16).to_le_bytes());
    out.extend_from_slice(&stream);
    let sum = Crc32::new().update(&head.key()).update(&out).finish();
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

/// A run's stream, expanded when it went through zstd, once its flags are
/// checked.
fn stream_of<'p>(zstd: &mut Zstd, flags: u8, payload: &'p [u8]) -> Result<Cow<'p, [u8]>> {
    let (times, values, rest) = (flags & 3, (flags >> 2) & 7, flags >> 5);
    if times > TIMES_CHANGES || values > VALUES_DECIMALS || rest > 1 {
        return Err(Error::corrupt("a run's flags"));
    }
    let stream =
        if flags & UNDER_ZSTD == 0 { Cow::Borrowed(payload) } else { Cow::Owned(zstd.expand(payload, MOST_STREAM)?) };
    if stream.len() > MOST_STREAM {
        return Err(Error::corrupt("a run's stream past its length"));
    }
    Ok(stream)
}

/// Writes nothing for a run evenly apart: its head's first and last times and
/// its count already say the step.
fn encode_times(head: &Head, points: &[Point]) -> (u8, Vec<u8>) {
    if points.len() == 1 {
        return (TIMES_EVEN, Vec::new());
    }
    if let Some(step) = head.step()
        && points.windows(2).all(|pair| distance(pair[0].at, pair[1].at) == step)
    {
        return (TIMES_EVEN, Vec::new());
    }
    let mut deltas = Vec::with_capacity(points.len() * 2);
    let mut changes = Vec::with_capacity(points.len() * 2);
    let mut previous = distance(points[0].at, points[1].at);
    put_count(&mut changes, previous);
    let mut signed = previous <= i64::MAX as u64;
    for (index, pair) in points.windows(2).enumerate() {
        let delta = distance(pair[0].at, pair[1].at);
        put_count(&mut deltas, delta);
        signed = signed && delta <= i64::MAX as u64;
        if index > 0 && signed {
            put_int(&mut changes, delta as i64 - previous as i64);
        }
        previous = delta;
    }
    if signed && changes.len() < deltas.len() { (TIMES_CHANGES, changes) } else { (TIMES_DELTAS, deltas) }
}

fn decode_times(head: &Head, mode: u8, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    let mut at = head.start;
    run[0].at = at;
    match mode {
        TIMES_EVEN => {
            if run.len() > 1 {
                let step = head.step().ok_or_else(|| Error::corrupt("an even run's step"))?;
                for point in &mut run[1..] {
                    at = advance(at, step);
                    point.at = at;
                }
            }
            if !bytes.is_empty() {
                return Err(Error::corrupt("times after an even run's"));
            }
        }
        _ => {
            let mut reader = Reader::new(bytes);
            let mut delta = 0_u64;
            for (index, point) in run.iter_mut().enumerate().skip(1) {
                delta = if mode == TIMES_DELTAS || index == 1 {
                    reader.uvarint()
                } else {
                    let changed = (delta as i64).checked_add(reader.int()).filter(|_| delta <= i64::MAX as u64);
                    changed.and_then(|changed| u64::try_from(changed).ok()).unwrap_or(0)
                };
                if delta == 0 || delta > distance(at, i64::MAX) {
                    return Err(Error::corrupt("a run's times do not increase, or pass the end of time"));
                }
                at = advance(at, delta);
                point.at = at;
            }
            reader.finish()?;
        }
    }
    if at != head.end {
        return Err(Error::corrupt("a run's last time is not its head's"));
    }
    Ok(())
}

fn encode_values(points: &[Point]) -> (u8, Vec<u8>) {
    let first = points[0].value.to_bits();
    if points.iter().all(|point| point.value.to_bits() == first) {
        return (VALUES_CONSTANT, Vec::new());
    }
    let (mut mode, mut best) = (VALUES_RAW, raw_values(points));
    match encode_integers(points) {
        Some(integers) if integers.len() < best.len() => (mode, best) = (VALUES_INTEGERS, integers),
        Some(_) => {}
        None => {
            if let Some(decimals) = encode_decimals(points).filter(|decimals| decimals.len() < best.len()) {
                (mode, best) = (VALUES_DECIMALS, decimals);
            }
        }
    }
    let xor = encode_xor(points);
    if xor.len() < best.len() {
        (mode, best) = (VALUES_XOR, xor);
    }
    (mode, best)
}

/// The values after the first, eight bytes each: the head keeps the first.
fn raw_values(points: &[Point]) -> Vec<u8> {
    points[1..].iter().flat_map(|point| point.value.to_bits().to_le_bytes()).collect()
}

/// Decodes the values of a run whose flags are `flags`.
fn decode_values(head: &Head, flags: u8, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    run[0].value = head.first;
    match (flags >> 2) & 7 {
        VALUES_CONSTANT => {
            run[1..].iter_mut().for_each(|point| point.value = head.first);
            if !bytes.is_empty() {
                return Err(Error::corrupt("values after a constant run's"));
            }
            Ok(())
        }
        VALUES_RAW => {
            let (words, rest) = bytes.as_chunks::<8>();
            if words.len() != run.len() - 1 || !rest.is_empty() {
                return Err(Error::corrupt("a raw run's length"));
            }
            for (point, word) in run[1..].iter_mut().zip(words) {
                point.value = f64::from_bits(u64::from_le_bytes(*word));
            }
            Ok(())
        }
        mode @ (VALUES_INTEGERS | VALUES_DECIMALS) => decode_integers(head, mode == VALUES_DECIMALS, bytes, run),
        _ => decode_xor(head, bytes, run),
    }
}

/// The run as integers, when every value is one exactly and each difference
/// fits a Simple8b word's widest field.
fn encode_integers(points: &[Point]) -> Option<Vec<u8>> {
    let integers = points.iter().map(|point| exact_integer(point.value)).collect::<Option<Vec<i64>>>()?;
    let deltas = folded_deltas(&integers)?;
    Some(pack_words(vec![DELTAS_IN_WORDS], &deltas))
}

/// The run as the integers a power of ten makes of it, when one up to 10⁹
/// gives every value's bits back; integers alone are [`encode_integers`]'.
fn encode_decimals(points: &[Point]) -> Option<Vec<u8>> {
    let scale =
        points.iter().map(|point| exact_scale(point.value)).try_fold(0, |most, scale| Some(most.max(scale?)))?;
    if scale == 0 {
        return None;
    }
    let factor = POWERS_OF_TEN[scale];
    let integers = points
        .iter()
        .map(|point| {
            let scaled = (point.value * factor).round();
            let integer = within_i64(scaled)?;
            ((integer as f64 / factor).to_bits() == point.value.to_bits()).then_some(integer)
        })
        .collect::<Option<Vec<i64>>>()?;
    let deltas = folded_deltas(&integers)?;
    Some(pack_words(vec![DELTAS_IN_WORDS, scale as u8], &deltas))
}

/// The smallest power of ten whose integers give `value`'s bits back; none
/// for a value no scale up to 10⁹ reproduces, `-0` among them.
pub(super) fn exact_scale(value: f64) -> Option<usize> {
    if !value.is_finite() {
        return None;
    }
    for (scale, factor) in POWERS_OF_TEN.iter().enumerate().take(MOST_SCALE + 1) {
        let integer = within_i64((value * factor).round())?;
        if (integer as f64 / factor).to_bits() == value.to_bits() {
            return Some(scale);
        }
    }
    None
}

fn exact_integer(value: f64) -> Option<i64> {
    let integer = within_i64(value)?;
    ((integer as f64).to_bits() == value.to_bits()).then_some(integer)
}

/// `value` as an `i64`, truncated, when it is one.
fn within_i64(value: f64) -> Option<i64> {
    const LIMIT: f64 = 9_223_372_036_854_775_808.0; // 2⁶³
    (-LIMIT..LIMIT).contains(&value).then_some(value as i64)
}

/// Each difference from the integer before, folded; none when one overflows
/// or passes 2⁶⁰, the widest field of a word.
fn folded_deltas(integers: &[i64]) -> Option<Vec<u64>> {
    integers.windows(2).map(|pair| pair[1].checked_sub(pair[0]).map(fold).filter(|folded| *folded < 1 << 60)).collect()
}

/// Simple8b: a selector in a word's top four bits says how many fields it
/// holds and how wide; the fields fill it from its lowest bit. The two
/// selectors of no width hold runs of ones.
const WORD_COUNTS: [usize; 16] = [240, 120, 60, 30, 20, 15, 12, 10, 8, 7, 6, 5, 4, 3, 2, 1];
const WORD_WIDTHS: [u32; 16] = [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 15, 20, 30, 60];

fn pack_words(mut out: Vec<u8>, mut values: &[u64]) -> Vec<u8> {
    while !values.is_empty() {
        for (selector, (&capacity, &width)) in WORD_COUNTS.iter().zip(&WORD_WIDTHS).enumerate() {
            let taken = capacity.min(values.len());
            let fits = if width == 0 {
                values.len() >= capacity && values[..taken].iter().all(|&value| value == 1)
            } else {
                values[..taken].iter().all(|&value| value >> width == 0)
            };
            if !fits {
                continue;
            }
            let mut word = (selector as u64) << 60;
            if width > 0 {
                for (place, &value) in values[..taken].iter().enumerate() {
                    word |= value << (place as u32 * width);
                }
            }
            out.extend_from_slice(&word.to_le_bytes());
            values = &values[taken..];
            break;
        }
    }
    out
}

fn decode_integers(head: &Head, decimals: bool, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    let mut reader = Reader::new(bytes);
    if reader.byte() != DELTAS_IN_WORDS {
        return Err(Error::corrupt("a run's integers are packed in a way this engine does not read"));
    }
    let (scale, mut integer) = if decimals {
        let scale = usize::from(reader.byte());
        if scale > MOST_SCALE {
            return Err(Error::corrupt("a run's scale"));
        }
        let first = within_i64((head.first * POWERS_OF_TEN[scale]).round());
        let first = first.filter(|first| (*first as f64 / POWERS_OF_TEN[scale]).to_bits() == head.first.to_bits());
        (Some(scale), first.ok_or_else(|| Error::corrupt("a run's first decimal"))?)
    } else {
        (None, exact_integer(head.first).ok_or_else(|| Error::corrupt("a run's first integer"))?)
    };
    reader.check()?;
    let (words, rest) = reader.rest().as_chunks::<8>();
    if !rest.is_empty() {
        return Err(Error::corrupt("a run's integer words"));
    }
    let mut words = words.iter();
    let mut index = 1;
    while index < run.len() {
        let word = u64::from_le_bytes(*words.next().ok_or_else(|| Error::corrupt("a run ends inside its words"))?);
        let selector = (word >> 60) as usize;
        let (capacity, width) = (WORD_COUNTS[selector], WORD_WIDTHS[selector]);
        let mut fields = word & ((1 << 60) - 1);
        let taken = capacity.min(run.len() - index);
        if width == 0 && (fields != 0 || capacity > run.len() - index) {
            return Err(Error::corrupt("a run of ones past its run's end"));
        }
        for point in &mut run[index..index + taken] {
            let folded = if width == 0 { 1 } else { fields & ((1 << width) - 1) };
            fields = if width == 0 { 0 } else { fields >> width };
            integer = integer.checked_add(unfold(folded)).ok_or_else(|| Error::corrupt("a run's integer overflows"))?;
            point.value = match scale {
                Some(scale) => integer as f64 / POWERS_OF_TEN[scale],
                None => {
                    let value = integer as f64;
                    if value >= 9_223_372_036_854_775_808.0 || value as i64 != integer {
                        return Err(Error::corrupt("a run's integer is not exact"));
                    }
                    value
                }
            };
        }
        if fields != 0 {
            return Err(Error::corrupt("a word's padding"));
        }
        index += taken;
    }
    if words.next().is_some() {
        return Err(Error::corrupt("words after a run's last integer"));
    }
    Ok(())
}

/// Gorilla's XOR: a bit says a value repeats; else the differing bits, inside
/// the window of the last value that opened one when they fit it, or a new
/// window of their own.
fn encode_xor(points: &[Point]) -> Vec<u8> {
    let mut writer = BitWriter::onto(Vec::new());
    let mut previous = points[0].value.to_bits();
    let (mut leading, mut trailing, mut window) = (0, 0, false);
    for point in &points[1..] {
        let current = point.value.to_bits();
        let difference = current ^ previous;
        previous = current;
        if difference == 0 {
            writer.write(0, 1);
            continue;
        }
        writer.write(1, 1);
        let (lead, trail) = (difference.leading_zeros().min(31), difference.trailing_zeros());
        if window && lead >= leading && trail >= trailing {
            writer.write(0, 1);
            writer.write(difference >> trailing, 64 - leading - trailing);
        } else {
            let width = 64 - lead - trail;
            writer.write(1, 1);
            writer.write(u64::from(lead), 5);
            writer.write(u64::from(width & 63), 6);
            writer.write(difference >> trail, width);
            (leading, trailing, window) = (lead, trail, true);
        }
    }
    writer.finish()
}

fn decode_xor(head: &Head, bytes: &[u8], run: &mut [Point]) -> Result<()> {
    let mut reader = BitReader::of(bytes);
    let mut value = head.first.to_bits();
    let (mut leading, mut trailing, mut window) = (0, 0, false);
    for point in &mut run[1..] {
        if reader.read(1) != 0 {
            if reader.read(1) != 0 {
                leading = reader.read(5) as u32;
                let width = match reader.read(6) as u32 {
                    0 => 64,
                    width => width,
                };
                if leading + width > 64 {
                    return Err(Error::corrupt("an XOR window past 64 bits"));
                }
                (trailing, window) = (64 - leading - width, true);
            } else if !window {
                return Err(Error::corrupt("an XOR value inside a window never opened"));
            }
            value ^= reader.read(64 - leading - trailing) << trailing;
        }
        point.value = f64::from_bits(value);
    }
    if !reader.finished() {
        return Err(Error::corrupt("an XOR run's bits"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
