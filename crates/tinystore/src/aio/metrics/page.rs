//! An awaited read's or aggregate's page, as [`crate::metrics::SeriesPage`]
//! is a blocking one's, whose walk is a [`Stream`]: each page after it read
//! on the store's pool once the walk has given the last of the one it holds.
//!
//! ```no_run
//! # use std::time::Duration;
//! # async fn f(stats: tinystore::aio::metrics::Metrics) -> tinystore::Result<()> {
//! let month = stats.query().name("ping_rtt_ms").since(Duration::from_hours(30 * 24)).limit(10_000);
//! let page = month.read().await?;
//! let every_series = page.all();
//! # Ok(())
//! # }
//! ```
//!
//! [`Stream`]: futures_core::Stream

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::vec::IntoIter;

use futures_core::Stream;

use super::{Aggregate, Op, Query, Series};
use crate::Result;

/// One page of an awaited read: its series, and `next` while there is more.
#[derive(Clone, Debug)]
pub struct SeriesPage {
    pub series: Vec<Series>,
    pub next: Option<String>,
    query: Query,
}

impl SeriesPage {
    pub(super) fn of(page: crate::metrics::SeriesPage, query: Query) -> SeriesPage {
        SeriesPage { series: page.series, next: page.next, query }
    }

    /// The page's series, then those of each page after it, as a stream.
    pub fn all(self) -> All<Series> {
        let query = self.query;
        All::of(self.series, self.next, move |after| {
            let read = query.clone().after(after).read();
            Box::pin(async move { read.await.map(|page| (page.series, page.next)) })
        })
    }
}

/// One page of an awaited aggregate: its groups, and `next` while there is
/// more.
#[derive(Clone, Debug)]
pub struct AggregatePage {
    pub groups: Vec<Aggregate>,
    pub next: Option<String>,
    query: Query,
    op: Op,
}

impl AggregatePage {
    pub(super) fn of(page: crate::metrics::AggregatePage, query: Query, op: Op) -> AggregatePage {
        AggregatePage { groups: page.groups, next: page.next, query, op }
    }

    /// The page's groups, then those of each page after it, as a stream.
    pub fn all(self) -> All<Aggregate> {
        let (query, op) = (self.query, self.op);
        All::of(self.groups, self.next, move |after| {
            let read = query.clone().after(after).aggregate(op);
            Box::pin(async move { read.await.map(|page| (page.groups, page.next)) })
        })
    }
}

/// What a walk reads the page after one by, from its `next`.
type Reading<T> = Pin<Box<dyn Future<Output = Result<(Vec<T>, Option<String>)>> + Send>>;

/// What starts reading the page after one, from its `next`.
type ReadOn<T> = Box<dyn FnMut(&str) -> Reading<T> + Send>;

/// The series or the groups of a page and of every page after it, a
/// [`Stream`] holding one page at a time. Dropped while a page is read, that
/// read stops as a dropped read does.
///
/// [`Stream`]: futures_core::Stream
pub struct All<T> {
    items: IntoIter<T>,
    next: Option<String>,
    reading: Option<Reading<T>>,
    read_on: ReadOn<T>,
}

impl<T> All<T> {
    fn of(items: Vec<T>, next: Option<String>, read_on: impl FnMut(&str) -> Reading<T> + Send + 'static) -> All<T> {
        All { items: items.into_iter(), next, reading: None, read_on: Box::new(read_on) }
    }
}

impl<T> fmt::Debug for All<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All").field("held", &self.items.len()).field("reading", &self.reading.is_some()).finish()
    }
}

impl<T: Unpin> Stream for All<T> {
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let all = self.get_mut();
        loop {
            if let Some(item) = all.items.next() {
                return Poll::Ready(Some(Ok(item)));
            }
            if let Some(reading) = &mut all.reading {
                let read = std::task::ready!(reading.as_mut().poll(context));
                all.reading = None;
                match read {
                    Ok((items, next)) => (all.items, all.next) = (items.into_iter(), next),
                    Err(error) => return Poll::Ready(Some(Err(error))),
                }
                continue;
            }
            let Some(after) = all.next.take() else { return Poll::Ready(None) };
            all.reading = Some((all.read_on)(&after));
        }
    }
}
