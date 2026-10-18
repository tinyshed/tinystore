//! A series' samples not packed yet, kept whole in its row of `heads`:
//!
//! ```text
//! 1 | count | chunk … | crc-32 of the series' id, 8 bytes, and all before it
//! chunk = start | end − start | count | first value, 8 bytes | body's length | body
//! ```
//!
//! A chunk is a run of the codec, 240 samples at most, so that each decodes
//! within the codec's bound. A write re-encodes only the chunks its samples
//! touch and copies the others byte for byte; a read decodes only the chunks
//! its range touches. The checksum binds the head to its series: a head
//! copied onto another does not read.

use super::binary::{advance, distance};
use super::codec::{self, Head, MOST_SAMPLES as CHUNK, Point};
use super::directory::MOST_BODY;
use crate::compress::checksum::Crc32;
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, put_count, put_int};
use crate::{Error, Result};

/// The samples a head holds at most: a backlog of one ingest fits, and
/// packing takes them as they become a block's worth.
pub(super) const MOST_SAMPLES: usize = 16_384;
/// What a head's row holds at most.
pub(super) const MOST_BYTES: usize = 1 << 20;
pub(super) const VERSION: u8 = 1;

/// How many samples a head holds and its first and last times, which the
/// series' state row keeps, so that a read finds the heads its range touches
/// without their bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Extent {
    pub(super) count: usize,
    pub(super) start: i64,
    pub(super) end: i64,
}

impl Extent {
    pub(super) fn of(points: &[Point]) -> Extent {
        match (points.first(), points.last()) {
            (Some(first), Some(last)) => Extent { count: points.len(), start: first.at, end: last.at },
            _ => Extent::default(),
        }
    }
}

/// A chunk of a head as its row holds it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Chunk<'a> {
    pub(super) head: Head,
    body: &'a [u8],
    /// The whole chunk as stored, copied as it is when a write keeps it.
    stored: &'a [u8],
}

impl Chunk<'_> {
    pub(super) fn overlaps(&self, from: i64, to: i64) -> bool {
        self.head.end >= from && self.head.start < to
    }
}

/// Splits a head into its chunks once its checksum, its count and its
/// extent are checked against what its state row says. It is the one reader
/// of the layout: decoding, reuse and partial reads start from what it gives.
pub(super) fn parse(series: i64, extent: Extent, bytes: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let parsed = chunks_of(series, extent, bytes);
    parsed.map_err(|error| error.within("a head"))
}

fn chunks_of(series: i64, extent: Extent, bytes: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if bytes.len() < 6 || bytes.len() > MOST_BYTES {
        return Err(Error::corrupt("its length"));
    }
    let (content, sum) = bytes.split_at(bytes.len() - 4);
    if Crc32::new().update(&series.to_le_bytes()).update(content).finish().to_le_bytes() != sum {
        return Err(Error::corrupt("its checksum"));
    }
    let mut reader = Reader::new(content);
    if reader.byte() != VERSION {
        return Err(Error::corrupt("its version"));
    }
    let count = reader.count(MOST_SAMPLES);
    if count == 0 || count != extent.count {
        return Err(Error::corrupt("its count is not its state's"));
    }
    let mut chunks: Vec<Chunk<'_>> = Vec::with_capacity(count.div_ceil(CHUNK));
    let mut parsed = 0;
    while parsed < count {
        let chunk = read_chunk(&mut reader, count - parsed)?;
        if chunks.last().is_some_and(|before| chunk.head.start <= before.head.end) {
            return Err(Error::corrupt("its chunks overlap"));
        }
        parsed += chunk.head.count;
        chunks.push(chunk);
    }
    reader.finish()?;
    if chunks[0].head.start != extent.start || chunks[chunks.len() - 1].head.end != extent.end {
        return Err(Error::corrupt("its first and last times are not its state's"));
    }
    Ok(chunks)
}

/// One chunk; `left` bounds its count.
fn read_chunk<'a>(reader: &mut Reader<'a>, left: usize) -> Result<Chunk<'a>> {
    let before = reader.remaining();
    let start = reader.int();
    let span = reader.uvarint();
    let count = reader.count(CHUNK.min(left));
    let first = f64::from_bits(reader.word());
    let body = reader.counted(MOST_BODY);
    reader.check()?;
    let stored = &before[..before.len() - reader.remaining().len()];
    let shaped = count > 0 && body.len() >= 8 && (count == 1) == (span == 0);
    if !shaped || span >= distance(start, i64::MAX) {
        return Err(Error::corrupt("a chunk's extent"));
    }
    let head = Head { start, end: advance(start, span), count, first };
    Ok(Chunk { head, body, stored })
}

/// Decodes the chunks that overlap `[from, to)` onto `out`, whole.
pub(super) fn decode(zstd: &mut Zstd, chunks: &[Chunk<'_>], from: i64, to: i64, out: &mut Vec<Point>) -> Result<()> {
    for chunk in chunks.iter().filter(|chunk| chunk.overlaps(from, to)) {
        codec::decode(zstd, &chunk.head, chunk.body, out).map_err(|error| error.within("a head's chunk"))?;
    }
    Ok(())
}

/// How many samples the chunks that overlap `[from, to)` hold.
pub(super) fn samples_within(chunks: &[Chunk<'_>], from: i64, to: i64) -> usize {
    chunks.iter().filter(|chunk| chunk.overlaps(from, to)).map(|chunk| chunk.head.count).sum()
}

/// The leading chunks a write of samples from `at` on leaves as they are:
/// full, and ending before it.
///
/// ```text
/// chunks          [0 … 239] [240 … 479] [480 … 520]
/// a write at 300                ↑
/// kept            [0 … 239]                          copied byte for byte
/// ```
pub(super) fn kept_before(chunks: &[Chunk<'_>], at: i64) -> usize {
    chunks.iter().take_while(|chunk| chunk.head.count == CHUNK && chunk.head.end < at).count()
}

/// The head of `points` behind the chunks `kept`, which are copied as they
/// are; nothing when both are empty.
///
/// ```text
/// kept      [240][240]
/// points              [240][ 57]         encoded now
/// head      1 │ 777 │ [240][240][240][ 57] │ crc-32
/// ```
pub(super) fn encode(zstd: &mut Zstd, series: i64, kept: &[Chunk<'_>], points: &[Point]) -> Result<Vec<u8>> {
    let count = kept.iter().map(|chunk| chunk.head.count).sum::<usize>() + points.len();
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > MOST_SAMPLES {
        return Err(Error::limit(format!(
            "{count} samples not packed yet, where a series holds {MOST_SAMPLES}: packing has not caught up"
        )));
    }
    let mut out = vec![VERSION];
    put_count(&mut out, count as u64);
    kept.iter().for_each(|chunk| out.extend_from_slice(chunk.stored));
    for run in points.chunks(CHUNK) {
        let (head, body) = codec::encode(zstd, run)?;
        put_int(&mut out, head.start);
        put_count(&mut out, distance(head.start, head.end));
        put_count(&mut out, head.count as u64);
        out.extend_from_slice(&head.first.to_bits().to_le_bytes());
        put_count(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        if out.len() + 4 > MOST_BYTES {
            return Err(Error::limit(format!("a head past {MOST_BYTES} bytes: packing has not caught up")));
        }
    }
    let sum = Crc32::new().update(&series.to_le_bytes()).update(&out).finish();
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

/// Two runs in the order of their times; a time in both takes the incoming
/// value.
///
/// ```text
/// existing   10  11       13
/// incoming       11'  12       14
/// merged     10  11'  12  13   14
/// ```
pub(super) fn merge(existing: &[Point], incoming: &[Point]) -> Vec<Point> {
    let mut merged = Vec::with_capacity(existing.len() + incoming.len());
    let (mut old, mut new) = (existing.iter().peekable(), incoming.iter().peekable());
    loop {
        let next = match (old.peek(), new.peek()) {
            (Some(a), Some(b)) if a.at < b.at => old.next(),
            (Some(a), Some(b)) if a.at == b.at => {
                old.next();
                new.next()
            }
            (Some(_), None) => old.next(),
            (_, Some(_)) => new.next(),
            (None, None) => return merged,
        };
        merged.extend(next.copied());
    }
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
