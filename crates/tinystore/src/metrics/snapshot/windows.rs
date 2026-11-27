//! What a page of a comparison's groups takes of one snapshot: whole groups,
//! chosen as an aggregate's page chooses them, each over every window of the
//! comparison. A group is taken whole or not at all, since its windows are
//! one answer: a page stops before the group that would pass `limit` or a
//! bound of a query, and a group that alone passes one is `limit`.
//!
//! ```text
//! groups by host   a ──  b ──  c ──  d ──      limit 2
//! windows          newest, 1 day before, 2 days before …
//! the page         a, b over every window      next from c
//! ```
//!
//! A group of more series than one query finds is read in parts instead,
//! the series of each part over every window from a snapshot of its own.

use super::chosen::Choice;
use super::paging::{chosen, named};
use super::{Holds, Reading, Taken, Want};
use crate::metrics::aggregate::Shape;
use crate::metrics::budget::Budget;
use crate::metrics::engine::Engine;
use crate::metrics::matching::{self, Found};
use crate::metrics::page::{GroupPlace, Paging};
use crate::metrics::query::Asked;
use crate::metrics::series::Labels;
use crate::{ErrorKind, Result};

/// One group of a comparison's page: its series as each window took them,
/// the newest window first.
#[derive(Debug)]
pub(in crate::metrics) struct GroupWindows {
    pub(in crate::metrics) group: (String, Labels),
    pub(in crate::metrics) windows: Vec<Vec<Taken>>,
}

/// What a page of a comparison's groups takes: its groups, and where the
/// page after it begins; or the name and labels of its first group alone, of
/// more series than a page holds, which an `op` that adds reads in parts.
#[derive(Debug)]
pub(in crate::metrics) enum WindowsTaken {
    Groups(Vec<GroupWindows>, Option<GroupPlace>),
    Wide((String, Labels)),
}

/// One part of such a group: its series among those found from an id on,
/// over every window, the id the next part's series are found from, and the
/// first group after it met among them.
#[derive(Debug)]
pub(in crate::metrics) struct WideWindows {
    pub(in crate::metrics) windows: Vec<Vec<Taken>>,
    pub(in crate::metrics) beyond: Option<i64>,
    pub(in crate::metrics) after: Option<(String, Labels)>,
}

impl Holds for WindowsTaken {
    fn taken(&mut self) -> Vec<&mut Taken> {
        match self {
            WindowsTaken::Groups(groups, _) => {
                groups.iter_mut().flat_map(|group| group.windows.iter_mut().flatten()).collect()
            }
            WindowsTaken::Wide(_) => Vec::new(),
        }
    }
}

impl Holds for WideWindows {
    fn taken(&mut self) -> Vec<&mut Taken> {
        self.windows.iter_mut().flatten().collect()
    }
}

impl Engine {
    /// The groups of a comparison's page over every one of `ranges`, out of
    /// one snapshot, from where the page begins, `limit` of them at most.
    pub(in crate::metrics) fn take_compared(
        &self,
        asked: &Asked,
        shape: &Shape,
        (ranges, paging): (&[(i64, i64)], &Paging),
        budget: &mut Budget,
    ) -> Result<WindowsTaken> {
        self.snapshot(asked, Want::Buckets(shape), budget, |reading, asked| {
            let most = reading.budget.series_limit();
            let (groups, beyond) = match chosen(reading.connection, asked, shape, paging, most)? {
                Choice::Groups(groups, beyond) => (groups, beyond),
                Choice::TooMany(group, _) if shape.adds() => return Ok(WindowsTaken::Wide(group)),
                Choice::TooMany(group, series) => return Err(reading.budget.too_many(series).within(named(&group))),
            };
            let mut page = Vec::new();
            for (group, found) in groups {
                let place = || Some(GroupPlace { group: group.clone(), at: i64::MIN });
                if page.len() == paging.limit {
                    return Ok(WindowsTaken::Groups(page, place()));
                }
                let spent = *reading.budget;
                let windows = reading.budget.take_group(found.len());
                let windows = windows.and_then(|()| reading.windows(self, &found, asked, shape, ranges));
                match windows {
                    Ok(windows) => page.push(GroupWindows { group, windows }),
                    Err(error) if error.kind() == ErrorKind::Limit && !page.is_empty() => {
                        // the group goes to the next page, whole
                        *reading.budget = spent;
                        return Ok(WindowsTaken::Groups(page, place()));
                    }
                    Err(error) => return Err(error.within(named(&group))),
                }
            }
            Ok(WindowsTaken::Groups(page, beyond.map(|group| GroupPlace { group, at: i64::MIN })))
        })
    }

    /// One part of a group of more series than a page holds, over every one
    /// of `ranges`: the series of the group among as many as a query finds
    /// from the id `from` on, from a snapshot of its own.
    pub(in crate::metrics) fn take_wide_windows(
        &self,
        asked: &Asked,
        shape: &Shape,
        (ranges, group): (&[(i64, i64)], &(String, Labels)),
        from: i64,
        budget: &mut Budget,
    ) -> Result<WideWindows> {
        self.snapshot(asked, Want::Buckets(shape), budget, |reading, asked| {
            let (found, beyond) = matching::find_from(reading.connection, asked, from, reading.budget)?;
            let (mut members, mut after) = (Vec::new(), None);
            for series in found {
                shape.check(&series)?;
                let of = shape.group_of(&series).map_err(|error| error.within(&series.kept))?;
                if of == *group {
                    members.push(series);
                } else if of > *group && after.as_ref().is_none_or(|after| of < *after) {
                    after = Some(of);
                }
            }
            let windows = reading.windows(self, &members, asked, shape, ranges)?;
            Ok(WideWindows { windows, beyond, after })
        })
    }
}

impl Reading<'_, '_> {
    /// A group's series as each of `ranges` takes them, the newest first.
    fn windows(
        &mut self,
        engine: &Engine,
        found: &[Found],
        asked: &Asked,
        shape: &Shape,
        ranges: &[(i64, i64)],
    ) -> Result<Vec<Vec<Taken>>> {
        let mut windows = Vec::with_capacity(ranges.len());
        for &(from, to) in ranges {
            crate::cancel::check(|| "a comparison".to_owned())?;
            let range = Asked { from, to, ..asked.clone() };
            let mut taken = Vec::with_capacity(found.len());
            for series in found {
                taken.push(self.series(engine, series.clone(), &range, Want::Buckets(shape))?);
            }
            windows.push(taken);
        }
        Ok(windows)
    }
}
