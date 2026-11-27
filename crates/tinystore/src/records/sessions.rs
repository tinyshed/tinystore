//! Sessions: a read's events cut by a pause, each session the events that
//! share the values of `by`, one after another while each follows the one
//! before within `gap`.
//!
//! ```text
//! userId 1   09:00 09:10 09:25 ───── pause past 30m ───── 11:00 11:05
//!            └──── session 09:00–09:25, 3 events ────┘     └ 11:00–11:05, 2 ┘
//! ```
//!
//! It walks the read once in the order of the events' times, equal times in
//! their arrivals, across segments and streams as a page reads them, and
//! holds one open session a value of `by`: what it holds is its answer and
//! never the events, past its bounds a `limit` rather than fewer sessions.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::count::{value_of, weight};
use super::engine::nanos;
use super::event::Event;
use super::read::List;
use crate::limits::{SESSION_BYTES, SESSIONS};
use crate::{Error, Reservation, Result, unix_nanos};

/// The sessions one cut holds at most, open and closed, and what they weigh.
const MOST_SESSIONS: usize = SESSIONS.bound as usize;
const MOST_BYTES: u64 = SESSION_BYTES.bound;
/// The events a session keeps at most.
const MOST_KEPT: usize = 1000;
/// What a session, or a value of `by` that has its first sessions, weighs
/// beside its values' JSON.
const HELD_EACH: u64 = 64;

/// What `sessions` cuts a read into: the keys whose values a session shares,
/// the pause that ends one, and what it keeps of each.
#[derive(Clone, Debug, Default)]
pub struct Sessions {
    by: Vec<String>,
    gap: Option<Duration>,
    max: Option<Duration>,
    first_sessions: Option<usize>,
    max_events: usize,
}

/// A session: the events of one value of each key of `by`, each within
/// `gap` of the one before.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// Each key `by` named, as it was written, with its value; null for
    /// events without it.
    pub by: BTreeMap<String, Value>,
    /// Its first event's time, and its last's.
    pub from: SystemTime,
    pub to: SystemTime,
    /// Its events, every one, however few it keeps.
    pub count: u64,
    /// Its first events, as many as `max_events` asked.
    pub events: Vec<Event>,
    /// Whether it may go on outside what was read: its first event within
    /// `gap` of the range's start, or its last within `gap` of its end.
    pub partial: bool,
}

impl Sessions {
    /// Sessions of each value of these keys together: a field's, else a
    /// context's, or `stream`, `name`, `level` or `message`, the event's
    /// own before a field of that name.
    pub fn by<S: Into<String>>(keys: impl IntoIterator<Item = S>) -> Sessions {
        Sessions { by: keys.into_iter().map(Into::into).collect(), ..Sessions::default() }
    }

    /// The pause that ends a session: an event later than this after the
    /// one before begins the next.
    pub fn gap(mut self, gap: Duration) -> Self {
        self.gap = Some(gap);
        self
    }

    /// The longest a session lasts: an event later than this after its
    /// first begins the next, pause or none.
    pub fn max(mut self, max: Duration) -> Self {
        self.max = Some(max);
        self
    }

    /// Only each value's first `sessions` of the range; its later events are
    /// passed over.
    pub fn first_sessions(mut self, sessions: usize) -> Self {
        self.first_sessions = Some(sessions);
        self
    }

    /// The first events each session keeps, 1000 at most; none unless asked.
    pub fn max_events(mut self, events: usize) -> Self {
        self.max_events = events;
        self
    }

    /// The pause and the longest span, in nanoseconds, of a cut within its
    /// bounds; why it is refused otherwise.
    fn checked(&self) -> Result<(i64, Option<i64>)> {
        if self.by.is_empty() {
            return Err(Error::invalid("sessions take by: the keys whose values a session shares"));
        }
        let gap = match self.gap {
            Some(gap) if !gap.is_zero() => nanos(gap),
            _ => return Err(Error::invalid("sessions take a gap longer than none: the pause that ends one")),
        };
        if self.max.is_some_and(|max| max.is_zero()) {
            return Err(Error::invalid("a session's max: a span longer than none"));
        }
        if self.first_sessions == Some(0) {
            return Err(Error::invalid("first 0 sessions: it takes 1 or more"));
        }
        if self.max_events > MOST_KEPT {
            return Err(Error::invalid(format!("{} events a session: it keeps 0 to {MOST_KEPT}", self.max_events)));
        }
        Ok((gap, self.max.map(nanos)))
    }
}

impl List {
    /// The events the read finds cut into sessions by a pause, as
    /// [`Sessions`] says, in the order of their first events and those of
    /// one time by their values of `by`: an answer whole, not paged, or an
    /// error of kind `limit` past 10,000 sessions held, open ones among
    /// them, or past 64 MiB of what they keep.
    ///
    /// A session cut by the range is answered with `partial`. A count is of
    /// every event of its session, however few it keeps.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// use std::time::Duration;
    /// use tinystore::records::Sessions;
    /// let events = store.events("activity").open()?;
    /// let (month, minute) = (Duration::from_hours(30 * 24), Duration::from_secs(60));
    /// let visits = Sessions::by(["userId"]).gap(minute * 30).max(minute * 240).first_sessions(1).max_events(50);
    /// let first_visits = events.list().since(month).sessions(&visits)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn sessions(&self, sessions: &Sessions) -> Result<Vec<Session>> {
        let (gap, max) = sessions.checked().map_err(|error| self.within(error))?;
        let Some(read) = self.read_span()? else { return Ok(Vec::new()) };
        let mut cut = Cut::new(sessions, (gap, max), self.nothing_held());
        for event in self.walk("sessions")? {
            cut.add(event?).map_err(|error| self.within(error))?;
        }
        Ok(cut.finish(read))
    }
}

/// A session being cut.
struct Open {
    by: Vec<Value>,
    first: i64,
    last: i64,
    count: u64,
    kept: Vec<Event>,
}

/// The sessions of a read as its events come, in their order.
struct Cut<'a> {
    asked: &'a Sessions,
    gap: i64,
    max: Option<i64>,
    /// The open session of each value of `by`, by its JSON.
    open: HashMap<String, Open>,
    closed: Vec<Open>,
    /// How many sessions each value closed, while `first_sessions` counts them.
    ended: HashMap<String, usize>,
    held: Reservation,
    bytes: u64,
}

impl<'a> Cut<'a> {
    fn new(asked: &'a Sessions, (gap, max): (i64, Option<i64>), held: Reservation) -> Cut<'a> {
        let (open, closed, ended) = (HashMap::new(), Vec::new(), HashMap::new());
        Cut { asked, gap, max, open, closed, ended, held, bytes: 0 }
    }

    /// Adds an event to its value's open session, or begins one: after a
    /// pause past `gap`, or past `max` from the session's first event, its
    /// value's session ends, and once a value has its `first_sessions` its
    /// later events are passed over.
    fn add(&mut self, event: Event) -> Result<()> {
        let by: Vec<Value> = self.asked.by.iter().map(|key| value_of(&event, key).unwrap_or(Value::Null)).collect();
        let key = Value::Array(by.clone()).to_string();
        if self.has_its_first(&key) {
            return Ok(());
        }
        let at = unix_nanos(event.at);
        if let Some(open) = self.open.get_mut(&key) {
            let within = at - open.last <= self.gap && self.max.is_none_or(|max| at - open.first <= max);
            if within {
                open.last = at;
                open.count += 1;
                return keep(open, event, self.asked.max_events, (&mut self.held, &mut self.bytes));
            }
            self.close(&key)?;
            if self.has_its_first(&key) {
                return Ok(());
            }
        }
        self.begin(key, by, (at, event))
    }

    fn has_its_first(&self, key: &str) -> bool {
        self.asked.first_sessions.is_some_and(|first| self.ended.get(key).is_some_and(|ended| *ended >= first))
    }

    /// Begins a session of the value `key` with its first event, within the
    /// sessions a cut holds.
    fn begin(&mut self, key: String, by: Vec<Value>, (at, event): (i64, Event)) -> Result<()> {
        if self.open.len() + self.closed.len() >= MOST_SESSIONS {
            let refused = format!(
                "past the {MOST_SESSIONS} sessions one read holds: ask a shorter range or fewer first sessions"
            );
            return Err(SESSIONS.refuse(MOST_SESSIONS as u64 + 1, refused));
        }
        grow((&mut self.held, &mut self.bytes), HELD_EACH + key.len() as u64)?;
        let mut open = Open { by, first: at, last: at, count: 1, kept: Vec::new() };
        keep(&mut open, event, self.asked.max_events, (&mut self.held, &mut self.bytes))?;
        self.open.insert(key, open);
        Ok(())
    }

    /// Ends the open session of the value `key`, counting it toward its
    /// `first_sessions`.
    fn close(&mut self, key: &str) -> Result<()> {
        let Some(open) = self.open.remove(key) else { return Ok(()) };
        self.closed.push(open);
        if self.asked.first_sessions.is_some() {
            if !self.ended.contains_key(key) {
                grow((&mut self.held, &mut self.bytes), HELD_EACH + key.len() as u64)?;
            }
            *self.ended.entry(key.to_owned()).or_default() += 1;
        }
        Ok(())
    }

    /// Every session, the open ones ended, in the order of their first
    /// events and then of their values of `by`; each cut by `read`, the
    /// range read, said to be partial.
    fn finish(mut self, (from, to): (i64, i64)) -> Vec<Session> {
        self.closed.extend(self.open.drain().map(|(_, open)| open));
        let mut sessions: Vec<(i64, String, Open)> =
            self.closed.into_iter().map(|open| (open.first, Value::Array(open.by.clone()).to_string(), open)).collect();
        sessions.sort_by(|(a, a_by, _), (b, b_by, _)| a.cmp(b).then_with(|| a_by.cmp(b_by)));
        let keys = &self.asked.by;
        let gap = self.gap;
        let session = |(_, _, open): (i64, String, Open)| Session {
            by: keys.iter().cloned().zip(open.by).collect(),
            from: crate::clock::from_unix_nanos(open.first),
            to: crate::clock::from_unix_nanos(open.last),
            count: open.count,
            events: open.kept,
            partial: open.first.saturating_sub(from) <= gap || to.saturating_sub(open.last) <= gap,
        };
        sessions.into_iter().map(session).collect()
    }
}

/// Keeps an event in its session while it keeps fewer than `most`, its
/// weight held of the store's memory and of a cut's bound.
fn keep(open: &mut Open, event: Event, most: usize, held: (&mut Reservation, &mut u64)) -> Result<()> {
    if open.kept.len() >= most {
        return Ok(());
    }
    grow(held, weight(&event))?;
    open.kept.push(event);
    Ok(())
}

/// Grows what a cut holds by `bytes`: past its bound `limit`, as past the
/// store's memory.
///
/// The bound is of working memory, the events the sessions keep and one
/// open session a value of `by`: fewer `max_events`, a shorter range or a
/// `where` is the way on, since how many values are active within one `gap`
/// of each other is the read's own and no cut of the answer shrinks it.
fn grow((held, bytes): (&mut Reservation, &mut u64), more: u64) -> Result<()> {
    *bytes += more;
    if *bytes > MOST_BYTES {
        let refused =
            format!("past the {MOST_BYTES} bytes one read's sessions hold: ask fewer events or a shorter range");
        return Err(SESSION_BYTES.refuse(*bytes, refused));
    }
    held.grow(more, "records: sessions")
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
