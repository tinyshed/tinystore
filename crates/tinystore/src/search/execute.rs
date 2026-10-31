//! The best of a search's keys by block-max MaxScore, a window of
//! documents at a time, as Lucene's MaxScoreBulkScorer goes.
//!
//! In each window of 4,096 documents the lists are split by the most each
//! adds inside it: those whose maxima together cannot lift a document past
//! θ, the worst score among the best so far, are passive, and a window no
//! list can lift past θ is passed unread. The other lists add their
//! postings into an array one after another, and each document they touched
//! is completed from the passive lists, the one that adds most first, while
//! their bounds may still lift it past θ.
//!
//! A block whose bound cannot reach θ is never opened, yet the answer is
//! the one scoring every posting gives, [`every`], which the tests hold it
//! to: of equal scores the lower document comes first, and documents are
//! met in their order, so a document that could only tie may be passed.
//!
//! A filter's documents cross out of each window those it does not pass,
//! and a window that holds none of them is passed unread; a filter that
//! passes few is scored alone instead, [`best_of`], each of its documents
//! sought in every list.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;

use super::postings::{BLOCK, Posting};
use super::score::{Part, points};
use crate::Result;

/// Documents a window holds. Tests read windows of 512, so that an index of
/// a few thousand documents crosses many and their bounds skip lists.
const WINDOW: u32 = if cfg!(test) { 512 } else { 4096 };
/// Past every document.
pub(crate) const END: u32 = u32::MAX;
/// No key's id, since keys are numbered from 1: a list of postings in hand,
/// which never fetches a block by it.
const NO_KEY: i64 = 0;

/// A block of a list: its documents' range, the most its key adds to any of
/// them, and its postings when they are in hand already, a key's own row or
/// its head, rather than in a row of `blocks`.
#[derive(Debug)]
pub(crate) struct Block {
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) bound: u64,
    pub(crate) held: Option<Vec<Posting>>,
}

/// A key's postings in a search, and a cursor over them.
#[derive(Debug)]
pub(crate) struct List {
    pub(crate) key: i64,
    pub(crate) part: Part,
    /// The key's weight × idf.
    scale: f64,
    average: f64,
    blocks: Vec<Block>,
    block: usize,
    decoded: usize,
    postings: Vec<Posting>,
    at: usize,
}

/// What gives a list the postings of a block it does not hold: the
/// documents a set replaced left out, their lengths found.
pub(crate) trait Fetch {
    fn block(&mut self, list: &List, index: usize) -> Result<Vec<Posting>>;
}

impl List {
    /// A list of `blocks` in their documents' order, none empty.
    pub(crate) fn new(key: i64, part: Part, scale: f64, average: f64, blocks: Vec<Block>) -> List {
        List { key, part, scale, average, blocks, block: 0, decoded: usize::MAX, postings: Vec::new(), at: 0 }
    }

    /// A list of postings in hand, in their documents' order, that names no
    /// key: the words a prefix begins, joined. Its blocks hold as many as a
    /// stored block does, so that their bounds skip as theirs do.
    pub(crate) fn joined(part: Part, scale: f64, average: f64, postings: Vec<Posting>) -> List {
        let mut list = List::new(NO_KEY, part, scale, average, Vec::new());
        for postings in postings.chunks(BLOCK) {
            let block = list.in_hand(postings.to_vec());
            list.push(block);
        }
        list
    }

    /// Every posting the list holds, its stored blocks fetched, in its
    /// documents' order.
    pub(crate) fn into_postings(mut self, source: &mut dyn Fetch) -> Result<Vec<Posting>> {
        let mut postings = Vec::new();
        for block in 0..self.blocks.len() {
            self.block = block;
            self.open(source)?;
            postings.append(&mut self.postings);
        }
        Ok(postings)
    }

    /// A block of postings in hand, bounded by its own.
    pub(crate) fn in_hand(&self, postings: Vec<Posting>) -> Block {
        let bound = postings.iter().map(|posting| points(self.scale, posting.tf, posting.len, self.average)).max();
        let first = postings.first().map_or(0, |posting| posting.doc);
        let last = postings.last().map_or(0, |posting| posting.doc);
        Block { first, last, bound: bound.unwrap_or(0), held: Some(postings) }
    }

    /// A block of `blocks` as its directory gives it.
    pub(crate) fn stored(&self, first: u32, last: u32, most_tf: u32, least_len: u32) -> Block {
        Block { first, last, bound: points(self.scale, most_tf, least_len, self.average), held: None }
    }

    pub(crate) fn push(&mut self, block: Block) {
        self.blocks.push(block);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The cursor's document, the next block opened when this one is done.
    fn doc(&mut self, source: &mut dyn Fetch) -> Result<u32> {
        loop {
            if self.block >= self.blocks.len() {
                return Ok(END);
            }
            self.open(source)?;
            if let Some(posting) = self.postings.get(self.at) {
                return Ok(posting.doc);
            }
            self.block += 1;
            self.at = 0;
        }
    }

    fn open(&mut self, source: &mut dyn Fetch) -> Result<()> {
        if self.decoded == self.block {
            return Ok(());
        }
        self.postings = match self.blocks[self.block].held.take() {
            Some(postings) => postings,
            None => source.block(self, self.block)?,
        };
        self.decoded = self.block;
        Ok(())
    }

    fn next(&mut self) {
        self.at += 1;
    }

    /// Moves to the first posting at `doc` or after it, passing whole blocks
    /// before it unopened.
    fn seek(&mut self, doc: u32, source: &mut dyn Fetch) -> Result<u32> {
        while self.block < self.blocks.len() && self.blocks[self.block].last < doc {
            self.block += 1;
            self.at = 0;
        }
        if self.block >= self.blocks.len() {
            return Ok(END);
        }
        self.open(source)?;
        while self.postings.get(self.at).is_some_and(|posting| posting.doc < doc) {
            self.at += 1;
        }
        self.doc(source)
    }

    fn ahead(&self) -> &[Block] {
        &self.blocks[self.block.min(self.blocks.len())..]
    }

    /// The bound of the block that could hold `doc`, nothing when none could.
    fn bound_at(&self, doc: u32) -> u64 {
        self.block_at(doc).map_or(0, |block| block.bound)
    }

    fn block_at(&self, doc: u32) -> Option<&Block> {
        let ahead = self.ahead();
        let at = ahead.partition_point(|block| block.last < doc);
        ahead.get(at).filter(|block| block.first <= doc)
    }

    /// The most the key adds to any document in `[start, end)`.
    fn bound_over(&self, start: u32, end: u32) -> u64 {
        let ahead = self.ahead();
        let from = ahead.partition_point(|block| block.last < start);
        ahead[from..].iter().take_while(|block| block.first < end).map(|block| block.bound).max().unwrap_or(0)
    }

    /// A document no later than the key's first at `end` or after it, read
    /// from the blocks' ranges without opening one.
    fn next_from(&self, end: u32) -> u32 {
        let ahead = self.ahead();
        let from = ahead.partition_point(|block| block.last < end);
        ahead.get(from).map_or(END, |block| block.first.max(end))
    }

    fn points(&self) -> u64 {
        let posting = self.postings[self.at];
        points(self.scale, posting.tf, posting.len, self.average)
    }

    /// The index of the next stored block after `index` that the list holds
    /// in no row of its own, to read ahead with it.
    pub(crate) fn stored_after(&self, index: usize, most: usize) -> usize {
        let run = self.blocks[index + 1..].iter().take(most).take_while(|block| block.held.is_none()).count();
        index + run
    }

    pub(crate) fn first_of(&self, index: usize) -> u32 {
        self.blocks[index].first
    }

    /// Whether the cursor reached `index` from the block just before it.
    pub(crate) fn in_order(&self, index: usize) -> bool {
        index > 0 && self.decoded == index - 1
    }
}

/// A document's points, ordered by them and then the lower document first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scored(u64, Reverse<u32>);

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.0, self.1).cmp(&(other.0, other.1))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The best `k` and the worst of them, which a document must pass to enter.
struct Best {
    k: usize,
    heap: BinaryHeap<Reverse<Scored>>,
}

impl Best {
    fn full(&self) -> bool {
        self.heap.len() >= self.k
    }

    /// θ: what a document must pass, nothing while the best are not `k` yet.
    fn theta(&self) -> u64 {
        if self.full() { self.heap.peek().map_or(0, |Reverse(Scored(points, _))| *points) } else { 0 }
    }

    /// Takes `doc` in when it passes θ; documents come in their order, so
    /// one that ties θ comes after the one it ties and stays out.
    fn offer(&mut self, points: u64, doc: u32) {
        if !self.full() {
            self.heap.push(Reverse(Scored(points, Reverse(doc))));
        } else if points > self.theta() {
            self.heap.pop();
            self.heap.push(Reverse(Scored(points, Reverse(doc))));
        }
    }

    fn sorted(self) -> Vec<(u64, u32)> {
        let mut found: Vec<(u64, u32)> =
            self.heap.into_iter().map(|Reverse(Scored(points, Reverse(doc)))| (points, doc)).collect();
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        found
    }
}

/// The best `k` documents of every one the lists hold, or of those `passing`
/// holds, with their points, the best first.
pub(crate) fn best(
    lists: &mut [List],
    source: &mut dyn Fetch,
    k: usize,
    passing: Option<&[u32]>,
) -> Result<Vec<(u64, u32)>> {
    let best = Best { k, heap: BinaryHeap::with_capacity(k + 1) };
    let pass = passing.map(|docs| Pass { docs, at: 0 });
    let mut run = Run { lists, source, best, pass, points: Vec::new(), touched: Vec::new(), docs: Vec::new() };
    let mut start = run.start(0);
    while start != END && k > 0 {
        let end = start.saturating_add(WINDOW);
        run.window(start..end)?;
        start = run.start(end);
    }
    Ok(run.best.sorted())
}

/// The best `k` of `docs` alone, each sought in every list that could hold
/// it, the most it could add first, while their bounds may still lift it
/// past θ: a filter that passes few documents.
pub(crate) fn best_of(lists: &mut [List], source: &mut dyn Fetch, docs: &[u32], k: usize) -> Result<Vec<(u64, u32)>> {
    let mut best = Best { k, heap: BinaryHeap::with_capacity(k + 1) };
    let mut order: Vec<usize> = Vec::with_capacity(lists.len());
    for &doc in docs {
        order.clear();
        order.extend((0..lists.len()).filter(|at| lists[*at].block_at(doc).is_some()));
        order.sort_by_key(|at| Reverse(lists[*at].bound_at(doc)));
        let mut rest = order.iter().fold(0_u64, |sum, at| sum.saturating_add(lists[*at].bound_at(doc)));
        let (mut points, mut held) = (0_u64, false);
        for &at in &order {
            if best.full() && points.saturating_add(rest) <= best.theta() {
                break;
            }
            rest = rest.saturating_sub(lists[at].bound_at(doc));
            if lists[at].seek(doc, source)? == doc {
                points = points.saturating_add(lists[at].points());
                held = true;
            }
        }
        if held {
            best.offer(points, doc);
        }
    }
    Ok(best.sorted())
}

/// The documents a filter passes, met in their order.
struct Pass<'a> {
    docs: &'a [u32],
    at: usize,
}

impl Pass<'_> {
    /// The first passing document at `doc` or after it.
    fn from(&mut self, doc: u32) -> u32 {
        self.at += self.docs[self.at..].partition_point(|passing| *passing < doc);
        self.docs.get(self.at).copied().unwrap_or(END)
    }

    fn passes(&mut self, doc: u32) -> bool {
        self.from(doc) == doc
    }
}

/// A search under way: its lists, the best so far, the documents a filter
/// passes, and what the window being read holds.
struct Run<'a> {
    lists: &'a mut [List],
    source: &'a mut dyn Fetch,
    best: Best,
    pass: Option<Pass<'a>>,
    points: Vec<u64>,
    touched: Vec<bool>,
    docs: Vec<u32>,
}

/// A document no later than the first any list holds at `end` or after it.
fn next_from(lists: &[List], end: u32) -> u32 {
    lists.iter().map(|list| list.next_from(end)).min().unwrap_or(END)
}

impl Run<'_> {
    /// Where the next window starts at `from` or after it: at the next
    /// document a list may hold, and past the documents before the next the
    /// filter passes.
    fn start(&mut self, from: u32) -> u32 {
        let mut start = next_from(self.lists, from);
        let Some(pass) = &mut self.pass else { return start };
        while start != END {
            let passing = pass.from(start);
            if passing == END || passing < start.saturating_add(WINDOW) {
                return if passing == END { END } else { start };
            }
            start = next_from(self.lists, passing);
        }
        END
    }

    fn window(&mut self, documents: Range<u32>) -> Result<()> {
        if self.points.is_empty() {
            (self.points, self.touched) = (vec![0; WINDOW as usize], vec![false; WINDOW as usize]);
        }
        let bounds: Vec<u64> = self.lists.iter().map(|list| list.bound_over(documents.start, documents.end)).collect();
        let mut order: Vec<usize> = (0..self.lists.len()).collect();
        order.sort_by_key(|at| bounds[*at]);
        let passive = self.passive(&order, &bounds);
        if passive == order.len() {
            return Ok(());
        }
        for &at in &order[passive..] {
            self.add(at, &documents)?;
        }
        self.docs.sort_unstable();
        let rest = order[..passive].iter().fold(0_u64, |sum, at| sum.saturating_add(bounds[*at]));
        for doc in std::mem::take(&mut self.docs) {
            let slot = (doc - documents.start) as usize;
            let points = std::mem::take(&mut self.points[slot]);
            self.touched[slot] = false;
            if self.pass.as_mut().is_some_and(|pass| !pass.passes(doc)) {
                continue;
            }
            if self.best.full() && points.saturating_add(rest) <= self.best.theta() {
                continue;
            }
            let points = self.complete(&order[..passive], doc, points)?;
            self.best.offer(points, doc);
        }
        Ok(())
    }

    /// How many of the lists, least first, cannot together lift a document past θ.
    fn passive(&self, order: &[usize], bounds: &[u64]) -> usize {
        if !self.best.full() {
            return 0;
        }
        let theta = self.best.theta();
        let mut sum = 0_u64;
        order
            .iter()
            .take_while(|at| {
                sum = sum.saturating_add(bounds[**at]);
                sum <= theta
            })
            .count()
    }

    /// Adds every posting list `at` has in the window into its slot.
    fn add(&mut self, at: usize, documents: &Range<u32>) -> Result<()> {
        let list = &mut self.lists[at];
        let mut doc = list.seek(documents.start, self.source)?;
        while doc < documents.end {
            let slot = (doc - documents.start) as usize;
            if !self.touched[slot] {
                self.touched[slot] = true;
                self.docs.push(doc);
            }
            self.points[slot] = self.points[slot].saturating_add(list.points());
            list.next();
            doc = list.doc(self.source)?;
        }
        Ok(())
    }

    /// A document's points with what the passive lists add, the most first,
    /// asked while their bounds may still lift it past θ.
    fn complete(&mut self, passive: &[usize], doc: u32, mut points: u64) -> Result<u64> {
        let (full, theta) = (self.best.full(), self.best.theta());
        let mut rest = passive.iter().fold(0_u64, |sum, at| sum.saturating_add(self.lists[*at].bound_at(doc)));
        for &at in passive.iter().rev() {
            if full && points.saturating_add(rest) <= theta {
                break;
            }
            let bound = self.lists[at].bound_at(doc);
            rest = rest.saturating_sub(bound);
            if bound > 0 && self.lists[at].seek(doc, self.source)? == doc {
                points = points.saturating_add(self.lists[at].points());
            }
        }
        Ok(points)
    }
}

/// The best `k` by every posting of every list, of those `passing` holds
/// when a filter gives them: what [`best`] and [`best_of`] must answer.
#[cfg(test)]
pub(crate) fn every(
    lists: &mut [List],
    source: &mut dyn Fetch,
    k: usize,
    passing: Option<&[u32]>,
) -> Result<Vec<(u64, u32)>> {
    let mut scores: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for list in lists.iter_mut() {
        while list.doc(source)? != END {
            let doc = list.postings[list.at].doc;
            let total = scores.entry(doc).or_default();
            *total = total.saturating_add(list.points());
            list.next();
        }
    }
    let passes = |doc: &u32| passing.is_none_or(|docs| docs.binary_search(doc).is_ok());
    let mut all: Vec<(u64, u32)> =
        scores.into_iter().filter(|(doc, _)| passes(doc)).map(|(doc, points)| (points, doc)).collect();
    all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    all.truncate(k);
    Ok(all)
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;
