//! A key's blocks as search.db keeps them since its migration 0002: each
//! bit-packed, a run of them in a row one after another, and a directory of
//! fixed width that says where each one is and bounds what it adds.
//!
//! ```text
//! runs  (id)  → blocks one after another, a row taking more till it holds 32 KiB
//! lists (key) → packed: the key's directory
//!
//! directory   version u8, run count u32, then a run's (id i64, its first block u32),
//!             then a block's (first u32, last u32, bound f32, average u16, at u16)     little-endian
//! block       count − 1 u8, gap width u8, tf width u8, then in bits, the lowest first,
//!             doc − doc before − 1 for each posting after the first, tf − 1 for each
//! ```
//!
//! A block's bound is the most BM25 saturates any of its postings at
//! `average`, the part's average length rounded when the block was made,
//! itself rounded up to an f32. At a search's average A no posting
//! saturates more than bound × max(1, A / average): a longer average divides
//! each term of BM25's denominator by at most that much, and a shorter one
//! only grows the denominator. So a search bounds a block from its directory
//! alone, closer than the largest frequency over the shortest length would,
//! which may be two postings.
//!
//! search-fast measured this layout at 1.99 bytes a posting on MS MARCO
//! against varint blocks' 3.18, and reading it, a block at its place through
//! one blob handle rather than a statement a block, took 41 % of the median.
//! A run rather than one row a key keeps a pack's append to the last run's
//! bytes, where one row would be written again whole at every append.
//!
//! The directory's version says how its blocks and itself are laid out: a
//! layout after this one is a version of its own, which [`Directory::decode`]
//! tells apart, as `format-change` says.

use super::postings::{BLOCK, Posting, pack_inline, unpack_inline};
use super::score::saturated;
use crate::compress::bits::{BitReader, BitWriter};
use crate::{Error, Result};

/// The bytes a run takes blocks till it holds. A block takes 1,023 at
/// most, so every block starts below 32 KiB and its place fits an u16.
pub(crate) const RUN_BYTES: usize = 32 << 10;

/// A directory's first byte.
const VERSION: u8 = 1;
const HEADER: usize = 3;
const RUN_ENTRY: usize = 12;
const BLOCK_ENTRY: usize = 16;

/// A block as its key's directory gives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placed {
    pub(crate) first: u32,
    pub(crate) last: u32,
    /// The most any of its postings saturates at `average`, rounded up.
    pub(crate) bound: f32,
    pub(crate) average: u16,
    /// Where in its run its bytes start.
    pub(crate) at: u16,
}

/// A run of a key's blocks: its row, and the place of its first block among
/// the key's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) id: i64,
    pub(crate) first_block: u32,
}

/// A key's directory: its runs, the first holding block 0, and its blocks
/// in their documents' order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Directory {
    pub(crate) runs: Vec<Run>,
    pub(crate) blocks: Vec<Placed>,
}

impl Directory {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.runs.len() * RUN_ENTRY + self.blocks.len() * BLOCK_ENTRY);
        out.push(VERSION);
        out.extend_from_slice(&(self.runs.len() as u32).to_le_bytes());
        for run in &self.runs {
            out.extend_from_slice(&run.id.to_le_bytes());
            out.extend_from_slice(&run.first_block.to_le_bytes());
        }
        for block in &self.blocks {
            out.extend_from_slice(&block.first.to_le_bytes());
            out.extend_from_slice(&block.last.to_le_bytes());
            out.extend_from_slice(&block.bound.to_le_bytes());
            out.extend_from_slice(&block.average.to_le_bytes());
            out.extend_from_slice(&block.at.to_le_bytes());
        }
        out
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Directory> {
        let broken = |invariant: &str| Error::corrupt(format!("a key's directory: {invariant}"));
        let Some((&VERSION, bytes)) = bytes.split_first() else { return Err(broken("its version")) };
        let (count, rest) = bytes.split_first_chunk::<4>().ok_or_else(|| broken("no count of runs"))?;
        let count = u32::from_le_bytes(*count) as usize;
        let runs_end = count.checked_mul(RUN_ENTRY).filter(|end| *end <= rest.len());
        let (runs, blocks) = rest.split_at(runs_end.ok_or_else(|| broken("runs past its end"))?);
        let (blocks, tail) = blocks.as_chunks::<BLOCK_ENTRY>();
        if !tail.is_empty() {
            return Err(broken("a block's entry cut short"));
        }
        let runs: Vec<Run> = runs
            .as_chunks::<RUN_ENTRY>()
            .0
            .iter()
            .map(|entry| Run {
                id: i64::from_le_bytes(word(entry, 0)),
                first_block: u32::from_le_bytes(word(entry, 8)),
            })
            .collect();
        let blocks: Vec<Placed> = blocks
            .iter()
            .map(|entry| Placed {
                first: u32::from_le_bytes(word(entry, 0)),
                last: u32::from_le_bytes(word(entry, 4)),
                bound: f32::from_le_bytes(word(entry, 8)),
                average: u16::from_le_bytes(word(entry, 12)),
                at: u16::from_le_bytes(word(entry, 14)),
            })
            .collect();
        let directory = Directory { runs, blocks };
        directory.check().map_err(broken)?;
        Ok(directory)
    }

    /// Which invariant of a directory its bytes break, if any.
    fn check(&self) -> std::result::Result<(), &'static str> {
        if self.runs.first().is_some_and(|run| run.first_block != 0) || self.runs.is_empty() != self.blocks.is_empty() {
            return Err("its first run does not hold its first block");
        }
        if self.runs.windows(2).any(|pair| pair[0].first_block >= pair[1].first_block)
            || self.runs.last().is_some_and(|run| run.first_block as usize >= self.blocks.len())
        {
            return Err("a run holds no block");
        }
        if self
            .blocks
            .iter()
            .any(|block| block.first > block.last || block.average == 0 || block.bound.is_nan() || block.bound < 0.0)
        {
            return Err("a block's range or bound out of bounds");
        }
        if self.blocks.windows(2).any(|pair| pair[0].last >= pair[1].first) {
            return Err("its blocks out of order");
        }
        Ok(())
    }

    /// The places of the blocks the run at `place` holds.
    pub(crate) fn blocks_of(&self, place: usize) -> std::ops::Range<usize> {
        let end = self.runs.get(place + 1).map_or(self.blocks.len(), |next| next.first_block as usize);
        self.runs[place].first_block as usize..end
    }

    /// The place among the runs of the run that holds block `index`.
    pub(crate) fn run_of(&self, index: usize) -> usize {
        self.runs.partition_point(|run| run.first_block as usize <= index) - 1
    }

    /// Where block `index`'s bytes lie in its run, the run's length `run_len`.
    pub(crate) fn bytes_of(&self, index: usize, run_len: usize) -> std::ops::Range<usize> {
        let start = usize::from(self.blocks[index].at);
        let next_in_run = self.runs.get(self.run_of(index) + 1).is_none_or(|run| run.first_block as usize > index + 1);
        let end = match self.blocks.get(index + 1) {
            Some(next) if next_in_run => usize::from(next.at),
            _ => run_len,
        };
        start..end.max(start)
    }
}

fn word<const N: usize>(entry: &[u8], at: usize) -> [u8; N] {
    let mut bytes = [0; N];
    bytes.copy_from_slice(&entry[at..at + N]);
    bytes
}

/// The average a new block is bounded at: the part's, rounded, within an u16.
pub(crate) fn reference(average: f64) -> u16 {
    average.round().clamp(1.0, f64::from(u16::MAX)) as u16
}

/// The most any of `block`'s postings saturates at `average`, rounded up.
pub(crate) fn bound_of(block: &[Posting], average: u16) -> f32 {
    let most = block.iter().map(|posting| saturated(posting.tf, posting.len, f64::from(average))).fold(0.0, f64::max);
    up(most)
}

/// Postings of a key it keeps as its best, which a search scores first.
pub(crate) const BEST: usize = 10;
/// Blocks a key holds before it keeps its best. Over 200,000 passages of 50
/// words, ten of every key's in runs at twelve bytes each took 0.7 bytes a
/// posting against 1.8 for all else; as varints from two blocks on, 0.02.
pub(crate) const BEST_AT: usize = 2;

/// `best` with `fresh` among them, the `BEST` that saturate most at
/// `average`, the earlier document first of equals.
pub(crate) fn best_of(mut best: Vec<Posting>, fresh: &[Posting], average: u16) -> Vec<Posting> {
    best.extend_from_slice(fresh);
    let saturates = |posting: &Posting| saturated(posting.tf, posting.len, f64::from(average));
    best.sort_by(|a, b| saturates(b).total_cmp(&saturates(a)).then(a.doc.cmp(&b.doc)));
    best.truncate(BEST);
    best
}

/// A key's best in their documents' order, as a short list keeps its
/// postings: (doc − doc before, tf, len) varints.
pub(crate) fn encode_best(best: &[Posting]) -> Vec<u8> {
    let mut ordered = best.to_vec();
    ordered.sort_unstable_by_key(|posting| posting.doc);
    pack_inline(&ordered)
}

pub(crate) fn decode_best(bytes: &[u8]) -> Result<Vec<Posting>> {
    let best = unpack_inline(bytes)?;
    if best.len() > BEST {
        return Err(Error::corrupt("a key's best postings: more than ten"));
    }
    Ok(best)
}

/// The least f32 at `value` or above it.
fn up(value: f64) -> f32 {
    let near = value as f32;
    if f64::from(near) >= value { near } else { near.next_up() }
}

/// The width in bits of the largest of `values`.
fn width(values: impl Iterator<Item = u32>) -> u32 {
    values.max().map_or(0, |most| u32::BITS - most.leading_zeros())
}

/// A block's documents and frequencies, its first document left to its
/// directory, appended to `out`.
pub(crate) fn pack(block: &[Posting], out: &mut Vec<u8>) {
    debug_assert!(!block.is_empty() && block.len() <= BLOCK, "a block of 1 to {BLOCK} postings");
    let gaps = || block.windows(2).map(|pair| pair[1].doc - pair[0].doc - 1);
    let tfs = || block.iter().map(|posting| posting.tf - 1);
    let (gap_width, tf_width) = (width(gaps()), width(tfs()));
    out.extend_from_slice(&[(block.len() - 1) as u8, gap_width as u8, tf_width as u8]);
    let mut writer = BitWriter::onto(std::mem::take(out));
    gaps().for_each(|gap| writer.write(u64::from(gap), gap_width));
    tfs().for_each(|tf| writer.write(u64::from(tf), tf_width));
    *out = writer.finish();
}

/// A block's documents and frequencies, its first document `first`, onto
/// `out`, which is cleared first.
pub(crate) fn unpack(bytes: &[u8], first: u32, out: &mut Vec<(u32, u32)>) -> Result<()> {
    out.clear();
    let broken = |invariant: &str| Error::corrupt(format!("a packed block: {invariant}"));
    let [count, gap_width, tf_width] = *bytes.first_chunk::<HEADER>().ok_or_else(|| broken("no header"))?;
    let (count, gap_width, tf_width) = (usize::from(count) + 1, u32::from(gap_width), u32::from(tf_width));
    if count > BLOCK || gap_width > 32 || tf_width > 32 {
        return Err(broken("a count or a width out of bounds"));
    }
    let bits = (count - 1) * gap_width as usize + count * tf_width as usize;
    let body = &bytes[HEADER..];
    if body.len() != bits.div_ceil(8) {
        return Err(broken("its bytes are not its count's at its widths"));
    }
    let mut reader = BitReader::of(body);
    let mut doc = first;
    out.push((first, 0));
    for _ in 1..count {
        let step = (reader.read(gap_width) as u32).checked_add(1);
        doc = step.and_then(|step| doc.checked_add(step)).ok_or_else(|| broken("a document past 32 bits"))?;
        out.push((doc, 0));
    }
    for posting in out.iter_mut() {
        let tf = (reader.read(tf_width) as u32).checked_add(1);
        posting.1 = tf.ok_or_else(|| broken("a frequency past 32 bits"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::fixture::Random;

    fn block(random: &mut Random, count: usize) -> Vec<Posting> {
        let mut doc = 1 + random.below(1000) as u32;
        let widest = 1 << random.below(20);
        let spread = 1 + random.below(widest) as u32;
        (0..count)
            .map(|at| {
                if at > 0 {
                    doc += 1 + random.below(u64::from(spread)) as u32;
                }
                let widest = 1 << random.below(12);
                let tf = 1 + random.skewed(widest) as u32;
                Posting { doc, tf, len: 1 + random.below(500) as u32 }
            })
            .collect()
    }

    #[test]
    fn a_block_reads_back_as_written_at_any_widths() {
        let mut out = Vec::new();
        for seed in 0..500 {
            let mut random = Random::new(seed);
            let count = 1 + random.below(BLOCK as u64) as usize;
            let written = block(&mut random, count);
            let mut bytes = Vec::new();
            pack(&written, &mut bytes);
            unpack(&bytes, written[0].doc, &mut out).unwrap();
            let expected: Vec<(u32, u32)> = written.iter().map(|posting| (posting.doc, posting.tf)).collect();
            assert_eq!(out, expected, "seed {seed}");
        }
        let widest = [Posting { doc: 1, tf: u32::MAX, len: 1 }, Posting { doc: u32::MAX, tf: 1, len: 1 }];
        let mut bytes = Vec::new();
        pack(&widest, &mut bytes);
        unpack(&bytes, 1, &mut out).unwrap();
        assert_eq!(out, [(1, u32::MAX), (u32::MAX, 1)]);
    }

    #[test]
    fn a_directory_reads_back_and_says_where_each_block_lies() {
        let placed = |first, last, at| Placed { first, last, bound: 1.5, average: 40, at };
        let directory = Directory {
            runs: vec![Run { id: 7, first_block: 0 }, Run { id: 9, first_block: 2 }],
            blocks: vec![placed(1, 90, 0), placed(95, 300, 120), placed(301, 302, 0)],
        };
        let read = Directory::decode(&directory.encode()).unwrap();
        assert_eq!(read, directory);
        assert_eq!((read.run_of(0), read.run_of(1), read.run_of(2)), (0, 0, 1));
        assert_eq!(read.bytes_of(0, 200), 0..120);
        assert_eq!(read.bytes_of(1, 200), 120..200);
        assert_eq!(read.bytes_of(2, 31), 0..31);
        assert_eq!(Directory::decode(&Directory::default().encode()).unwrap(), Directory::default());
        let mut later = directory.encode();
        later[0] = VERSION + 1;
        assert_eq!(Directory::decode(&later).unwrap_err().to_string(), "a key's directory: its version: corrupt");
    }

    #[test]
    fn bytes_that_break_an_invariant_say_which() {
        let broken = |error: Error| error.to_string();
        let mut bytes = Vec::new();
        pack(&[Posting { doc: 3, tf: 2, len: 1 }, Posting { doc: 9, tf: 1, len: 1 }], &mut bytes);
        bytes.push(0);
        let mut out = Vec::new();
        assert_eq!(
            broken(unpack(&bytes, 3, &mut out).unwrap_err()),
            "a packed block: its bytes are not its count's at its widths: corrupt"
        );
        assert_eq!(
            broken(unpack(&[0, 0, 40], 3, &mut out).unwrap_err()),
            "a packed block: a count or a width out of bounds: corrupt"
        );
        let backwards = Directory {
            runs: vec![Run { id: 1, first_block: 0 }],
            blocks: vec![
                Placed { first: 50, last: 60, bound: 1.0, average: 3, at: 0 },
                Placed { first: 10, last: 20, bound: 1.0, average: 3, at: 9 },
            ],
        };
        assert_eq!(
            broken(Directory::decode(&backwards.encode()).unwrap_err()),
            "a key's directory: its blocks out of order: corrupt"
        );
    }

    #[test]
    fn a_bound_is_never_below_what_a_posting_of_its_block_saturates() {
        for seed in 0..300 {
            let mut random = Random::new(seed);
            let count = 1 + random.below(BLOCK as u64) as usize;
            let written = block(&mut random, count);
            let average = reference(1.0 + random.below(400) as f64);
            let bound = f64::from(bound_of(&written, average));
            assert!(written.iter().all(|posting| saturated(posting.tf, posting.len, f64::from(average)) <= bound));
        }
    }
}
