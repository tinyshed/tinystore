//! Reads: the events of a range that meet every condition given, a page at a
//! time, in the order of their times.
//!
//! ```text
//! pick rows by their times ─► fetch their bodies ─► parse ─► keep what matches ─► order ─► cut a page
//! └────────── one read of the file ──────────┘   └───────────────── after it ─────────────────┘
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::params;
use serde::Serialize;

use super::engine::{Records, Stream, nanos};
use super::event::{Event, Fields, Level, TraceId};
use super::head::{self, Codec};
use crate::sqlite::sql_error;
use crate::{Error, Result, unix_nanos};

/// The events of a page unless its `limit` says.
pub const PAGE_EVENTS: usize = 1000;
/// The events a page holds at most.
const MOST_EVENTS: usize = 10_000;
/// The rows and the bytes one page fetches before it ends early.
const FETCH_ROWS: usize = 1024;
const FETCH_BYTES: usize = 16 << 20;
/// What a read reserves for each event its rows hold, beside their bytes.
const EVENT_HELD: u64 = 1 << 10;
/// How long a read waits for the store's memory.
const PATIENCE: Duration = Duration::from_secs(10);

/// One page of an answer. `next` is the page after it, for `after`; the last
/// page has none.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub events: Vec<Event>,
    pub next: Option<String>,
}

/// What a read looks for.
#[derive(Clone, Debug, Default)]
struct Query {
    since: Option<Duration>,
    from: Option<SystemTime>,
    to: Option<SystemTime>,
    streams: Vec<String>,
    names: Vec<String>,
    min_level: Option<Level>,
    trace_id: Option<TraceId>,
    /// Each a key and the JSON its value is written as.
    filters: Vec<(String, String)>,
    /// In lower case.
    search: Option<String>,
    newest: bool,
    limit: Option<usize>,
    after: Option<String>,
    /// A condition that could not be taken, for the read that refuses it.
    refused: Option<String>,
}

impl Query {
    /// Whether an event meets every condition but the range.
    fn keeps(&self, event: &Event) -> bool {
        let named = self.names.is_empty() || self.names.contains(&event.name);
        let grave = self.min_level.is_none_or(|least| event.level.is_some_and(|level| level >= least));
        let traced = self.trace_id.is_none_or(|trace| event.trace_id == Some(trace));
        let holds = |(key, json): &(String, String)| has(&event.fields, key, json) || has(&event.context, key, json);
        named
            && grave
            && traced
            && self.filters.iter().all(holds)
            && self.search.as_ref().is_none_or(|text| says(event, text))
    }
}

/// Whether `fields` have `key` at their top with a value written as `json`.
fn has(fields: &Fields, key: &str, json: &str) -> bool {
    fields.iter().any(|(name, value)| name == key && value == json)
}

/// Whether an event holds `text`, which is in lower case: in its message, in
/// its name, or in a value of its fields or its context.
fn says(event: &Event, text: &str) -> bool {
    let within = |words: &str| words.to_lowercase().contains(text);
    let values = |fields: &Fields| fields.iter().any(|(_, json)| value_says(json, text));
    event.message.as_deref().is_some_and(within)
        || within(&event.name)
        || values(&event.fields)
        || values(&event.context)
}

/// Whether a JSON value holds `text` at any depth: a string as it reads, any
/// other value as it is written. A key is not looked at.
fn value_says(json: &str, text: &str) -> bool {
    match json.as_bytes().first() {
        Some(b'"') => serde_json::from_str::<String>(json).is_ok_and(|read| read.to_lowercase().contains(text)),
        Some(b'[') => serde_json::from_str::<Vec<&serde_json::value::RawValue>>(json)
            .is_ok_and(|items| items.iter().any(|item| value_says(item.get(), text))),
        Some(b'{') => Fields::read(json).is_ok_and(|inner| inner.iter().any(|(_, value)| value_says(value, text))),
        _ => json.to_lowercase().contains(text),
    }
}

/// A read being asked: every condition narrows it, and `page` or `all` runs
/// it.
#[derive(Clone)]
pub struct List {
    records: Arc<Records>,
    /// The stream read; every stream without one.
    stream: Option<Stream>,
    query: Query,
}

impl fmt::Debug for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stream = self.stream.as_ref().map(|stream| &stream.name);
        f.debug_struct("List").field("stream", &stream).field("query", &self.query).finish()
    }
}

impl List {
    pub(super) fn of(records: Arc<Records>, stream: Option<Stream>) -> List {
        List { records, stream, query: Query::default() }
    }

    /// The span before now the range covers; or `from` and `to`.
    pub fn since(mut self, span: Duration) -> Self {
        self.query.since = Some(span);
        self
    }

    /// The range's start, which is in it.
    pub fn from(mut self, at: SystemTime) -> Self {
        self.query.from = Some(at);
        self
    }

    /// The range's end, which is not in it.
    pub fn to(mut self, at: SystemTime) -> Self {
        self.query.to = Some(at);
        self
    }

    /// Any of these streams, for a read of every stream.
    pub fn streams<S: Into<String>>(mut self, streams: impl IntoIterator<Item = S>) -> Self {
        self.query.streams.extend(streams.into_iter().map(Into::into));
        self
    }

    /// Any of these names: `["log"]` is the lines.
    pub fn names<S: Into<String>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.query.names.extend(names.into_iter().map(Into::into));
        self
    }

    /// This level and worse. An event without a level is not found.
    pub fn min_level(mut self, level: Level) -> Self {
        self.query.min_level = Some(level);
        self
    }

    pub fn trace_id(mut self, trace_id: TraceId) -> Self {
        self.query.trace_id = Some(trace_id);
        self
    }

    /// `key` with this value, at the top of an event's fields or of its
    /// context, by the JSON the value is written as: `42` finds `42`, and
    /// neither `"42"` nor `42.0`.
    pub fn filter(mut self, key: &str, value: &impl Serialize) -> Self {
        match serde_json::to_string(value) {
            Ok(json) => self.query.filters.push((key.to_owned(), json)),
            Err(error) => self.query.refused = Some(format!("the value of {key:?}, which JSON cannot write: {error}")),
        }
        self
    }

    /// This text as a phrase, its case ignored: in an event's message, in
    /// its name, or in a value of its fields or its context at any depth.
    pub fn search(mut self, text: &str) -> Self {
        self.query.search = Some(text.to_lowercase());
        self
    }

    /// Newest first; oldest first without it.
    pub fn newest(mut self) -> Self {
        self.query.newest = true;
        self
    }

    /// The events of a page: 1000, and 10,000 at most.
    pub fn limit(mut self, events: usize) -> Self {
        self.query.limit = Some(events);
        self
    }

    /// A page's `next`, with the rest of the read as it was: the page after
    /// it, in the range the first page had.
    pub fn after(mut self, next: &str) -> Self {
        self.query.after = Some(next.to_owned());
        self
    }

    /// One page, from one moment of the file. It never splits a time, and
    /// may hold fewer events than its limit, or none, and still have a
    /// `next`: a read ends at what it may fetch rather than failing.
    pub fn page(&self) -> Result<Page> {
        let read = self.records.page(self.stream.as_ref(), &self.query);
        read.map_err(|error| match &self.stream {
            Some(stream) => error.within(format!("events {}", stream.name)),
            None => error.within("events"),
        })
    }

    /// Every event of the answer, a page at a time.
    pub fn all(self) -> All {
        All { list: self, page: Vec::new().into_iter(), more: true }
    }
}

/// Every event of an answer, read a page at a time as it is walked.
#[derive(Debug)]
pub struct All {
    list: List,
    page: std::vec::IntoIter<Event>,
    more: bool,
}

impl Iterator for All {
    type Item = Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.page.next() {
                return Some(Ok(event));
            }
            if !self.more {
                return None;
            }
            match self.list.page() {
                Ok(page) => {
                    self.more = page.next.is_some();
                    self.list.query.after = page.next;
                    self.page = page.events.into_iter();
                }
                Err(error) => {
                    self.more = false;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// A read with its range and its streams worked out.
struct Asked {
    /// The range in nanoseconds, both ends in it.
    first: i64,
    last: i64,
    limit: usize,
    /// The streams read, by id: each one's name, and the time before which
    /// it keeps nothing.
    streams: HashMap<i64, (Arc<str>, i64)>,
    /// Their ids as SQLite takes a list, when they are not every stream.
    only: Option<String>,
}

/// A head row as a read fetched it.
struct Fetched {
    id: i64,
    stream: i64,
    body: Vec<u8>,
}

/// An event a read found, with where it stood: its row, and its place in it.
struct Found {
    at: i64,
    row: i64,
    place: usize,
    event: Event,
}

const OLDEST_ROWS: &str = "select id, stream, first_at, last_at, count, body from heads
    where last_at >= ?1 and first_at <= ?2 and (?3 = 0 or levels & ?3 != 0)
      and (?4 is null or stream in (select value from json_each(?4)))
    order by first_at, id";
const NEWEST_ROWS: &str = "select id, stream, first_at, last_at, count, body from heads
    where last_at >= ?1 and first_at <= ?2 and (?3 = 0 or levels & ?3 != 0)
      and (?4 is null or stream in (select value from json_each(?4)))
    order by last_at desc, id desc";

impl Records {
    /// One page: work out the range → fetch its rows → parse and keep what
    /// matches → cut the page.
    fn page(&self, stream: Option<&Stream>, query: &Query) -> Result<Page> {
        let Some(asked) = self.asked(stream, query)? else { return Ok(Page { events: Vec::new(), next: None }) };
        let (rows, horizon, events) = self.fetch(&asked, query)?;
        let bytes: usize = rows.iter().map(|row| row.body.len()).sum();
        let _held = self.memory.reserve(bytes as u64 + events * EVENT_HELD, "records: a read", PATIENCE)?;
        let found = found(&asked, query, rows, horizon)?;
        cut(found, &asked, query.newest, horizon)
    }

    /// The range and the streams of a read; `None` when they hold nothing.
    fn asked(&self, stream: Option<&Stream>, query: &Query) -> Result<Option<Asked>> {
        if let Some(refused) = &query.refused {
            return Err(Error::invalid(refused.clone()));
        }
        let limit = query.limit.unwrap_or(PAGE_EVENTS);
        if limit == 0 || limit > MOST_EVENTS {
            return Err(Error::invalid(format!("a page of {limit} events: it holds 1 to {MOST_EVENTS}")));
        }
        let now = self.now();
        let read: Vec<Stream> = match stream {
            Some(_) if !query.streams.is_empty() => {
                return Err(Error::invalid("streams, which a read of every stream takes: a stream reads its own"));
            }
            Some(stream) => self.stream(&stream.name).into_iter().collect(),
            None if query.streams.is_empty() => self.every_stream(),
            None => query.streams.iter().filter_map(|name| self.stream(name)).collect(),
        };
        let only = (stream.is_some() || !query.streams.is_empty())
            .then(|| format!("[{}]", read.iter().map(|stream| stream.id.to_string()).collect::<Vec<_>>().join(",")));
        let kept = |stream: &Stream| now.saturating_sub(nanos(stream.keeps()));
        let Some(oldest) = read.iter().map(kept).min() else { return Ok(None) };
        let (first, last) = range(query, now)?;
        let first = first.max(oldest);
        let streams = read.iter().map(|stream| (stream.id, (Arc::clone(&stream.name), kept(stream)))).collect();
        Ok((first <= last).then_some(Asked { first, last, limit, streams, only }))
    }

    /// The rows of a range from its near end, their bodies fetched in one
    /// read of the file, with the edge of the rows left behind and how many
    /// events the fetched ones hold.
    ///
    /// A page is whole up to that edge and no further: a row left behind
    /// begins at it, oldest first, or ends at it, newest first, so every
    /// event on the near side of it is in a row fetched. A row that reaches
    /// over the range's near end is fetched whatever the page holds already,
    /// so that the edge lies inside the range and the next page begins past
    /// this one.
    fn fetch(&self, asked: &Asked, query: &Query) -> Result<(Vec<Fetched>, Option<i64>, u64)> {
        let mask = query.min_level.map_or(0, Level::bits_from);
        let failed = |error| sql_error("read the head rows", error);
        self.file.read(|connection| {
            let sql = if query.newest { NEWEST_ROWS } else { OLDEST_ROWS };
            let mut select = connection.prepare_cached(sql).map_err(failed)?;
            let mut rows = select.query(params![asked.first, asked.last, mask, asked.only]).map_err(failed)?;
            let (mut fetched, mut events, mut bytes) = (Vec::new(), 0_u64, 0);
            while let Some(row) = rows.next().map_err(failed)? {
                let (first_at, last_at): (i64, i64) = (row.get(2).map_err(failed)?, row.get(3).map_err(failed)?);
                let over_the_end = if query.newest { last_at >= asked.last } else { first_at <= asked.first };
                let full = events >= asked.limit as u64 || fetched.len() >= FETCH_ROWS || bytes >= FETCH_BYTES;
                if full && !over_the_end {
                    return Ok((fetched, Some(if query.newest { last_at } else { first_at }), events));
                }
                let body: Vec<u8> = row.get(5).map_err(failed)?;
                events += row.get::<_, i64>(4).map_err(failed)?.max(0) as u64;
                bytes += body.len();
                fetched.push(Fetched { id: row.get(0).map_err(failed)?, stream: row.get(1).map_err(failed)?, body });
            }
            Ok((fetched, None, events))
        })
    }
}

/// The range a read asks for, both ends in it: a page's `next`, or the
/// query's own span.
fn range(query: &Query, now: i64) -> Result<(i64, i64)> {
    if let Some(after) = &query.after {
        let ends = after.split_once('~').and_then(|(first, last)| Some((first.parse().ok()?, last.parse().ok()?)));
        return ends.ok_or_else(|| Error::invalid(format!("after {after:?}, which is no page's next")));
    }
    if query.since.is_some() && (query.from.is_some() || query.to.is_some()) {
        return Err(Error::invalid("since beside from or to: a range is one or the other"));
    }
    let first = match (query.since, query.from) {
        (Some(span), _) => now.saturating_sub(nanos(span)),
        (None, Some(from)) => unix_nanos(from),
        (None, None) => i64::MIN,
    };
    Ok((first, query.to.map_or(i64::MAX, |to| unix_nanos(to).saturating_sub(1))))
}

/// The events of the fetched rows that the read keeps, in its order: by
/// their times, those of one time as they were written, or all of it
/// backwards.
fn found(asked: &Asked, query: &Query, rows: Vec<Fetched>, horizon: Option<i64>) -> Result<Vec<Found>> {
    let mut codec = Codec::default();
    let mut found = Vec::new();
    for row in rows {
        let Some((stream, kept)) = asked.streams.get(&row.stream) else { continue };
        let parsed = head::parse(stream, &row.body, &mut codec);
        let events = parsed.map_err(|error| error.within(format!("head row {} of {stream}", row.id)))?;
        for (place, (at, event)) in events.into_iter().enumerate() {
            let in_range = at >= asked.first.max(*kept) && at <= asked.last;
            let whole = horizon.is_none_or(|edge| if query.newest { at > edge } else { at < edge });
            if in_range && whole && query.keeps(&event) {
                found.push(Found { at, row: row.id, place, event });
            }
        }
    }
    found.sort_by_key(|found| (found.at, found.row, found.place));
    if query.newest {
        found.reverse();
    }
    Ok(found)
}

/// A page of what was found: the limit's worth, less the events of a time
/// the limit would split, and where the next page begins.
fn cut(mut found: Vec<Found>, asked: &Asked, newest: bool, horizon: Option<i64>) -> Result<Page> {
    let mut next = horizon.map(|edge| if newest { (asked.first, edge) } else { (edge, asked.last) });
    if found.len() > asked.limit {
        let mut whole = asked.limit;
        while whole > 0 && found[whole].at == found[whole - 1].at {
            whole -= 1;
        }
        if whole == 0 {
            let limit = asked.limit;
            return Err(Error::limit(format!("more events at one time than a page of {limit} holds")));
        }
        let edge = found[whole - 1].at;
        next = Some(if newest { (asked.first, edge - 1) } else { (edge + 1, asked.last) });
        found.truncate(whole);
    }
    let next = next.map(|(first, last)| format!("{first}~{last}"));
    Ok(Page { events: found.into_iter().map(|found| found.event).collect(), next })
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
