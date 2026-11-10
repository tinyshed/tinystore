//! A long piece's blocks as search.db keeps them since its migration 0002:
//! each bit-packed, a run of them in a row one after another, and a
//! directory of fixed width that says where each one is and bounds what it
//! adds.
//!
//! ```text
//! runs   (id)           → blocks one after another, a row taking more till it holds 32 KiB
//! pieces (segment, key) → directory: its header, then its blocks' entries
//!
//! header      version u8, block count u32, run count u32, then a run's (id i64, its first block u32)
//! entry       first u32, last u32, bound f32, average u16, at u16                       little-endian
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
//! A merge writes a piece once and only a purge takes from it, so its
//! directory is one row; a run of 32 KiB keeps a block's place in an u16.
//!
//! The directory's version says how its blocks and itself are laid out: a
//! layout after this one is a version of its own, which [`Directory::decode`]
//! tells apart, as `format-change` says.

use super::postings::{BLOCK, Posting, pack_inline, unpack_inline};
use super::score::saturated;
use crate::compress::bits::BitWriter;
use crate::{Error, Result};

/// The bytes a run takes blocks till it holds. A block takes 1,023 at
/// most, so every block starts below 32 KiB and its place fits an u16.
/// Tests take 1 KiB, so that a few thousand documents fill many runs.
pub(crate) const RUN_BYTES: usize = if cfg!(test) { 1 << 10 } else { 32 << 10 };

/// A directory's first byte.
const VERSION: u8 = 1;
const HEADER: usize = 3;
const RUN_ENTRY: usize = 12;
const BLOCK_ENTRY: usize = 16;

/// A block as its piece's directory gives it.
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

/// A run of a piece's blocks: its row, and the place of its first block
/// among the piece's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) id: i64,
    pub(crate) first_block: u32,
}

/// A piece's directory: its runs, the first holding block 0, and its blocks
/// in their documents' order.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Directory {
    pub(crate) runs: Vec<Run>,
    pub(crate) blocks: Vec<Placed>,
}

impl Directory {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(9 + self.runs.len() * RUN_ENTRY + self.blocks.len() * BLOCK_ENTRY);
        out.push(VERSION);
        out.extend_from_slice(&(self.blocks.len() as u32).to_le_bytes());
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

    /// A directory of its bytes, checked.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Directory> {
        let broken = |invariant: &str| Error::corrupt(format!("a piece's directory: {invariant}"));
        let Some((&VERSION, bytes)) = bytes.split_first() else { return Err(broken("its version")) };
        let (blocks, bytes) = bytes.split_first_chunk::<4>().ok_or_else(|| broken("no count of blocks"))?;
        let (runs, bytes) = bytes.split_first_chunk::<4>().ok_or_else(|| broken("no count of runs"))?;
        let (blocks, runs) = (u32::from_le_bytes(*blocks) as usize, u32::from_le_bytes(*runs) as usize);
        let runs_end = runs.checked_mul(RUN_ENTRY).filter(|end| *end <= bytes.len());
        let (runs, entries) = bytes.split_at(runs_end.ok_or_else(|| broken("its runs cut short"))?);
        let (entries, tail) = entries.as_chunks::<BLOCK_ENTRY>();
        if !tail.is_empty() || entries.len() != blocks {
            return Err(broken("its entries are not its count's"));
        }
        let runs = runs
            .as_chunks::<RUN_ENTRY>()
            .0
            .iter()
            .map(|entry| Run {
                id: i64::from_le_bytes(word(entry, 0)),
                first_block: u32::from_le_bytes(word(entry, 8)),
            })
            .collect();
        let blocks = entries
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

    /// Where block `index`'s bytes lie in its run, the run's length
    /// `run_len`: from its place to the next block's or the run's end. A
    /// directory decodes without its runs, so a place past one is refused
    /// here, where every reader of a block's bytes asks for them.
    pub(crate) fn bytes_of(&self, index: usize, run_len: usize) -> Result<std::ops::Range<usize>> {
        let start = usize::from(self.blocks[index].at);
        let run = self.run_of(index);
        let next_in_run = self.runs.get(run + 1).is_none_or(|next| next.first_block as usize > index + 1);
        let end = match self.blocks.get(index + 1) {
            Some(next) if next_in_run => usize::from(next.at),
            _ => run_len,
        };
        if start >= end || end > run_len {
            let broken = Error::corrupt("a piece's directory: a block placed past the next or past its run's end");
            return Err(broken.within(format!("run {}", self.runs[run].id)));
        }
        Ok(start..end)
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
/// `average`, the earlier document first of equals. Each posting's
/// saturation is reckoned once, and a fresh one below the tenth best is
/// passed over unsorted: a merge calls this for every block it packs.
pub(crate) fn best_of(best: Vec<Posting>, fresh: &[Posting], average: u16) -> Vec<Posting> {
    let saturates = |posting: &Posting| saturated(posting.tf, posting.len, f64::from(average));
    let mut scored: Vec<(f64, Posting)> = best.iter().map(|posting| (saturates(posting), *posting)).collect();
    let least = if scored.len() >= BEST { scored.iter().map(|(at, _)| *at).fold(f64::INFINITY, f64::min) } else { 0.0 };
    scored.extend(fresh.iter().map(|posting| (saturates(posting), *posting)).filter(|(at, _)| *at >= least));
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.doc.cmp(&b.1.doc)));
    scored.truncate(BEST);
    scored.into_iter().map(|(_, posting)| posting).collect()
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

/// A block's postings, its first document `first`, onto `out`, which is
/// cleared first: their documents and frequencies, their lengths 0.
pub(crate) fn unpack(bytes: &[u8], first: u32, out: &mut Vec<Posting>) -> Result<()> {
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
    let (mut doc, mut at) = (u64::from(first), 0);
    out.push(Posting { doc: first, tf: 1, len: 0 });
    out.extend((1..count).map(|_| {
        doc += u64::from(bits_at(body, at, gap_width)) + 1;
        at += gap_width as usize;
        Posting { doc: doc as u32, tf: 1, len: 0 }
    }));
    if doc > u64::from(u32::MAX) {
        return Err(broken("a document past 32 bits"));
    }
    // a frequency of 1 each, most rare words' blocks, takes no bits
    if tf_width > 0 {
        for posting in out.iter_mut() {
            posting.tf = bits_at(body, at, tf_width).wrapping_add(1);
            at += tf_width as usize;
        }
    }
    if tf_width == 32 && out.iter().any(|posting| posting.tf == 0) {
        return Err(broken("a frequency past 32 bits"));
    }
    Ok(())
}

/// The `width` bits, 32 at most, at bit `at` of `body`: one load of the
/// eight bytes there, a shift and a mask, rather than a reader's refill a
/// value, which took a search's blocks two and a half times as long to read.
#[inline]
fn bits_at(body: &[u8], at: usize, width: u32) -> u32 {
    let from = &body[(at / 8).min(body.len())..];
    let word = match from.first_chunk::<8>() {
        Some(word) => u64::from_le_bytes(*word),
        None => from.iter().rev().fold(0, |word, byte| word << 8 | u64::from(*byte)),
    };
    ((word >> (at % 8)) & ((1_u64 << width) - 1)) as u32
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
            let expected: Vec<Posting> = written.iter().map(|posting| Posting { len: 0, ..*posting }).collect();
            assert_eq!(out, expected, "seed {seed}");
        }
        let widest = [Posting { doc: 1, tf: u32::MAX, len: 1 }, Posting { doc: u32::MAX, tf: 1, len: 1 }];
        let mut bytes = Vec::new();
        pack(&widest, &mut bytes);
        unpack(&bytes, 1, &mut out).unwrap();
        assert_eq!(out, [Posting { doc: 1, tf: u32::MAX, len: 0 }, Posting { doc: u32::MAX, tf: 1, len: 0 }]);
    }

    #[test]
    fn a_directory_reads_back_as_written_and_says_where_each_block_lies() {
        let placed = |first, last, at| Placed { first, last, bound: 1.5, average: 40, at };
        let directory = Directory {
            runs: vec![Run { id: 7, first_block: 0 }, Run { id: 9, first_block: 2 }],
            blocks: vec![placed(1, 90, 0), placed(95, 300, 120), placed(301, 302, 0)],
        };
        let read = Directory::decode(&directory.encode()).unwrap();
        assert_eq!(read, directory);
        assert_eq!((read.run_of(0), read.run_of(1), read.run_of(2)), (0, 0, 1));
        assert_eq!(read.bytes_of(0, 200).unwrap(), 0..120);
        assert_eq!(read.bytes_of(1, 200).unwrap(), 120..200);
        assert_eq!(read.bytes_of(2, 31).unwrap(), 0..31);
        assert_eq!(Directory::decode(&Directory::default().encode()).unwrap(), Directory::default());
        let mut later = directory.encode();
        later[0] = VERSION + 1;
        assert_eq!(Directory::decode(&later).unwrap_err().to_string(), "a piece's directory: its version: corrupt");
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
            "a piece's directory: its blocks out of order: corrupt"
        );
        let mut short = backwards.encode();
        short.truncate(short.len() - 1);
        assert_eq!(
            broken(Directory::decode(&short).unwrap_err()),
            "a piece's directory: its entries are not its count's: corrupt"
        );
    }

    // contract: ERR-04
    #[test]
    fn a_block_placed_past_its_run_or_the_next_says_so_and_names_its_run() {
        let placed = |first, last, at| Placed { first, last, bound: 1.0, average: 3, at };
        let directory = Directory {
            runs: vec![Run { id: 7, first_block: 0 }, Run { id: 9, first_block: 2 }],
            blocks: vec![placed(1, 90, 0), placed(95, 300, 500), placed(301, 302, 40)],
        };
        let misplaced = "run 7: a piece's directory: a block placed past the next or past its run's end: corrupt";
        let lying = |index, run_len| directory.bytes_of(index, run_len).map_err(|error| error.to_string());
        assert_eq!(lying(0, 200), Err(misplaced.to_owned()), "the next block's place past the run");
        assert_eq!(lying(1, 200), Err(misplaced.to_owned()), "its own place past the run");
        assert_eq!(lying(0, 600), Ok(0..500));
        assert_eq!(lying(2, 40), Err(misplaced.replace("run 7", "run 9")), "its place at the run's end");
        assert_eq!(lying(2, 41), Ok(40..41));
        let backwards =
            Directory { blocks: vec![placed(1, 90, 30), placed(95, 300, 10), placed(301, 302, 0)], ..directory };
        assert_eq!(backwards.bytes_of(0, 200).unwrap_err().to_string(), misplaced, "the next block placed before it");
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
