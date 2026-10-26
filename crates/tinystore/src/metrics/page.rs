//! Pages of a read. A read given `limit` answers at most that many samples
//! and `next`, the place the page after it begins; it stops before whatever
//! would pass a bound of a query, before the snapshot copies it, so that a
//! range of any length reads a page at a time.
//!
//! ```text
//! series by id   s1 ─────────────────   s2 ─────────────────   s3 ──────
//! page 1         [from ……………… to)      [from … 14:03:20)
//! page 2                               [14:03:20 … to)        [from … …)
//! its after      "1~<query>~<from>~<to>~<now>~<s2>~<14:03:20>"
//! ```
//!
//! No snapshot is held between pages: each is one read of the file, and the
//! cursor keeps the range and the clock the first page read, so that every
//! page reads the window the first one did, `since` resolved once.
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

use std::vec::IntoIter;

use super::query::{Asked, Query};
use super::series::Series;
use crate::{Error, Result};

/// The samples of a page unless its `limit` says, and at most.
pub(super) const MOST_SAMPLES: usize = 100_000;

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
    pub fn all(self) -> All {
        All { series: self.series.into_iter(), next: self.next, query: self.query }
    }
}

/// The series of a page and of every page after it, one page held at a time.
#[derive(Debug)]
pub struct All {
    series: IntoIter<Series>,
    next: Option<String>,
    query: Query,
}

impl Iterator for All {
    type Item = Result<Series>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(series) = self.series.next() {
                return Some(Ok(series));
            }
            let after = self.next.take()?;
            match self.query.clone().after(&after).read() {
                Ok(page) => (self.series, self.next) = (page.series.into_iter(), page.next),
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

/// Where a page begins: the first series of an id at least `series`, read
/// from `at` on, and each series after it from the range's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Place {
    pub(super) series: i64,
    pub(super) at: i64,
}

impl Place {
    pub(super) const FIRST: Place = Place { series: i64::MIN, at: i64::MIN };
}

/// A page asked for: its samples at most, the query its cursor names, and
/// where it begins.
#[derive(Clone, Debug)]
pub(super) struct Paging {
    pub(super) limit: usize,
    pub(super) query: String,
    pub(super) from: Place,
}

/// What a page's `next` holds: the query it goes on with, the range and the
/// clock the first page read, and where the next page begins.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Cursor {
    pub(super) query: String,
    pub(super) from: i64,
    pub(super) to: i64,
    pub(super) now: i64,
    pub(super) place: Place,
}

impl Cursor {
    /// The cursor of the page after one read for `paging` over `asked`.
    pub(super) fn after(asked: &Asked, paging: &Paging, place: Place) -> Cursor {
        let query = paging.query.clone();
        Cursor { query, from: asked.from, to: asked.to, now: asked.now, place }
    }

    pub(super) fn written(&self) -> String {
        let Cursor { query, from, to, now, place } = self;
        format!("{VERSION}~{query}~{from}~{to}~{now}~{}~{}", place.series, place.at)
    }

    /// A cursor as [`Cursor::written`] wrote it; anything else is `invalid`,
    /// a cursor of another version naming it.
    pub(super) fn read(next: &str) -> Result<Cursor> {
        let fields: Vec<&str> = next.split('~').collect();
        let number = |field: &str| field.parse::<i64>().ok();
        match fields[..] {
            [VERSION, query, from, to, now, series, at] => {
                let read = (number(from), number(to), number(now), number(series), number(at));
                let (Some(from), Some(to), Some(now), Some(series), Some(at)) = read else {
                    return Err(not_a_page(next));
                };
                Ok(Cursor { query: query.to_owned(), from, to, now, place: Place { series, at } })
            }
            [version, ..] if fields.len() == 7 => Err(Error::invalid(format!(
                "after {next:?}: a page's next of version {version}, which this store does not read"
            ))),
            _ => Err(not_a_page(next)),
        }
    }
}

fn not_a_page(next: &str) -> Error {
    Error::invalid(format!("after {next:?}, which is no page's next"))
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
