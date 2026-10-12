//! What a store holds, none of it read from a sample: its names, each with
//! what the file keeps beside its series, a name's labels and a label's
//! values, a page at a time in the byte order of their text.
//!
//! ```no_run
//! # fn f(stats: tinystore::metrics::Metrics) -> tinystore::Result<()> {
//! let page = stats.names().prefix("http_").page()?;
//! if let Some(next) = page.next {
//!     let more = stats.names().prefix("http_").after(next).page()?;
//! }
//! let labels = stats.label_names("http_requests_total").page()?.labels;
//! let routes = stats.label_values("http_requests_total", "route").prefix("/api").page()?.values;
//! # Ok(())
//! # }
//! ```
//!
//! A name's count of series is the dictionary's count of its pair
//! `__name__`, its newest time is kept on its row and its oldest is one seek
//! of its series' oldest samples; its labels' values are kept apart, a row a
//! value with how many series hold it, so that no list walks a name's series.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, params};

use super::Metrics;
use super::engine::Engine;
use super::labels::{Kept, MOST_NAME_BYTES, MOST_VALUE_BYTES, NAME};
use super::names::{KEEP, millis};
use super::series::{Kind, Labels};
use crate::clock::from_unix_millis;
use crate::sqlite::{TextBytes, prefix_end, sql_error};
use crate::{Error, Result};

/// What a page holds unless told, and at most.
const PAGE: usize = 1000;
const MOST_PAGE: usize = 10_000;

/// A name's row and what it keeps beside its series: kind, keep, unit, help,
/// newest time, count of series, oldest sample. A name whose newest sample is
/// older than it keeps holds nothing a read finds.
const NAMES: &str = "select n.name, n.kind, n.keep, n.unit, n.help, n.newest,
    coalesce((select posting_count from label_values where name = '__name__' and value = n.name), 0),
    (select min(oldest) from series_state where name = n.id)
    from names n where n.kind is not null and n.newest >= ?1 - coalesce(n.keep, ?2)
    and n.name > ?3 and n.name >= ?4 and (?5 is null or n.name < ?5)
    order by n.name limit cast(?6 as integer)";
const NAME_ID: &str = "select id from names where name = ?1";
/// The next label of a name past `?2`: one seek, however many values it has.
const NEXT_LABEL: &str = "select label from name_labels where name = ?1 and label > ?2
    and label >= ?3 and (?4 is null or label < ?4) order by label limit 1";
const VALUES: &str = "select value from name_labels where name = ?1 and label = ?2 and value > ?3
    and value >= ?4 and (?5 is null or value < ?5) order by value limit cast(?6 as integer)";

/// A name the store holds series of, as `names` gives it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct NameInfo {
    pub name: String,
    pub kind: Kind,
    /// How many series it has.
    pub series: u64,
    pub unit: String,
    pub help: String,
    pub keep: Duration,
    /// Its oldest sample's time, or where its `keep` begins while it holds
    /// samples from before, which no read finds.
    pub oldest: SystemTime,
    pub newest: SystemTime,
}

/// A page of the names a store holds.
#[derive(Clone, Debug, PartialEq)]
pub struct NamePage {
    pub names: Vec<NameInfo>,
    /// The name the next page starts after, when there is more.
    pub next: Option<String>,
}

/// A page of a name's label names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelPage {
    pub labels: Vec<String>,
    /// The label the next page starts after, when there is more.
    pub next: Option<String>,
}

/// A page of the values a label of a name holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuePage {
    pub values: Vec<String>,
    /// The value the next page starts after, when there is more.
    pub next: Option<String>,
}

/// What a list lists.
#[derive(Clone, Debug)]
enum Of {
    Names,
    Labels { name: String },
    Values { name: String, label: String },
}

/// A page of what a store holds, its options before its `page`: 1000 unless
/// `limit` says.
#[derive(Clone)]
#[must_use = "a list reads nothing until its page"]
pub struct List<P> {
    engine: Arc<Engine>,
    of: Of,
    prefix: String,
    after: String,
    limit: usize,
    made: PhantomData<fn() -> P>,
}

impl<P> std::fmt::Debug for List<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("List")
            .field("of", &self.of)
            .field("prefix", &self.prefix)
            .field("after", &self.after)
            .field("limit", &self.limit)
            .finish()
    }
}

impl Metrics {
    /// The names the store holds samples of, each with its kind, its series,
    /// its unit and help, what it keeps and its oldest and newest sample.
    pub fn names(&self) -> List<NamePage> {
        List::of(&self.engine, Of::Names)
    }

    /// A name's label names, but the store's own.
    pub fn label_names(&self, name: impl Into<String>) -> List<LabelPage> {
        List::of(&self.engine, Of::Labels { name: name.into() })
    }

    /// The values a label of a name's series holds.
    pub fn label_values(&self, name: impl Into<String>, label: impl Into<String>) -> List<ValuePage> {
        List::of(&self.engine, Of::Values { name: name.into(), label: label.into() })
    }
}

impl<P> List<P> {
    fn of(engine: &Arc<Engine>, of: Of) -> List<P> {
        List {
            engine: Arc::clone(engine),
            of,
            prefix: String::new(),
            after: String::new(),
            limit: PAGE,
            made: PhantomData,
        }
    }

    /// Only those that start with this text.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// The page that starts past this one, as a page's `next` says it.
    pub fn after(mut self, next: impl Into<String>) -> Self {
        self.after = next.into();
        self
    }

    /// How many a page holds at most, 1 to 10,000.
    pub fn limit(mut self, most: usize) -> Self {
        self.limit = most;
        self
    }

    /// The list's bounds, checked.
    fn checked(&self) -> Result<()> {
        if !(1..=MOST_PAGE).contains(&self.limit) {
            return Err(Error::invalid(format!("a page of {}: it holds 1 to {MOST_PAGE}", self.limit)));
        }
        if self.prefix.len() > MOST_VALUE_BYTES || self.after.len() > MOST_VALUE_BYTES {
            return Err(Error::invalid(format!("a prefix or an `after` of {MOST_VALUE_BYTES} bytes at most")));
        }
        match &self.of {
            Of::Names => Ok(()),
            Of::Labels { name } => Kept::of(name, &Labels::new()).map(drop),
            Of::Values { name, label } => {
                Kept::of(name, &Labels::new())?;
                if label.starts_with("__") {
                    return Err(Error::invalid(format!(
                        "label {label:?}: a name beginning with __ is the store's own"
                    )));
                }
                if label.is_empty() || label.len() > MOST_NAME_BYTES {
                    return Err(Error::invalid(format!("label {label:?}: a name of 1 to {MOST_NAME_BYTES} bytes")));
                }
                Ok(())
            }
        }
    }

    /// A page of what the list read, which reads one past its limit, and
    /// where the next starts when there is more.
    fn paged<T>(&self, read: Result<Vec<T>>, key: impl Fn(&T) -> &str) -> Result<(Vec<T>, Option<String>)> {
        let within = match &self.of {
            Of::Names => "metrics".to_owned(),
            Of::Labels { name } | Of::Values { name, .. } => format!("metrics {name}"),
        };
        let mut read = read.map_err(|error| error.within(within))?;
        let more = read.len() > self.limit;
        read.truncate(self.limit);
        let next = if more { read.last().map(|last| key(last).to_owned()) } else { None };
        Ok((read, next))
    }
}

impl List<NamePage> {
    pub fn page(self) -> Result<NamePage> {
        let read = self.checked().and_then(|()| self.engine.names(&self));
        let (names, next) = self.paged(read, |info| &info.name)?;
        Ok(NamePage { names, next })
    }
}

impl List<LabelPage> {
    pub fn page(self) -> Result<LabelPage> {
        let read = self.checked().and_then(|()| match &self.of {
            Of::Labels { name } => self.engine.label_names(name, &self),
            _ => Err(Error::internal("a page of labels without its name")),
        });
        let (labels, next) = self.paged(read, String::as_str)?;
        Ok(LabelPage { labels, next })
    }
}

impl List<ValuePage> {
    pub fn page(self) -> Result<ValuePage> {
        let read = self.checked().and_then(|()| match &self.of {
            Of::Values { name, label } => self.engine.label_values(name, label, &self),
            _ => Err(Error::internal("a page of values without its label")),
        });
        let (values, next) = self.paged(read, String::as_str)?;
        Ok(ValuePage { values, next })
    }
}

/// A name's row as `names` reads it.
type NameRow = (String, Option<String>, Option<i64>, String, String, i64, i64, Option<i64>);

impl Engine {
    /// The names a page lists, one past its limit.
    fn names<P>(&self, list: &List<P>) -> Result<Vec<NameInfo>> {
        debug_assert_eq!(NAME, "__name__", "NAMES counts a name's series by this label");
        let now = self.now();
        let end = prefix_end(&list.prefix);
        let failed = |error| sql_error("read the names", error);
        let rows: Vec<NameRow> = self.file.read(|connection| {
            let mut select = connection.prepare_cached(NAMES).map_err(failed)?;
            let wanted = list.limit as i64 + 1;
            let given = params![now, millis(KEEP), list.after, list.prefix, end.as_deref().map(TextBytes), wanted];
            let rows = select.query_map(given, |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            });
            rows.and_then(Iterator::collect).map_err(failed)
        })?;
        rows.into_iter().map(|row| name_info(row, now)).collect()
    }

    /// A name's labels a page lists, one past its limit, a seek each.
    fn label_names<P>(&self, name: &str, list: &List<P>) -> Result<Vec<String>> {
        let end = prefix_end(&list.prefix);
        let failed = |error| sql_error("read a name's labels", error);
        self.file.read(|connection| {
            let Some(id) = name_id(connection, name)? else { return Ok(Vec::new()) };
            let mut next = connection.prepare_cached(NEXT_LABEL).map_err(failed)?;
            let mut labels: Vec<String> = Vec::new();
            while labels.len() <= list.limit {
                let last = labels.last().unwrap_or(&list.after);
                let given = params![id, last, list.prefix, end.as_deref().map(TextBytes)];
                match next.query_row(given, |row| row.get(0)).optional().map_err(failed)? {
                    Some(label) => labels.push(label),
                    None => break,
                }
            }
            Ok(labels)
        })
    }

    /// A label's values a page lists, one past its limit.
    fn label_values<P>(&self, name: &str, label: &str, list: &List<P>) -> Result<Vec<String>> {
        let end = prefix_end(&list.prefix);
        let failed = |error| sql_error("read a label's values", error);
        self.file.read(|connection| {
            let Some(id) = name_id(connection, name)? else { return Ok(Vec::new()) };
            let mut select = connection.prepare_cached(VALUES).map_err(failed)?;
            let wanted = list.limit as i64 + 1;
            let given = params![id, label, list.after, list.prefix, end.as_deref().map(TextBytes), wanted];
            let values = select.query_map(given, |row| row.get(0));
            values.and_then(Iterator::collect).map_err(failed)
        })
    }
}

fn name_id(connection: &Connection, name: &str) -> Result<Option<i64>> {
    let id = connection
        .prepare_cached(NAME_ID)
        .and_then(|mut select| select.query_row(params![name], |row| row.get(0)).optional());
    id.map_err(|error| sql_error("find a name", error))
}

/// A name's row as `names` gives it, its oldest no earlier than where its
/// `keep` begins.
fn name_info((name, kind, keep, unit, help, newest, series, oldest): NameRow, now: i64) -> Result<NameInfo> {
    let kind = kind.as_deref().and_then(Kind::parse).ok_or_else(|| Error::corrupt(format!("{name}: a kind")))?;
    let keep = keep.map_or(KEEP, |millis| Duration::from_millis(millis.max(0).unsigned_abs()));
    let cutoff = now.saturating_sub(millis(keep));
    let oldest = oldest.unwrap_or(cutoff).max(cutoff);
    let series = u64::try_from(series).unwrap_or(0);
    Ok(NameInfo {
        name,
        kind,
        series,
        unit,
        help,
        keep,
        oldest: from_unix_millis(oldest),
        newest: from_unix_millis(newest),
    })
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
