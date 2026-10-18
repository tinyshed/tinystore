//! Reads: the events of a range that meet every condition given, a page at a
//! time, in the order of their times.
//!
//! ```text
//! pick rows by their times ─► fetch their bodies ─► parse ─► keep what matches ─► order ─► cut a page
//! └────────── one read of the file ──────────┘   └───────────────── after it ─────────────────┘
//! ```
//!
//! An event is in a head row until its stream is packed and in a block after
//! it, and a read picks among both by the same rule. A block whose segment
//! lacks a name or a key the read asks for, or whose filters say it holds
//! no trace or id it asks for, is passed without its body being fetched; a
//! block fetched has the columns the read asks about read first, and its
//! others only when an event in it may be kept.

use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, Rows, ToSql, params};
use serde::Serialize;

use super::block::{self, Wanted};
use super::bloom::may_hold;
use super::engine::{Records, Stream, nanos};
use super::event::{Event, Fields, Kept, Level, TraceId};
use super::head::{self, Codec};
use super::pack::{CONTEXT_KEY, FIELD_KEY, NAME_KEY};
use super::repair::Part;
use super::segment::Schema;
use super::tail::Tail;
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
pub(super) const EVENT_HELD: u64 = 1 << 10;
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
    fn keeps(&self, event: &Kept) -> bool {
        let named = self.names.is_empty() || self.names.contains(&event.name);
        let grave = self.min_level.is_none_or(|least| event.level.is_some_and(|level| level >= least));
        let traced = self.trace_id.is_none_or(|trace| event.trace_id == Some(trace));
        let holds = |(key, json): &(String, String)| has(&event.fields, key, json) || has(&event.context, key, json);
        let said = self.search.as_ref().is_none_or(|text| says(event, text));
        named && grave && traced && self.filters.iter().all(holds) && said
    }
}

/// Whether `fields` have `key` at their top with a value written as `json`.
fn has(fields: &Fields, key: &str, json: &str) -> bool {
    fields.iter().any(|(name, value)| name == key && value == json)
}

/// Whether an event holds `text`, which is in lower case: in its message, in
/// its name, or in a value of its fields or its context.
fn says(event: &Kept, text: &str) -> bool {
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
        self.records.page(self.stream.as_ref(), &self.query).map_err(|error| self.within(error))
    }

    /// The keys the events of the read's range hold, in their fields and
    /// their contexts, sorted: as many as its limit, 1000 unless it says. A
    /// packed segment that reaches into the range gives every key it holds,
    /// which the file keeps beside its events.
    pub fn field_names(&self) -> Result<Vec<String>> {
        self.field_names_prefix("")
    }

    /// The keys `field_names` gives that begin with `prefix`.
    pub fn field_names_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let named = self.records.field_names(self.stream.as_ref(), &self.query, prefix);
        named.map_err(|error| self.within(error))
    }

    /// Every event the read finds, ten thousand a page, for `what` to count:
    /// a count takes no newest first and no page after another, and only
    /// `top` takes a limit.
    pub(super) fn walk(&self, what: &str) -> Result<All> {
        let limited = what != "top" && self.query.limit.is_some();
        if self.query.newest || self.query.after.is_some() || limited {
            let refused = if what == "top" { "newest or after" } else { "newest, limit or after" };
            return Err(self.within(Error::invalid(format!("{what} takes no {refused}"))));
        }
        let mut walk = self.clone();
        walk.query.limit = Some(MOST_EVENTS);
        Ok(walk.all())
    }

    /// The range the read asks for, both ends in it, in nanoseconds.
    pub(super) fn span(&self) -> Result<(i64, i64)> {
        range(&self.query, self.records.now()).map_err(|error| self.within(error))
    }

    /// The limit the read was given.
    pub(super) fn given_limit(&self) -> Option<usize> {
        self.query.limit
    }

    /// An error of this read, named by what it reads.
    fn within(&self, error: Error) -> Error {
        match &self.stream {
            Some(stream) => error.within(format!("events {}", stream.name)),
            None => error.within("events"),
        }
    }

    /// The events appended from now on that meet the read's conditions, a
    /// batch as they come: a tail takes no range, order, limit or after.
    pub fn tail(&self) -> Result<Tail> {
        let query = &self.query;
        let ranged = query.since.is_some() || query.from.is_some() || query.to.is_some();
        if ranged || query.newest || query.limit.is_some() || query.after.is_some() {
            return Err(self.within(Error::invalid("a tail takes no range, order, limit or after")));
        }
        if let Some(refused) = &query.refused {
            return Err(self.within(Error::invalid(refused.clone())));
        }
        let streams = match &self.stream {
            Some(_) if !query.streams.is_empty() => {
                let why = "streams, which a read of every stream takes: a stream reads its own";
                return Err(self.within(Error::invalid(why)));
            }
            Some(stream) => Some(vec![stream.name.to_string()]),
            None if query.streams.is_empty() => None,
            None => Some(query.streams.clone()),
        };
        let query = query.clone();
        Ok(Tail::open(&self.records, streams, Box::new(move |event: &Kept| query.keeps(event))))
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
    newest: bool,
    /// The streams read, by id: each one's name, and the time before which
    /// it keeps nothing.
    streams: HashMap<i64, (Arc<str>, i64)>,
    /// Their ids as SQLite takes a list, when they are not every stream.
    only: Option<String>,
}

impl Asked {
    /// The end of a row a read walks by: where it begins, oldest first, and
    /// where it ends, newest first.
    fn edge(&self, row: &Picked) -> i64 {
        if self.newest { row.last_at } else { row.first_at }
    }

    /// Whether a row reaches over the end of the range a read begins at, and
    /// so may hold the first event of the page.
    fn reaches_over(&self, row: &Picked) -> bool {
        if self.newest { row.last_at >= self.last } else { row.first_at <= self.first }
    }
}

/// Where a row of events is. A stream's blocks were written before what
/// waits in its head, so of two events of one time the block's comes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Block,
    Head,
}

/// A row a read picked by what its index says, its body not read yet.
struct Picked {
    kind: Kind,
    id: i64,
    /// A block's segment, whose row says how to read it; none of a head row.
    segment: i64,
    stream: i64,
    first_at: i64,
    last_at: i64,
    events: u64,
    size: usize,
}

/// What a read fetched: its rows, each with its body, and the rows of the
/// segments its blocks are of, by their ids.
struct Fetched {
    rows: Vec<(Picked, Vec<u8>)>,
    segments: HashMap<i64, Vec<u8>>,
}

/// An event a read found, with where it stood.
struct Found {
    at: i64,
    place: (Kind, i64, usize),
    stream: Arc<str>,
    event: Kept,
}

/// The rows of a range that may hold what a read asks, from the range's
/// older end: the same statement over the heads and over the blocks.
macro_rules! rows_of {
    ($table:literal, $segment:literal, $order:literal) => {
        concat!(
            "select id, stream, first_at, last_at, count, size, ",
            $segment,
            " from ",
            $table,
            " where last_at >= ?1 and first_at <= ?2 and (?3 = 0 or levels & ?3 != 0)
               and (?4 is null or stream in (select value from json_each(?4))) order by ",
            $order
        )
    };
}

const HEADS_OLDEST: &str = rows_of!("heads", "0", "first_at, id");
const HEADS_NEWEST: &str = rows_of!("heads", "0", "last_at desc, id desc");
const BLOCKS_OLDEST: &str = rows_of!("blocks", "segment", "first_at, id");
const BLOCKS_NEWEST: &str = rows_of!("blocks", "segment", "last_at desc, id desc");
const HEAD_BODY: &str = "select body from heads where id = ?1";
const BLOCK_BODY: &str = "select body from blocks where id = ?1";
const SEGMENT_BODY: &str = "select body from segments where id = ?1";
/// The keys of fields and of contexts, kinds 1 and 2, of the segments that
/// reach into a range.
const SEGMENT_KEYS: &str = "select distinct key from segment_keys where kind in (1, 2) and key >= ?3
    and segment in (select id from segments where holder is null and last_at >= ?1 and first_at <= ?2
    and (?4 is null or stream in (select value from json_each(?4))))";
const HEAD_BODIES: &str = "select id, stream, body from heads where last_at >= ?1 and first_at <= ?2
    and (?3 is null or stream in (select value from json_each(?3)))";
const HAS_KEY: &str = "select exists (select 1 from segment_keys where segment = ?1 and kind = ?2 and key = ?3)";
const TRACES_BLOOM: &str = "select bloom from block_traces where block = ?1";
const FIELD_BLOOM: &str = "select bloom from block_filters where block = ?1 and key = ?2";

impl Records {
    /// One page: work out the range → pick and fetch its rows → parse and
    /// keep what matches → cut the page.
    fn page(&self, stream: Option<&Stream>, query: &Query) -> Result<Page> {
        let Some(asked) = self.asked(stream, query)? else { return Ok(Page { events: Vec::new(), next: None }) };
        let (fetched, horizon) = self.fetch(&asked, query)?;
        let bytes: usize = fetched.rows.iter().map(|(picked, _)| picked.size).sum();
        let events: u64 = fetched.rows.iter().map(|(picked, _)| picked.events).sum();
        let _held = self.memory.reserve(bytes as u64 + events * EVENT_HELD, "records: a read", PATIENCE)?;
        let found = self.found(&asked, query, fetched, horizon)?;
        cut(found, &asked, horizon)
    }

    /// The keys a read's range holds: its packed segments' beside them, and
    /// those of the events that wait, read from their rows after the file is.
    fn field_names(&self, stream: Option<&Stream>, query: &Query, prefix: &str) -> Result<Vec<String>> {
        let Some(asked) = self.asked(stream, query)? else { return Ok(Vec::new()) };
        let failed = |error| sql_error("read the keys events hold", error);
        let (mut keys, heads) = self.file.read(|connection| {
            let mut segments = connection.prepare_cached(SEGMENT_KEYS).map_err(failed)?;
            let keys = segments.query_map(params![asked.first, asked.last, prefix, asked.only], |row| row.get(0));
            let keys: BTreeSet<String> = keys.and_then(Iterator::collect).map_err(failed)?;
            let mut heads = connection.prepare_cached(HEAD_BODIES).map_err(failed)?;
            let bodies = heads.query_map(params![asked.first, asked.last, asked.only], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            });
            let bodies: Vec<(i64, i64, Vec<u8>)> = bodies.and_then(Iterator::collect).map_err(failed)?;
            Ok((keys, bodies))
        })?;
        let mut codec = Codec::default();
        for (id, stream, body) in heads {
            // A head row that no longer reads is damage, listed as a read over it lists it.
            let shown = asked.streams.get(&stream).map_or_else(|| stream.to_string(), |(name, _)| name.to_string());
            let events = head::parse(&body, &mut codec)
                .map_err(|error| self.met((Part::HeadRow, id), &shown, error))?
                .into_iter()
                .filter(|event| (asked.first..=asked.last).contains(&event.at));
            keys.extend(events.flat_map(|event| {
                event.fields.iter().chain(event.context.iter()).map(|(key, _)| key.to_owned()).collect::<Vec<_>>()
            }));
        }
        Ok(keys.into_iter().filter(|key| key.starts_with(prefix)).take(asked.limit).collect())
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
        Ok((first <= last).then_some(Asked { first, last, limit, newest: query.newest, streams, only }))
    }

    /// The rows of a range from the end a read begins at, picked by their
    /// index and then fetched, in one read of the file, with the edge of the
    /// rows left behind.
    ///
    /// A page is whole up to that edge and no further: a row left behind
    /// begins at it, oldest first, or ends at it, newest first, so every
    /// event on the near side of it is in a row fetched. A row that reaches
    /// over the range's own end is fetched whatever the page holds already,
    /// so that the edge lies inside the range and the next page begins past
    /// this one.
    fn fetch(&self, asked: &Asked, query: &Query) -> Result<(Fetched, Option<i64>)> {
        let mask = query.min_level.map_or(0, Level::bits_from);
        let failed = |error| sql_error("pick the rows of a read", error);
        self.file.read(|connection| {
            let (heads, blocks) =
                if asked.newest { (HEADS_NEWEST, BLOCKS_NEWEST) } else { (HEADS_OLDEST, BLOCKS_OLDEST) };
            let (mut heads, mut blocks) =
                (connection.prepare_cached(heads).map_err(failed)?, connection.prepare_cached(blocks).map_err(failed)?);
            let range: [&dyn ToSql; 4] = [&asked.first, &asked.last, &mask, &asked.only];
            let mut heads = heads.query(range).map_err(failed)?;
            let mut blocks = blocks.query(range).map_err(failed)?;
            let mut waiting = [picked(&mut blocks, Kind::Block)?, picked(&mut heads, Kind::Head)?];
            let mut passing = Passing { query, connection, segments: HashMap::new() };
            let (mut taken, mut events, mut bytes) = (Vec::new(), 0, 0);
            let mut horizon = None;
            loop {
                // of the two rows that wait, the one a read walking its way meets first
                let nearer = match (&waiting[0], &waiting[1]) {
                    (None, None) => break,
                    (Some(_), None) => 0,
                    (None, Some(_)) => 1,
                    (Some(block), Some(head)) if asked.newest => usize::from(asked.edge(head) > asked.edge(block)),
                    (Some(block), Some(head)) => usize::from(asked.edge(head) < asked.edge(block)),
                };
                let Some(row) = waiting[nearer].take() else { break };
                let full = events >= asked.limit as u64 || taken.len() >= FETCH_ROWS || bytes >= FETCH_BYTES;
                if full && !asked.reaches_over(&row) {
                    horizon = Some(asked.edge(&row));
                    break;
                }
                waiting[nearer] = if row.kind == Kind::Block {
                    picked(&mut blocks, Kind::Block)?
                } else {
                    picked(&mut heads, Kind::Head)?
                };
                if row.kind == Kind::Head || passing.may_find(&row)? {
                    (events, bytes) = (events + row.events, bytes + row.size);
                    taken.push(row);
                }
            }
            let mut segments = HashMap::new();
            for row in taken.iter().filter(|row| row.kind == Kind::Block) {
                if let Entry::Vacant(unread) = segments.entry(row.segment) {
                    unread.insert(body_by(connection, SEGMENT_BODY, row.segment)?);
                }
            }
            let rows = taken.into_iter().map(|row| {
                let sql = if row.kind == Kind::Block { BLOCK_BODY } else { HEAD_BODY };
                body_by(connection, sql, row.id).map(|body| (row, body))
            });
            Ok((Fetched { rows: rows.collect::<Result<_>>()?, segments }, horizon))
        })
    }
}

/// What a read asks that tells it to pass a block without fetching it.
struct Passing<'q> {
    query: &'q Query,
    connection: &'q Connection,
    /// Whether each segment met holds a name and every key the read asks
    /// for, by its id.
    segments: HashMap<i64, bool>,
}

impl Passing<'_> {
    /// Whether a block may hold an event the read asks for: its segment
    /// holds one of the names asked and every key a filter names, in a field
    /// or a context; its traces may hold the trace asked; and the values of
    /// a field that look like ids may hold the one a filter names, unless a
    /// context holds the key.
    fn may_find(&mut self, block: &Picked) -> Result<bool> {
        let query = self.query;
        if query.names.is_empty() && query.trace_id.is_none() && query.filters.is_empty() {
            return Ok(true);
        }
        let holds = match self.segments.get(&block.segment) {
            Some(holds) => *holds,
            None => {
                let holds = self.segment_holds(block.segment)?;
                *self.segments.entry(block.segment).or_insert(holds)
            }
        };
        if !holds {
            return Ok(false);
        }
        if let Some(trace) = query.trace_id
            && !self.bloom(TRACES_BLOOM, params![block.id])?.is_some_and(|bloom| may_hold(&bloom, &trace.0))
        {
            return Ok(false);
        }
        for (key, json) in &query.filters {
            let bloom = self.bloom(FIELD_BLOOM, params![block.id, key])?;
            if bloom.is_some_and(|bloom| !may_hold(&bloom, json.as_bytes()))
                && !self.has(block.segment, CONTEXT_KEY, key)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn segment_holds(&self, segment: i64) -> Result<bool> {
        let mut named = self.query.names.is_empty();
        for name in &self.query.names {
            named = named || self.has(segment, NAME_KEY, name)?;
        }
        if !named {
            return Ok(false);
        }
        for (key, _) in &self.query.filters {
            if !self.has(segment, FIELD_KEY, key)? && !self.has(segment, CONTEXT_KEY, key)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn has(&self, segment: i64, kind: u8, key: &str) -> Result<bool> {
        let found = self
            .connection
            .prepare_cached(HAS_KEY)
            .and_then(|mut select| select.query_row(params![segment, kind, key], |row| row.get(0)));
        found.map_err(|error| sql_error("look up what a segment holds", error))
    }

    fn bloom(&self, sql: &str, key: impl rusqlite::Params) -> Result<Option<Vec<u8>>> {
        let found = self
            .connection
            .prepare_cached(sql)
            .and_then(|mut select| select.query_row(key, |row| row.get(0)).optional());
        found.map_err(|error| sql_error("read a block's filter", error))
    }
}

/// The next row a statement picked, if it has one.
fn picked(rows: &mut Rows<'_>, kind: Kind) -> Result<Option<Picked>> {
    let failed = |error| sql_error("pick the rows of a read", error);
    let Some(row) = rows.next().map_err(failed)? else { return Ok(None) };
    let read: rusqlite::Result<Picked> = (|| {
        let (events, size): (i64, i64) = (row.get(4)?, row.get(5)?);
        let (events, size) = (events.max(0) as u64, size.max(0) as usize);
        Ok(Picked {
            kind,
            id: row.get(0)?,
            segment: row.get(6)?,
            stream: row.get(1)?,
            first_at: row.get(2)?,
            last_at: row.get(3)?,
            events,
            size,
        })
    })();
    read.map(Some).map_err(failed)
}

/// The body of the row `id`, by the statement that reads its table's.
fn body_by(connection: &Connection, sql: &str, id: i64) -> Result<Vec<u8>> {
    let body = connection.prepare_cached(sql).and_then(|mut select| select.query_row([id], |found| found.get(0)));
    body.map_err(|error| sql_error("fetch a row of a read", error))
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

impl Records {
    /// The events of the fetched rows that the read keeps, in its order: by
    /// their times, those of one time as they were written, or all of it
    /// backwards. A row that no longer reads fails the read, named as a drop
    /// names it.
    fn found(&self, asked: &Asked, query: &Query, fetched: Fetched, horizon: Option<i64>) -> Result<Vec<Found>> {
        let mut codec = Codec::default();
        let mut schemas: HashMap<i64, Schema> = HashMap::new();
        let owners: HashMap<i64, i64> = fetched.rows.iter().map(|(row, _)| (row.segment, row.stream)).collect();
        for (id, row) in &fetched.segments {
            let stream = owners.get(id).and_then(|stream| asked.streams.get(stream)).map_or("", |(name, _)| &**name);
            let schema =
                Schema::parse(row, &mut codec).map_err(|error| self.met((Part::Segment, *id), stream, error))?;
            schemas.insert(*id, schema);
        }
        let wanted = Wanted {
            first: asked.first,
            last: asked.last,
            names: &query.names,
            min_level: query.min_level,
            trace: query.trace_id,
            filters: &query.filters,
        };
        let mut found = Vec::new();
        for (row, body) in fetched.rows {
            let Some((stream, kept)) = asked.streams.get(&row.stream) else { continue };
            let parsed = match (row.kind, schemas.get(&row.segment)) {
                (Kind::Head, _) => head::parse(&body, &mut codec)
                    .map(|events| events.into_iter().enumerate().collect())
                    .map_err(|error| self.met((Part::HeadRow, row.id), stream, error)),
                (Kind::Block, Some(schema)) => {
                    block::parse_wanted(schema, &body, &mut codec, &wanted).map_err(|error| {
                        self.met((Part::Segment, row.segment), stream, error.within(format!("block {}", row.id)))
                    })
                }
                (Kind::Block, None) => Err(Error::internal(format!("records: block {} has no segment", row.id))),
            };
            for (place, event) in parsed? {
                let at = event.at;
                let in_range = at >= asked.first.max(*kept) && at <= asked.last;
                let whole = horizon.is_none_or(|edge| if asked.newest { at > edge } else { at < edge });
                if in_range && whole && query.keeps(&event) {
                    found.push(Found { at, place: (row.kind, row.id, place), stream: Arc::clone(stream), event });
                }
            }
        }
        found.sort_by_key(|found| (found.at, found.place));
        if asked.newest {
            found.reverse();
        }
        Ok(found)
    }
}

/// A page of what was found: the limit's worth, less the events of a time
/// the limit would split, and where the next page begins.
fn cut(mut found: Vec<Found>, asked: &Asked, horizon: Option<i64>) -> Result<Page> {
    let mut next = horizon.map(|edge| if asked.newest { (asked.first, edge) } else { (edge, asked.last) });
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
        next = Some(if asked.newest { (asked.first, edge - 1) } else { (edge + 1, asked.last) });
        found.truncate(whole);
    }
    let next = next.map(|(first, last)| format!("{first}~{last}"));
    Ok(Page { events: found.into_iter().map(|found| found.event.into_event(&found.stream)).collect(), next })
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
