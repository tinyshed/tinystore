//! The buckets a group's page met samples in, which its `limit` counts: a
//! stretch without samples costs a page nothing, so that a page is full
//! unless it is the last or a bound stopped it. A bucket ends the page once
//! it is the first past the `limit` the page holds.
//!
//! A part's times are read from its clock or its chunk only where they
//! decide something: a run with a sample a bucket or more meets every bucket
//! it spans, since reading each time of dense samples costs a page a third
//! more, and a gap inside such a run only shortens the page that holds it.
//!
//! ```text
//! limit 3, every 1m   samples at 10:00:10  10:00:40  10:07:05  10:09:30  10:12:00
//! buckets met         10:00  10:07  10:09  10:12
//! the page            [from … 10:12)       three buckets; the next page begins at 10:12
//! ```

use std::collections::BTreeSet;

use crate::metrics::aggregate::Shape;

/// The buckets a group's page met samples in so far.
#[derive(Debug)]
pub(super) struct Sampled<'s> {
    shape: &'s Shape,
    /// Where the range begins, which is the bucket of a range of one.
    range_from: i64,
    to: i64,
    most: usize,
    /// The starts of the buckets met: the `most` first and the one after.
    starts: BTreeSet<i64>,
    /// The bucket the last sample fell in, `[start, end)`, so that a run of
    /// samples in one bucket finds it once.
    last: (i64, i64),
}

impl<'s> Sampled<'s> {
    pub(super) fn new(shape: &'s Shape, range_from: i64, most: usize, to: i64) -> Sampled<'s> {
        Sampled { shape, range_from, to, most, starts: BTreeSet::new(), last: (i64::MAX, i64::MIN) }
    }

    /// Where the page ends at most: the start of the first bucket past the
    /// `most` it holds, or the range's end while it holds no more.
    pub(super) fn end(&self) -> i64 {
        match self.starts.last() {
            Some(&after) if self.starts.len() > self.most => after,
            _ => self.to,
        }
    }

    /// A sample's time: the bucket it falls in is met.
    pub(super) fn meet(&mut self, at: i64) {
        if at >= self.last.0 && at < self.last.1 {
            return;
        }
        let start = self.shape.bucket_of(at, self.range_from);
        self.last = (start, self.shape.after_buckets(start, 1, i64::MAX));
        if start >= self.end() {
            return;
        }
        self.starts.insert(start);
        if self.starts.len() > self.most + 1 {
            self.starts.pop_last();
        }
    }

    /// Meets every bucket from the one that holds `from` to the one that
    /// holds `to`, a run of `count` samples, when it holds a sample a bucket
    /// at least, so that its times need not be read; false, meeting none,
    /// when it is sparser, and its caller meets each sample. A gap inside a
    /// run that dense counts the buckets it leaves empty, which ends a page
    /// early and never answers a bucket wrongly.
    pub(super) fn meet_run(&mut self, from: i64, to: i64, count: usize) -> bool {
        let shape = self.shape;
        let first = shape.bucket_of(from, self.range_from);
        // the start of the bucket after the one at `start`, none past the run
        let after =
            |start: i64| Some(shape.after_buckets(start, 1, i64::MAX)).filter(|&next| next > start && next <= to);
        let mut spanned = 0;
        let mut at = Some(first);
        while let Some(start) = at.filter(|&start| start < self.end()) {
            spanned += 1;
            if spanned > count {
                return false;
            }
            at = after(start);
        }
        let mut at = Some(first);
        while let Some(start) = at.filter(|&start| start < self.end()) {
            self.meet(start.max(from));
            at = after(start);
        }
        true
    }

    /// How many of the buckets met begin before `end`.
    pub(super) fn before(&self, end: i64) -> usize {
        self.starts.range(..end).count()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::metrics::Op;
    use crate::metrics::aggregate::Asking;

    const MINUTE: i64 = 60_000;

    fn every_minute() -> Shape {
        Shape::of(Op::Count, Asking { every: Some(Duration::from_mins(1)), ..Asking::default() }).unwrap()
    }

    #[test]
    fn a_page_ends_at_the_first_bucket_with_samples_past_its_limit() {
        let shape = every_minute();
        let mut sampled = Sampled::new(&shape, 0, 3, 60 * MINUTE);
        for at in [10_000, 40_000, 7 * MINUTE + 5000, 9 * MINUTE + 30_000, 12 * MINUTE, 13 * MINUTE] {
            sampled.meet(at);
        }
        assert_eq!(sampled.end(), 12 * MINUTE, "three buckets, the empty minutes between them free");
        assert_eq!(sampled.before(sampled.end()), 3);
        sampled.meet(MINUTE + 1);
        assert_eq!((sampled.end(), sampled.before(sampled.end())), (9 * MINUTE, 3), "a series met later goes first");
    }

    #[test]
    fn a_page_short_of_its_limit_ends_with_the_range() {
        let shape = every_minute();
        let mut sampled = Sampled::new(&shape, 0, 3, 60 * MINUTE);
        assert_eq!((sampled.end(), sampled.before(60 * MINUTE)), (60 * MINUTE, 0), "a group without samples");
        sampled.meet(30 * MINUTE);
        assert_eq!((sampled.end(), sampled.before(60 * MINUTE)), (60 * MINUTE, 1));
        let one = Shape::of(Op::Count, Asking::default()).unwrap();
        let mut whole = Sampled::new(&one, 5 * MINUTE, 1, 60 * MINUTE);
        whole.meet(6 * MINUTE);
        whole.meet(59 * MINUTE);
        assert_eq!((whole.end(), whole.before(whole.end())), (60 * MINUTE, 1), "a range of one bucket");
    }

    #[test]
    fn a_run_with_a_sample_a_bucket_meets_every_bucket_it_spans_and_a_sparser_one_none() {
        let shape = every_minute();
        let mut sampled = Sampled::new(&shape, 0, 100, 60 * MINUTE);
        assert!(sampled.meet_run(30_000, 5 * MINUTE, 6), "six samples over six minutes");
        assert_eq!(sampled.before(60 * MINUTE), 6);
        assert!(!sampled.meet_run(10 * MINUTE, 20 * MINUTE, 3), "three samples over eleven minutes");
        assert_eq!(sampled.before(60 * MINUTE), 6, "a sparser run meets nothing, for its times to be read");
        let mut short = Sampled::new(&shape, 0, 2, 60 * MINUTE);
        assert!(short.meet_run(0, 9 * MINUTE, 10), "past where the page ends a run's buckets count nothing");
        assert_eq!((short.end(), short.before(short.end())), (2 * MINUTE, 2));
    }
}
