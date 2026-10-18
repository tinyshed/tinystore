//! A group: up to 32 blocks of one series, packed, its directory in one row
//! of `groups` and its times in a shared clock. A directory says of each
//! block its first value, its summary but what its prediction says, its exact
//! sums, and where its body lives: inline when it is 16 bytes or fewer, else
//! the payload its id names. A payload's id never moves, so that a merge
//! rewrites directories and no body.
//!
//! ```text
//! directory = 5 | blocks | live, 4 bytes | given rows, 4 bytes | way | entries, through zstd when shorter | crc-32
//! entry     = first value | flags | the summary's floats that miss their prediction | resets
//!             | the exact sum and increase | the body's length | the body, or its payload's id
//! ```
//!
//! The checksum binds a directory to its series, its first and last times and
//! its clock, which its row holds: the Go engine kept the clock's id in the
//! bytes too, 8 bytes a group a page never needed.

use super::binary::{put_float, read_float};
use super::clock::{MOST_BLOCKS, Times};
use super::exact::{BLOCK_BITS, Exact};
use super::summary::{Summary, Sums};
use crate::compress::checksum::Crc32;
use crate::compress::frame::Zstd;
use crate::compress::varint::{Reader, put_count};
use crate::{Error, Result};

/// The directory's format: 1 until v0.1.0, since nothing before it is kept.
pub(super) const VERSION: u8 = 1;
const HEADER: usize = 10;
/// A body this long or shorter lives in its directory.
pub(super) const INLINE: usize = 16;
/// What a directory row holds at most.
pub(super) const MOST_BYTES: usize = 8192;
/// What a directory's entries take at most before zstd as packing fills
/// them, which leaves room for its header and its checksum whatever zstd
/// makes of them.
pub(super) const MOST_ENTRIES: usize = MOST_BYTES - 128;
/// What a block's body takes at most.
pub(super) const MOST_BODY: usize = 8200;

const FLAG_VALID: u8 = 1 << 5;
const FLAG_EXACT: u8 = 1 << 6;

/// A block of a group, as its directory and its clock describe it.
#[derive(Clone, Debug)]
pub(super) struct Block {
    pub(super) times: Times,
    pub(super) first: f64,
    pub(super) summary: Summary,
    pub(super) body_len: usize,
    /// Its payload's id when its body lives in one, once it was given one.
    pub(super) payload: i64,
    /// Its body: inline, or its payload's once fetched.
    pub(super) body: Vec<u8>,
}

impl Block {
    pub(super) fn external(&self) -> bool {
        self.body_len > INLINE
    }
}

/// A group as its row and its directory hold it.
#[derive(Clone, Debug)]
pub(super) struct Group {
    pub(super) series: i64,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) clock: i64,
    /// The slots that still count; retention takes a slot out, never moves one.
    pub(super) live: u32,
    pub(super) blocks: Vec<Block>,
}

impl Group {
    pub(super) fn is_live(&self, slot: usize) -> bool {
        self.live >> slot & 1 == 1
    }

    /// The slots given rows of `payloads`.
    fn given(&self) -> u32 {
        self.blocks.iter().enumerate().filter(|(_, block)| block.external()).fold(0, |mask, (slot, _)| mask | 1 << slot)
    }

    /// What binds a directory to its group.
    pub(super) fn checksum(&self, bytes: &[u8]) -> u32 {
        let mut key = [0_u8; 32];
        for (place, value) in [self.series, self.start, self.end, self.clock].iter().enumerate() {
            key[place * 8..place * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        Crc32::new().update(&key).update(bytes).finish()
    }

    /// The entries of every block, before zstd.
    pub(super) fn entries(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.blocks.len() * 24);
        self.blocks.iter().for_each(|block| put_entry(&mut raw, block));
        raw
    }

    /// The directory's bytes.
    pub(super) fn encode(&self, zstd: &mut Zstd) -> Result<Vec<u8>> {
        let raw = self.entries();
        if raw.len() > MOST_BYTES {
            return Err(Error::limit("a group's directory past its bound"));
        }
        let mut out = vec![VERSION, self.blocks.len() as u8];
        out.extend_from_slice(&self.live.to_le_bytes());
        out.extend_from_slice(&self.given().to_le_bytes());
        let squeezed = zstd.squeeze(&raw).map_err(|error| error.within("metrics"))?;
        if squeezed.len() < raw.len() {
            out.push(1);
            out.extend_from_slice(&squeezed);
        } else {
            out.push(0);
            out.extend_from_slice(&raw);
        }
        if out.len() + 4 > MOST_BYTES {
            return Err(Error::limit("a group's directory past its bound"));
        }
        let sum = self.checksum(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        Ok(out)
    }

    /// A group read back from its row and its clock, every block's entry
    /// checked against them.
    pub(super) fn decode(zstd: &mut Zstd, row: &Row, clock: Vec<Times>) -> Result<Group> {
        let decoded = decoded(zstd, row, clock);
        decoded.map_err(|error| error.within(format!("a group of series {} from {}", row.series, row.start)))
    }
}

/// A group's row: what a read finds before it decodes the directory.
#[derive(Clone, Debug)]
pub(super) struct Row {
    pub(super) series: i64,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) clock: i64,
    pub(super) directory: Vec<u8>,
}

fn decoded(zstd: &mut Zstd, row: &Row, clock: Vec<Times>) -> Result<Group> {
    let bytes = &row.directory;
    if bytes.len() < HEADER + 1 + 4 || bytes.len() > MOST_BYTES || bytes[0] != VERSION {
        return Err(Error::corrupt("its directory's version or length"));
    }
    let mut group =
        Group { series: row.series, start: row.start, end: row.end, clock: row.clock, live: 0, blocks: Vec::new() };
    let (content, sum) = bytes.split_at(bytes.len() - 4);
    if group.checksum(content).to_le_bytes() != sum {
        return Err(Error::corrupt("its directory's checksum"));
    }
    let count = usize::from(content[1]);
    let live = u32::from_le_bytes(content[2..6].try_into().unwrap_or_default());
    let given = u32::from_le_bytes(content[6..10].try_into().unwrap_or_default());
    // the slots past its count; a count past what a word holds is refused below
    let beyond = 1_u32.checked_shl(u32::from(content[1])).map_or(0, |bit| !(bit - 1));
    if count == 0 || count > MOST_BLOCKS || count != clock.len() || live == 0 || (live | given) & beyond != 0 {
        return Err(Error::corrupt("its directory's slots"));
    }
    group.live = live;
    let raw = match content[HEADER] {
        0 => content[HEADER + 1..].to_vec(),
        1 => zstd.expand(&content[HEADER + 1..], MOST_BYTES)?,
        _ => return Err(Error::corrupt("its directory's way")),
    };
    let mut reader = Reader::new(&raw);
    for (slot, times) in clock.into_iter().enumerate() {
        let block = read_entry(&mut reader, times, given >> slot & 1 == 1)?;
        if block.payload != 0 && group.blocks.iter().any(|before| before.payload == block.payload) {
            return Err(Error::corrupt("a payload two blocks name"));
        }
        group.blocks.push(block);
    }
    reader.finish()?;
    if group.blocks[0].times.start != row.start || group.blocks[count - 1].times.end != row.end {
        return Err(Error::corrupt("its first and last times are not its row's"));
    }
    Ok(group)
}

/// What a block's entry takes in a directory, before zstd.
pub(super) fn entry_len(block: &Block) -> usize {
    let mut out = Vec::new();
    put_entry(&mut out, block);
    out.len()
}

/// What a summary holds when every sample is the first, which an entry
/// keeps no float of:
///
/// ```text
/// first 42, 240 samples    last 42, min 42, max 42, sum 10080, increase 0
/// ```
fn predictions(times: &Times, first: f64) -> [f64; 5] {
    [first, first, first, times.count as f64 * first, 0.0]
}

fn put_entry(out: &mut Vec<u8>, block: &Block) {
    put_float(out, block.first);
    let summary = &block.summary;
    let values = [summary.last, summary.min, summary.max, summary.sum, summary.increase];
    let predicted = predictions(&block.times, block.first);
    let mut flags = 0_u8;
    if summary.valid {
        flags |= FLAG_VALID;
    }
    if summary.exact.is_some() {
        flags |= FLAG_EXACT;
    }
    for (bit, (value, prediction)) in values.iter().zip(predicted).enumerate() {
        if value.to_bits() == prediction.to_bits() {
            flags |= 1 << bit;
        }
    }
    out.push(flags);
    for (bit, value) in values.iter().enumerate() {
        if flags & (1 << bit) == 0 {
            put_float(out, *value);
        }
    }
    put_count(out, u64::from(summary.resets));
    if let Some(sums) = &summary.exact {
        out.extend_from_slice(&sums.sum);
        out.extend_from_slice(&sums.increase);
    }
    put_count(out, block.body_len as u64);
    if block.external() {
        put_count(out, block.payload.unsigned_abs());
    } else {
        out.extend_from_slice(&block.body);
    }
}

fn read_entry(reader: &mut Reader<'_>, times: Times, external: bool) -> Result<Block> {
    let first = read_float(reader);
    let flags = reader.byte();
    if flags & 0x80 != 0 {
        reader.fail("a summary's flags");
    }
    let mut values = predictions(&times, first);
    for (bit, value) in values.iter_mut().enumerate() {
        if flags & (1 << bit) == 0 {
            *value = read_float(reader);
        }
    }
    let resets = reader.count(times.count.saturating_sub(1)) as u16;
    let exact = (flags & FLAG_EXACT != 0).then(|| Sums { sum: canonical(reader), increase: canonical(reader) });
    let [last, min, max, sum, increase] = values;
    let summary = Summary { last, min, max, sum, increase, resets, valid: flags & FLAG_VALID != 0, exact };
    let body_len = reader.count(MOST_BODY);
    reader.check()?;
    if body_len > 0 && body_len < 6 || external != (body_len > INLINE) {
        return Err(Error::corrupt("a block's body length"));
    }
    let mut block = Block { times, first, summary, body_len, payload: 0, body: Vec::new() };
    if external {
        block.payload = i64::try_from(reader.uvarint()).ok().filter(|id| *id > 0).unwrap_or(0);
        if block.payload == 0 {
            reader.fail("a block's payload id");
        }
    } else {
        block.body = reader.take(body_len).to_vec();
    }
    reader.check()?;
    Ok(block)
}

/// An exact sum's bytes as an entry spells them, checked to be its one
/// spelling.
fn canonical(reader: &mut Reader<'_>) -> Vec<u8> {
    let before = reader.remaining();
    Exact::read(reader, BLOCK_BITS);
    before[..before.len() - reader.remaining().len()].to_vec()
}

#[cfg(test)]
#[path = "directory_tests.rs"]
mod tests;
