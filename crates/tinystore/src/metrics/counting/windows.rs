//! A comparison's groups counted: each window's aggregate of each group, as
//! a query of that window alone answers it, from what one snapshot took; or
//! a group of more series than a page holds, its parts joined into each
//! window's bucket apart, a snapshot a part.

use super::{BUCKET_HELD, Counting, Joining};
use crate::compress::frame::Zstd;
use crate::metrics::aggregate::{Aggregate, Bucket, Shape};
use crate::metrics::budget::Budget;
use crate::metrics::engine::Engine;
use crate::metrics::page::{GroupPlace, Paging, Start};
use crate::metrics::query::Asked;
use crate::metrics::read::PATIENCE;
use crate::metrics::series::Labels;
use crate::metrics::snapshot::{GroupWindows, Taken, TakenGroup, WindowsTaken};
use crate::{Reservation, Result};

/// A group, and each window's one bucket of it, the newest first: none
/// where the window held no sample.
pub(in crate::metrics) type Windowed = ((String, Labels), Vec<Option<Bucket>>);

impl Engine {
    /// The groups of a comparison over each of `ranges`, whole; or those of
    /// a page, and where the page after it begins, none on the last. A group
    /// without a sample in any window is left out, and a page whose groups
    /// all were goes on past them, a snapshot each time, so that only the
    /// last page holds none.
    pub(in crate::metrics) fn compare_groups(
        &self,
        asked: &Asked,
        shape: &Shape,
        ranges: &[(i64, i64)],
        paging: Option<&Paging>,
    ) -> Result<(Vec<Windowed>, Option<GroupPlace>)> {
        let Some(paging) = paging else {
            return Ok((by_group(self.aggregate_ranges(asked, shape, ranges)?), None));
        };
        let mut paging = paging.clone();
        loop {
            let mut budget = Budget::comparing(true);
            let (page, next) = match self.take_compared(asked, shape, (ranges, &paging), &mut budget)? {
                WindowsTaken::Groups(groups, next) => {
                    (self.count_windows(asked, shape, ranges, groups, &mut budget)?, next)
                }
                WindowsTaken::Wide(group) => self.wide_windows(asked, shape, ranges, group)?,
            };
            match next {
                Some(next) if page.is_empty() => {
                    crate::cancel::check(|| "a comparison".to_owned())?;
                    paging.from = Start::Group(next);
                }
                next => return Ok((page, next)),
            }
        }
    }

    /// Each group's buckets, a window at a time, under the store's memory
    /// for what the snapshot copied.
    fn count_windows(
        &self,
        asked: &Asked,
        shape: &Shape,
        ranges: &[(i64, i64)],
        groups: Vec<GroupWindows>,
        budget: &mut Budget,
    ) -> Result<Vec<Windowed>> {
        let mut held = self.memory.reserve_for_read(budget.bytes as u64, "metrics: a comparison", PATIENCE)?;
        let mut zstd = self.codecs.take();
        let mut answer = Vec::with_capacity(groups.len());
        let mut counted = Ok(());
        for group in groups {
            match self.group_windows(asked, shape, ranges, group, (&mut zstd, budget, &mut held)) {
                Ok(Some(windowed)) => answer.push(windowed),
                Ok(None) => {}
                Err(error) => {
                    counted = Err(error);
                    break;
                }
            }
        }
        self.codecs.give_back(zstd);
        counted.map(|()| answer)
    }

    /// One group's bucket of each window; none when no window held a sample.
    fn group_windows(
        &self,
        asked: &Asked,
        shape: &Shape,
        ranges: &[(i64, i64)],
        GroupWindows { group, windows }: GroupWindows,
        (zstd, budget, held): (&mut Zstd, &mut Budget, &mut Reservation),
    ) -> Result<Option<Windowed>> {
        let mut buckets = Vec::with_capacity(ranges.len());
        for (members, &(from, to)) in windows.into_iter().zip(ranges) {
            let range = Asked { from, to, ..asked.clone() };
            let mut counting = Counting {
                engine: self,
                shape,
                asked: &range,
                zstd: &mut *zstd,
                budget: &mut *budget,
                held: &mut *held,
                points: Vec::new(),
            };
            let (name, labels) = group.clone();
            let taken = TakenGroup { name, labels, members, more: false, fills: false };
            buckets.push(one_bucket(counting.groups(vec![taken])?));
        }
        Ok(buckets.iter().any(Option::is_some).then_some((group, buckets)))
    }

    /// A page's one group of more series than a page holds, for an `op`
    /// that adds: its series read as many at a time as a query finds, each
    /// part over every window from a snapshot of its own and within a page's
    /// bounds, and joined into each window's bucket, so that the store holds
    /// one part's bytes and a bucket a window.
    fn wide_windows(
        &self,
        asked: &Asked,
        shape: &Shape,
        ranges: &[(i64, i64)],
        group: (String, Labels),
    ) -> Result<(Vec<Windowed>, Option<GroupPlace>)> {
        let mut zstd = self.codecs.take();
        let joined = self.wide_windows_joined(asked, shape, (ranges, &group), &mut zstd);
        self.codecs.give_back(zstd);
        let WideJoined { buckets, after } = joined?;
        let next = after.map(|group| GroupPlace { group, at: i64::MIN });
        let answered = buckets.iter().any(Option::is_some).then_some((group, buckets));
        Ok((answered.into_iter().collect(), next))
    }

    /// Every part of a wide group joined into each window's bucket, and the
    /// first group after it.
    fn wide_windows_joined(
        &self,
        asked: &Asked,
        shape: &Shape,
        (ranges, group): (&[(i64, i64)], &(String, Labels)),
        zstd: &mut Zstd,
    ) -> Result<WideJoined> {
        let mut budget = Budget::comparing(true);
        let mut joinings: Vec<Joining<'_>> = ranges.iter().map(|_| Joining::new(shape)).collect();
        let mut sums = self.memory.reserve_for_read(0, "metrics: a comparison's buckets", PATIENCE)?;
        let (mut from, mut after) = (i64::MIN, None);
        loop {
            crate::cancel::check(|| "a comparison".to_owned())?;
            budget.next_part();
            let part = self.take_wide_windows(asked, shape, (ranges, group), from, &mut budget)?;
            after = [after, part.after].into_iter().flatten().min();
            let joined: usize = joinings.iter().map(|joining| joining.joined.len()).sum();
            self.join_part(asked, ranges, &part.windows, (&mut joinings, zstd, &mut budget))?;
            let now_joined: usize = joinings.iter().map(|joining| joining.joined.len()).sum();
            sums.grow((now_joined - joined) as u64 * BUCKET_HELD, "metrics: a comparison's buckets")?;
            match part.beyond {
                Some(next) => from = next,
                None => break,
            }
        }
        let buckets = joinings.into_iter().map(|joining| joining.answer().map(|buckets| buckets.into_iter().next()));
        Ok(WideJoined { buckets: buckets.collect::<Result<Vec<_>>>()?, after })
    }

    /// One part's series joined into each window's bucket, under the store's
    /// memory for what its snapshot copied.
    fn join_part(
        &self,
        asked: &Asked,
        ranges: &[(i64, i64)],
        windows: &[Vec<Taken>],
        (joinings, zstd, budget): (&mut [Joining<'_>], &mut Zstd, &mut Budget),
    ) -> Result<()> {
        let mut held = self.memory.reserve_for_read(budget.bytes as u64, "metrics: a comparison", PATIENCE)?;
        for ((members, joining), &(from, to)) in windows.iter().zip(joinings.iter_mut()).zip(ranges) {
            let range = Asked { from, to, ..asked.clone() };
            let shape = joining.shape;
            let mut counting = Counting {
                engine: self,
                shape,
                asked: &range,
                zstd: &mut *zstd,
                budget: &mut *budget,
                held: &mut held,
                points: Vec::new(),
            };
            for series in members {
                crate::cancel::check(|| "a comparison".to_owned())?;
                let counted = joining.begin(&series.found).and_then(|()| counting.series(series, joining));
                counted.map_err(|error| self.met(&series.found, error))?;
            }
        }
        Ok(())
    }
}

/// A wide group's parts joined: each window's bucket, and the first group
/// after it.
struct WideJoined {
    buckets: Vec<Option<Bucket>>,
    after: Option<(String, Labels)>,
}

/// What each window's aggregate answered, by group, in the order of their
/// names and labels: a group's bucket of each window, none where the window
/// held none of its samples.
fn by_group(windows: Vec<Vec<Aggregate>>) -> Vec<Windowed> {
    let count = windows.len();
    let mut groups: std::collections::BTreeMap<(String, Labels), Vec<Option<Bucket>>> = Default::default();
    for (at, answered) in windows.into_iter().enumerate() {
        for group in answered {
            let buckets = groups.entry((group.name, group.labels)).or_insert_with(|| vec![None; count]);
            buckets[at] = group.buckets.into_iter().next();
        }
    }
    groups.into_iter().collect()
}

/// The one bucket of a window's one group, when it held a sample.
fn one_bucket(groups: Vec<Aggregate>) -> Option<Bucket> {
    groups.into_iter().next().and_then(|group| group.buckets.into_iter().next())
}
