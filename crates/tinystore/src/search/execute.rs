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

use super::packed::Placed;
use super::postings::{BLOCK, Posting};
use super::score::{Part, bound, points};
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
#[derive(Clone, Debug)]
pub(crate) struct Block {
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) bound: u64,
    pub(crate) held: Option<Vec<Posting>>,
}

/// A key's postings in a search, and a cursor over them. A clone reads from
/// where the list stands, apart from it.
#[derive(Clone, Debug)]
pub(crate) struct List {
    pub(crate) key: i64,
    pub(crate) part: Part,
    /// The key's weight × idf.
    scale: f64,
    average: f64,
    blocks: Vec<Block>,
    /// Documents it holds and the points it adds to each, known before a
    /// search reads a block: a key's best postings.
    seeds: Vec<(u32, u64)>,
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
        let seeds = Vec::new();
        List { key, part, scale, average, blocks, seeds, block: 0, decoded: usize::MAX, postings: Vec::new(), at: 0 }
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

    /// A block bounded by its largest frequency over its shortest length,
    /// which no posting of it passes, as a list a release before 0002 wrote
    /// bounds its blocks.
    pub(crate) fn stored(&self, first: u32, last: u32, most_tf: u32, least_len: u32) -> Block {
        Block { first, last, bound: points(self.scale, most_tf, least_len, self.average), held: None }
    }

    /// A block of a run as its directory gives it.
    pub(crate) fn packed(&self, block: &Placed) -> Block {
        let bound = bound(self.scale, block.bound, block.average, self.average);
        Block { first: block.first, last: block.last, bound, held: None }
    }

    /// Postings of documents it holds, scored before any block is read.
    pub(crate) fn seed<'p>(&mut self, postings: impl Iterator<Item = &'p Posting>) {
        let (scale, average) = (self.scale, self.average);
        self.seeds.extend(postings.map(|posting| (posting.doc, points(scale, posting.tf, posting.len, average))));
    }

    pub(crate) fn push(&mut self, block: Block) {
        self.blocks.push(block);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The most the key adds to any document.
    pub(crate) fn most(&self) -> u64 {
        self.blocks.iter().map(|block| block.bound).max().unwrap_or(0)
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

    /// The decoded block's postings from the cursor to the first at `end` or
    /// after it, the cursor moved past them.
    fn take_before(&mut self, end: u32) -> &[Posting] {
        let from = self.at.min(self.postings.len());
        let taken = self.postings[from..].partition_point(|posting| posting.doc < end);
        self.at = from + taken;
        &self.postings[from..self.at]
    }

    /// The place of the first block that could hold `doc` or a later one,
    /// for [`List::bound_walking`] to walk on from.
    fn walk_from(&self, doc: u32) -> usize {
        let from = self.block.min(self.blocks.len());
        from + self.blocks[from..].partition_point(|block| block.last < doc)
    }

    /// The bound of the block that could hold `doc`, nothing when none
    /// could, `walk` stepped on to it from the block that could hold an
    /// earlier document: documents of a window are completed in their order,
    /// so the blocks are passed once rather than searched for each.
    fn bound_walking(&self, walk: &mut usize, doc: u32) -> u64 {
        while self.blocks.get(*walk).is_some_and(|block| block.last < doc) {
            *walk += 1;
        }
        self.blocks.get(*walk).filter(|block| block.first <= doc).map_or(0, |block| block.bound)
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

    pub(crate) fn first_of(&self, index: usize) -> u32 {
        self.blocks[index].first
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

/// The best `k` and the worst of them, which a document must pass to enter;
/// and a floor no document of the best `k` is at or below, known before the
/// search begins.
struct Best {
    k: usize,
    heap: BinaryHeap<Reverse<Scored>>,
    floor: u64,
}

impl Best {
    fn new(k: usize, floor: u64) -> Best {
        Best { k, heap: BinaryHeap::with_capacity(k + 1), floor }
    }

    fn full(&self) -> bool {
        self.heap.len() >= self.k
    }

    /// Whether θ already passes documents by: the best are `k`, or a floor is known.
    fn pruning(&self) -> bool {
        self.full() || self.floor > 0
    }

    /// θ: what a document must pass, the floor while the best are not `k` yet.
    fn theta(&self) -> u64 {
        let worst = if self.full() { self.heap.peek().map_or(0, |Reverse(Scored(points, _))| *points) } else { 0 };
        worst.max(self.floor)
    }

    /// Takes `doc` in when it passes θ; documents come in their order, so
    /// one that ties θ comes after the one it ties and stays out.
    fn offer(&mut self, points: u64, doc: u32) {
        if points <= self.floor {
            return;
        }
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

/// One below the `k`th most that the lists' seeds add to a document, of
/// those `passing` holds: `k` documents score at least that much, so no
/// document of the best `k` scores less. Nothing when they seed fewer.
/// search-fast opened 6 % fewer blocks on MS MARCO with a key's ten best.
fn floor(lists: &[List], k: usize, passing: Option<&[u32]>) -> u64 {
    #[cfg(test)]
    if !SEEDED.get() {
        return 0;
    }
    let mut seeded: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    for list in lists {
        for &(doc, points) in &list.seeds {
            if passing.is_none_or(|docs| docs.binary_search(&doc).is_ok()) {
                let sum = seeded.entry(doc).or_default();
                *sum = sum.saturating_add(points);
            }
        }
    }
    if k == 0 || seeded.len() < k {
        return 0;
    }
    let mut sums: Vec<u64> = seeded.into_values().collect();
    let at = sums.len() - k;
    sums.select_nth_unstable(at).1.saturating_sub(1)
}

#[cfg(test)]
thread_local! {
    /// Whether this thread's searches start from their keys' best, for a timing.
    pub(crate) static SEEDED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// The best `k` documents of every one the lists hold, or of those `passing`
/// holds, with their points, the best first.
pub(crate) fn best(
    lists: &mut [List],
    source: &mut dyn Fetch,
    k: usize,
    passing: Option<&[u32]>,
) -> Result<Vec<(u64, u32)>> {
    let best = Best::new(k, floor(lists, k, passing));
    let pass = passing.map(|docs| Pass { docs, at: 0 });
    let mut run = Run {
        lists,
        source,
        best,
        pass,
        points: Vec::new(),
        touched: Vec::new(),
        docs: Vec::new(),
        walks: Vec::new(),
        bounds: Vec::new(),
    };
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
    let mut best = Best::new(k, 0);
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

/// The documents that hold a word for each of `words`, each the places in
/// `lists` of the lists that find one word, of those `passing` holds when a
/// filter gives them, in their order: the documents of the word whose lists
/// hold the fewest blocks, each sought in the lists of every other word. A
/// word no list finds leaves none.
pub(crate) fn held_by_all(
    lists: &[List],
    words: &[Vec<usize>],
    source: &mut dyn Fetch,
    passing: Option<&[u32]>,
) -> Result<Vec<u32>> {
    let blocks = |word: &&Vec<usize>| word.iter().map(|at| lists[*at].blocks.len()).sum::<usize>();
    let Some(fewest) = words.iter().min_by_key(blocks) else { return Ok(Vec::new()) };
    let mut held = Vec::new();
    for &at in fewest {
        let mut list = lists[at].clone();
        while list.doc(source)? != END {
            held.push(list.postings[list.at].doc);
            list.next();
        }
    }
    held.sort_unstable();
    held.dedup();
    held.retain(|doc| passing.is_none_or(|docs| docs.binary_search(doc).is_ok()));
    for word in words.iter().filter(|word| !std::ptr::eq(*word, fewest)) {
        let mut finding: Vec<List> = word.iter().map(|at| lists[*at].clone()).collect();
        let mut kept = Vec::with_capacity(held.len());
        for doc in held {
            if any_holds(&mut finding, doc, source)? {
                kept.push(doc);
            }
        }
        held = kept;
    }
    Ok(held)
}

/// Whether any of `lists` holds `doc`, each moved to it or past it.
fn any_holds(lists: &mut [List], doc: u32, source: &mut dyn Fetch) -> Result<bool> {
    for list in lists {
        if list.seek(doc, source)? == doc {
            return Ok(true);
        }
    }
    Ok(false)
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
    /// Where each passive list's walk stands, in the order of the passive lists.
    walks: Vec<usize>,
    /// Each passive list's bound at the document being completed.
    bounds: Vec<u64>,
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
        self.walks.clear();
        self.walks.extend(order[..passive].iter().map(|at| self.lists[*at].walk_from(documents.start)));
        for doc in std::mem::take(&mut self.docs) {
            let slot = (doc - documents.start) as usize;
            let points = std::mem::take(&mut self.points[slot]);
            self.touched[slot] = false;
            if self.pass.as_mut().is_some_and(|pass| !pass.passes(doc)) {
                continue;
            }
            if self.best.pruning() && points.saturating_add(rest) <= self.best.theta() {
                continue;
            }
            let points = self.complete(&order[..passive], doc, points)?;
            self.best.offer(points, doc);
        }
        Ok(())
    }

    /// How many of the lists, least first, cannot together lift a document past θ.
    fn passive(&self, order: &[usize], bounds: &[u64]) -> usize {
        if !self.best.pruning() {
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

    /// Adds every posting list `at` has in the window into its slot, a
    /// decoded block's at a time.
    fn add(&mut self, at: usize, documents: &Range<u32>) -> Result<()> {
        let Run { lists, source, points: sums, touched, docs, .. } = self;
        let list = &mut lists[at];
        let mut doc = list.seek(documents.start, *source)?;
        while doc < documents.end {
            let (scale, average) = (list.scale, list.average);
            for posting in list.take_before(documents.end) {
                let slot = (posting.doc - documents.start) as usize;
                if !touched[slot] {
                    touched[slot] = true;
                    docs.push(posting.doc);
                }
                sums[slot] = sums[slot].saturating_add(points(scale, posting.tf, posting.len, average));
            }
            doc = list.doc(*source)?;
        }
        Ok(())
    }

    /// A document's points with what the passive lists add, the most first,
    /// asked while their bounds may still lift it past θ.
    fn complete(&mut self, passive: &[usize], doc: u32, mut points: u64) -> Result<u64> {
        let (full, theta) = (self.best.pruning(), self.best.theta());
        self.bounds.clear();
        for (walk, at) in self.walks.iter_mut().zip(passive) {
            self.bounds.push(self.lists[*at].bound_walking(walk, doc));
        }
        let mut rest = self.bounds.iter().fold(0_u64, |sum, bound| sum.saturating_add(*bound));
        for (place, &at) in passive.iter().enumerate().rev() {
            if full && points.saturating_add(rest) <= theta {
                break;
            }
            let bound = self.bounds[place];
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
