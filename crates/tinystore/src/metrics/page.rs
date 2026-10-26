//! Pages of a read or an aggregate. Given `limit`, a call answers at most
//! that many samples or buckets and `next`, the place the page after it
//! begins; it stops before whatever would pass a bound of a query, before
//! the snapshot copies it, so that a range of any length reads a page at a
//! time.
//!
//! ```text
//! series by id   s1 ─────────────────   s2 ─────────────────   s3 ──────
//! page 1         [from ……………… to)      [from … 14:03:20)
//! page 2                               [14:03:20 … to)        [from … …)
//! its after      "1~<query>~<from>~<to>~<now>~<s2>~<14:03:20>"
//! ```
//!
//! An aggregate's pages go a group after another the same way, and end only
//! between two whole buckets of a group. No snapshot is held between pages:
//! each is one read of the file, and the cursor keeps the range and the clock
//! the first page read, so that every page reads the window the first one
//! did, `since` resolved once.
//!
//! ```no_run
//! # use std::time::Duration;
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! let month = stats.query().name("ping_rtt_ms").since(Duration::from_hours(30 * 24)).limit(10_000);
//! let page = month.read()?;
//! if let Some(next) = &page.next {
//!     let more = month.clone().after(next).read()?;
//! }
//! for series in page.all() {
//!     let series = series?;
//! }
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::vec::IntoIter;

use super::aggregate::{Aggregate, Op};
use super::query::{Asked, Query};
use super::series::{Labels, Series};
use crate::{Error, Result};

/// The samples or buckets of a page unless its `limit` says, and at most.
pub(super) const MOST: usize = 100_000;

/// The version of what a page's `next` holds: one of another is refused,
/// never read as this one.
const VERSION: &str = "1";

/// One page of a read: its series, and `next` while there is more. A series
/// the page cut says [`Series::more`], and the page `next` reads begins with
/// the rest of it. A read without `limit` is one page, whole, without `next`.
#[derive(Clone, Debug)]
pub struct SeriesPage {
    pub series: Vec<Series>,
    pub next: Option<String>,
    /// The read that gave the page, which a walk reads the next one by.
    query: Query,
}

impl SeriesPage {
    pub(super) fn of(series: Vec<Series>, next: Option<String>, query: Query) -> SeriesPage {
        SeriesPage { series, next, query }
    }

    /// The page's series, then those of each page after it, read one page at
    /// a time as the walk reaches the end of the one it holds.
    pub fn all(self) -> All<Series> {
        let query = self.query;
        All::of(self.series, self.next, move |after| {
            query.clone().after(after).read().map(|page| (page.series, page.next))
        })
    }
}

/// One page of an aggregate: its groups, and `next` while there is more. A
/// group the page cut says [`Aggregate::more`], and the page `next` reads
/// begins with the rest of its buckets. An aggregate without `limit` is one
/// page, whole, without `next`.
#[derive(Clone, Debug)]
pub struct AggregatePage {
    pub groups: Vec<Aggregate>,
    pub next: Option<String>,
    /// The aggregate that gave the page, which a walk reads the next one by.
    query: Query,
    op: Op,
}

impl AggregatePage {
    pub(super) fn of(groups: Vec<Aggregate>, next: Option<String>, query: Query, op: Op) -> AggregatePage {
        AggregatePage { groups, next, query, op }
    }

    /// The page's groups, then those of each page after it, read one page at
    /// a time as the walk reaches the end of the one it holds.
    pub fn all(self) -> All<Aggregate> {
        let (query, op) = (self.query, self.op);
        All::of(self.groups, self.next, move |after| {
            query.clone().after(after).aggregate(op).map(|page| (page.groups, page.next))
        })
    }
}

/// What a walk reads the page after one by, from its `next`.
type ReadOn<T> = Box<dyn FnMut(&str) -> Result<(Vec<T>, Option<String>)> + Send>;

/// The series or the groups of a page and of every page after it, one page
/// held at a time.
pub struct All<T> {
    items: IntoIter<T>,
    next: Option<String>,
    read_on: ReadOn<T>,
}

impl<T> All<T> {
    fn of(
        items: Vec<T>,
        next: Option<String>,
        read_on: impl FnMut(&str) -> Result<(Vec<T>, Option<String>)> + Send + 'static,
    ) -> All<T> {
        All { items: items.into_iter(), next, read_on: Box::new(read_on) }
    }
}

impl<T> fmt::Debug for All<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All").field("held", &self.items.len()).field("next", &self.next).finish()
    }
}

impl<T> Iterator for All<T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(item) = self.items.next() {
                return Some(Ok(item));
            }
            let after = self.next.take()?;
            match (self.read_on)(&after) {
                Ok((items, next)) => (self.items, self.next) = (items.into_iter(), next),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// Where a read's page begins: the first series of an id at least `series`,
/// read from `at` on, and each series after it from the range's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) series: i64,
    pub(super) at: i64,
}

/// Where an aggregate's page begins: the group of this name and these
/// labels, from the bucket at `at` on, or the first group after it when it
/// is gone; each group after it from the range's start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GroupPlace {
    pub(super) group: (String, Labels),
    pub(super) at: i64,
}

/// Where a page begins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Start {
    First,
    Series(Place),
    Group(GroupPlace),
}

/// A page asked for: its samples or buckets at most, the query its cursor
/// names, and where it begins.
#[derive(Clone, Debug)]
pub(super) struct Paging {
    pub(super) limit: usize,
    pub(super) query: String,
    pub(super) from: Start,
}

impl Paging {
    /// Where a read's page begins.
    pub(super) fn series_from(&self) -> Place {
        match self.from {
            Start::Series(place) => place,
            _ => Place { series: i64::MIN, at: i64::MIN },
        }
    }

    /// Where an aggregate's page begins: none for its first group.
    pub(super) fn group_from(&self) -> Option<&GroupPlace> {
        match &self.from {
            Start::Group(place) => Some(place),
            _ => None,
        }
    }
}

/// What a page's `next` holds: the query it goes on with, the range and the
/// clock the first page read, and where the next page begins.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Cursor {
    pub(super) query: String,
    pub(super) from: i64,
    pub(super) to: i64,
    pub(super) now: i64,
    pub(super) start: Start,
}

impl Cursor {
    /// The cursor of the page after one read for `paging` over `asked`.
    pub(super) fn after(asked: &Asked, paging: &Paging, start: Start) -> Cursor {
        let query = paging.query.clone();
        Cursor { query, from: asked.from, to: asked.to, now: asked.now, start }
    }

    pub(super) fn written(&self) -> String {
        let Cursor { query, from, to, now, start } = self;
        let (place, at) = match start {
            Start::First => (String::new(), i64::MIN),
            Start::Series(place) => (place.series.to_string(), place.at),
            Start::Group(place) => (group_written(&place.group), place.at),
        };
        format!("{VERSION}~{query}~{from}~{to}~{now}~{place}~{at}")
    }

    /// A cursor as [`Cursor::written`] wrote it; anything else is `invalid`,
    /// a cursor of another version naming it.
    pub(super) fn read(next: &str) -> Result<Cursor> {
        let fields: Vec<&str> = next.split('~').collect();
        let number = |field: &str| field.parse::<i64>().ok();
        match fields[..] {
            [VERSION, query, from, to, now, place, at] => {
                let read = (number(from), number(to), number(now), number(at));
                let (Some(from), Some(to), Some(now), Some(at)) = read else { return Err(not_a_page(next)) };
                let start = start_of(place, at).ok_or_else(|| not_a_page(next))?;
                Ok(Cursor { query: query.to_owned(), from, to, now, start })
            }
            [version, ..] if fields.len() == 7 => Err(Error::invalid(format!(
                "after {next:?}: a page's next of version {version}, which this store does not read"
            ))),
            _ => Err(not_a_page(next)),
        }
    }
}

/// Where a cursor's place begins: a series by its id, a group by `g` and the
/// hex of its name and labels as JSON, from `at` on.
fn start_of(place: &str, at: i64) -> Option<Start> {
    if place.is_empty() {
        return Some(Start::First);
    }
    if let Some(hex) = place.strip_prefix('g') {
        return group_read(hex).map(|group| Start::Group(GroupPlace { group, at }));
    }
    place.parse().ok().map(|series| Start::Series(Place { series, at }))
}

fn group_written(group: &(String, Labels)) -> String {
    let json = serde_json::to_vec(group).unwrap_or_default();
    let hex: String = json.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("g{hex}")
}

fn group_read(hex: &str) -> Option<(String, Labels)> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> =
        (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok()).collect();
    serde_json::from_slice(&bytes?).ok()
}

fn not_a_page(next: &str) -> Error {
    Error::invalid(format!("after {next:?}, which is no page's next"))
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
