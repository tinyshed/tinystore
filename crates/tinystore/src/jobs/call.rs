use std::fmt;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::dedupe::Dedupe;
use super::gather::{self, Gather, Gathered, Gathering};
#[cfg(any(feature = "wire", feature = "sql", feature = "async"))]
use super::queue::Change;
use super::queue::Queue;
use super::quiet::{Hours, Quiet};
use super::repeat::Asked as Repeat;
use super::values::{self, Kept};
use super::write::Prepared;
use crate::clock::millis;
use crate::{Error, Result, Transaction, unix_millis};

/// An id or a group is at most this long.
const MAX_ID: usize = 1024;

/// The times a job may have: the years 1 to 9999, well before where parked
/// jobs begin.
const EARLIEST: i64 = -62_135_596_800_000;
const LATEST: i64 = 253_402_300_799_999;

/// Whether a job may have time `at`, in unix milliseconds.
pub(crate) fn in_years(at: i64) -> bool {
    (EARLIEST..=LATEST).contains(&at)
}

/// The longest span a queue's option or a repeat may give, in milliseconds:
/// from the first time a job may have to the last. One longer moves a job
/// past every time it may have.
pub(crate) const LONGEST_SPAN: i64 = LATEST - EARLIEST;

/// A call on one job with what it asks beside its value, run by its last
/// step: `later.id(&key).at(nine_am).add(&draft)`.
#[must_use = "a job's call runs at its last step: add, set or update"]
pub struct JobCall<'a, V> {
    queue: &'a Queue<V>,
    /// The transaction the call runs in, when it was made through one.
    tx: Option<&'a Transaction<'a>>,
    asked: Asked,
}

/// What a call asks beside its value.
#[derive(Debug, Default)]
pub(crate) struct Asked {
    id: Option<String>,
    when: Option<When>,
    group: Option<String>,
    repeat: Option<Repeat>,
    dedupe: Option<Dedupe>,
    keep: Option<Duration>,
    term: Option<Term>,
    quiet: Option<Quiet>,
    /// A step the chain refused, which its last step reports.
    refused: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum When {
    At(SystemTime),
    After(Duration),
}

/// When a job leaves its queue unless written again: a span from its last
/// write, or a time.
#[derive(Clone, Copy, Debug)]
enum Term {
    Ttl(Duration),
    At(SystemTime),
}

impl<'a, V> JobCall<'a, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    pub(crate) fn new(queue: &'a Queue<V>, tx: Option<&'a Transaction<'a>>) -> Self {
        JobCall { queue, tx, asked: Asked::default() }
    }

    /// The job's id, 1 to 1024 bytes of text: what finds it again, and what
    /// adds it once however many times its caller asks.
    pub fn id(mut self, id: impl AsRef<str>) -> Self {
        self.asked.id = Some(id.as_ref().to_owned());
        self
    }

    /// Runs the job at `time`; a time in the past runs it now.
    pub fn at(self, time: SystemTime) -> Self {
        self.when(When::At(time))
    }

    /// Runs the job `span` from now.
    pub fn delay(self, span: Duration) -> Self {
        self.when(When::After(span))
    }

    /// The group whose running jobs the queue's concurrency bounds.
    pub fn group(mut self, group: impl AsRef<str>) -> Self {
        self.asked.group = Some(group.as_ref().to_owned());
        self
    }

    /// Repeats the job every `span`, each id at a phase of its own within it,
    /// the same across restarts; its first run within the next interval unless
    /// `at` or `delay` says. A repeat needs an id, which alone stops it.
    pub fn every(mut self, span: Duration) -> Self {
        self.asked.repeat = Some(Repeat::Every(span));
        self
    }

    /// Repeats the job by a five-field cron expression on the wall clock of
    /// `zone`, an IANA name, `"UTC"` among them.
    pub fn cron(mut self, expr: &str, zone: &str) -> Self {
        self.asked.repeat = Some(Repeat::Cron { expr: expr.to_owned(), zone: zone.to_owned() });
        self
    }

    /// How long this job holds its id once done, in the place of its queue's:
    /// a span from when it was done, or `Dedupe::calendar(Calendar::Day,
    /// "Europe/Moscow")`, until the local day it was done in ends.
    pub fn dedupe(mut self, dedupe: impl Into<Dedupe>) -> Self {
        self.asked.dedupe = Some(dedupe.into());
        self
    }

    /// How long this job is kept once it failed for good, in the place of its
    /// queue's; `Duration::ZERO` keeps it not at all, its watchers still told
    /// it failed and why.
    pub fn keep(mut self, span: Duration) -> Self {
        self.asked.keep = Some(span);
        self
    }

    /// The job leaves its queue `span` after its last write unless written
    /// again: an `upsert`, a `set` or an `update` that names it renews it,
    /// as a key's `ttl` does. A job does not start at or after its term; a
    /// run under way finishes, and a repeat ends there.
    pub fn ttl(self, span: Duration) -> Self {
        self.term(Term::Ttl(span))
    }

    /// The hours in which this job does not run, in the place of its queue's,
    /// for its first run and every later one; `Quiet::none()` holds it by
    /// none. A time it is given inside them, `at` among them, becomes their
    /// end: the hours are a promise to the person, not a hint.
    pub fn quiet(mut self, quiet: Quiet) -> Self {
        self.asked.quiet = Some(quiet);
        self
    }

    /// The job leaves its queue at `time`, as `ttl` says; one already past
    /// is gone at once.
    pub fn expires_at(self, time: SystemTime) -> Self {
        self.term(Term::At(time))
    }

    /// Adds the job unless its id is taken: by a job scheduled, waiting or
    /// running, or done while the queue's dedupe keeps it. A job failed for
    /// good holds no id, and the new job takes its place. Says whether it
    /// added one; a job without an id is always added.
    #[expect(clippy::should_implement_trait, reason = "a queue's add puts a job in it, as BullMQ's does: no sum")]
    pub fn add(self, value: &V) -> Result<bool> {
        self.queue.add_asked(self.asked, value, self.tx)
    }

    /// Makes the id's job this value at this time, whatever it was: a job
    /// waiting is made new, a failed one starts again, and a running one runs
    /// once more after this run, with this value.
    pub fn set(self, value: &V) -> Result<()> {
        self.queue.set_asked(self.asked, value, self.tx)
    }

    /// Adds the job when its id is free, as `add` does, or brings the one
    /// there to this call: its value, group, repeat and options of its own
    /// where they differ, its time, attempts and steps as they were. A
    /// changed value drops the steps kept for the old one; a changed repeat
    /// moves a job not yet due to the repeat's next time. Says whether it
    /// added or changed anything: a job that already was so is not written,
    /// so it is cheap to call on every request. A done job its dedupe holds
    /// is left alone, `false`, as `add` leaves it.
    pub fn upsert(self, value: &V) -> Result<bool> {
        self.queue.upsert_asked(self.asked, value, self.tx)
    }

    /// Gathers `items`, a JSON list, into the id's job: a job scheduled or
    /// waiting takes them after its own, in the order they came, and is due
    /// `settle` after this gather, never later than `max` after the gather
    /// that started it. With no job there, it adds one of these items; a
    /// running one runs on with what it read, and the items gathered
    /// meanwhile are its next run's alone once it ends well, at their settle
    /// time, follow its own in its retry, and are a job of their own under
    /// the id once it fails for good. `count` is what the next run takes. A done job
    /// its dedupe holds keeps the id, and the gather adds nothing, `count` 0,
    /// as `add` would; a failed one gives the id up, as to `add`. A job that
    /// repeats, or a value that is not a list there or here, is `invalid`; a
    /// list past a job's bound a `limit`. A gather needs an id, and its time
    /// is its settle's: `at`, `delay` and a repeat are refused beside it.
    pub fn gather(self, items: &V, gather: Gather) -> Result<Gathered> {
        self.queue.gather_asked(self.asked, items, gather, self.tx)
    }

    /// Changes the id's job when it is not running: its value, the steps kept
    /// for the old one dropped with it, and its time, group or repeat when the
    /// call names them. Says whether it did: a job that runs, is done or failed,
    /// or never was answers `false`.
    pub fn update(self, value: &V) -> Result<bool> {
        self.queue.update_asked(self.asked, value, self.tx)
    }

    /// The call's write without waiting; see [`Queue::write_then`].
    #[cfg(feature = "wire")]
    pub(crate) fn write_then(self, change: Change, value: &V, done: impl FnOnce(Result<bool>) + Send + 'static) {
        self.queue.write_then(change, self.asked, value, done);
    }

    /// The call's gather checked and its value encoded now, queued later; an
    /// awaited call and the wire's are never made through a transaction.
    #[cfg(any(feature = "wire", feature = "async"))]
    pub(crate) fn gathering(self, items: &V, gather: Gather) -> super::queue::GatherWriting {
        debug_assert!(self.tx.is_none(), "a queued gather is made outside a transaction");
        self.queue.gathering(self.asked, items, gather)
    }

    /// The call's write checked and its value encoded now, queued later; see
    /// [`Queue::writing`]. An awaited call is never made through a
    /// transaction, whose calls are blocking.
    #[cfg(feature = "async")]
    pub(crate) fn writing(self, change: Change, value: &V) -> super::queue::Writing {
        debug_assert!(self.tx.is_none(), "an awaited job's call is made outside a transaction");
        self.queue.writing(change, self.asked, value)
    }

    /// The call's write for a batch of the file the queue is kept in; see
    /// [`Queue::lent`]. A call made through a transaction runs in it instead.
    #[cfg(feature = "sql")]
    pub(crate) fn lent(self, change: Change, value: &V) -> Result<crate::engine::LentWrite> {
        if self.tx.is_some() {
            return Err(Error::invalid("a job's call made through a transaction runs in it, not in a batch"));
        }
        self.queue.lent(change, self.asked, value)
    }

    fn term(mut self, term: Term) -> Self {
        if self.asked.term.is_some() {
            self.asked.refused = Some("a term given twice: ttl or expires_at, not both".to_owned());
        }
        self.asked.term = Some(term);
        self
    }

    fn when(mut self, when: When) -> Self {
        if self.asked.when.is_some() {
            self.asked.refused = Some("a time given twice: at or delay, not both".to_owned());
        }
        self.asked.when = Some(when);
        self
    }
}

impl<V> fmt::Debug for JobCall<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobCall").field("asked", &self.asked).finish()
    }
}

impl Asked {
    /// The options of a job's own, as a write passed on carries them after
    /// its JSON, so that a home of a release before refuses the write rather
    /// than drop them: a dedupe, 0 none, 1 a span, 2 a calendar's unit and
    /// zone; then a keep, 0 none or 1 and its span; then a term, 0 none, 1
    /// a ttl's span or 2 a time; then quiet hours, 0 none or 1 and their text.
    /// Nothing when none is given. Hours that do not check are `invalid` here,
    /// before the write leaves, as the home would find them.
    #[cfg(feature = "replication")]
    pub(crate) fn write_own(&self, out: &mut crate::replication::Out) -> Result<()> {
        let quiet = self.quiet.as_ref().map(Quiet::kept).transpose()?;
        if self.dedupe.is_none() && self.keep.is_none() && self.term.is_none() && quiet.is_none() {
            return Ok(());
        }
        match &self.dedupe {
            None => out.byte(0),
            Some(dedupe) => out.byte(1).text(&dedupe.text()),
        };
        match self.keep {
            None => out.byte(0),
            Some(span) => out.byte(1).span(span),
        };
        match self.term {
            None => out.byte(0),
            Some(Term::Ttl(span)) => out.byte(1).span(span),
            Some(Term::At(time)) => out.byte(2).long(unix_millis(time) as u64),
        };
        match &quiet {
            None => out.byte(0),
            Some(quiet) => out.byte(1).text(quiet),
        };
        Ok(())
    }

    /// Reads what [`Asked::write_own`] wrote, when the write carries it.
    #[cfg(feature = "replication")]
    pub(crate) fn read_own(&mut self, at: &mut crate::replication::In<'_>) -> Result<()> {
        if at.done() {
            return Ok(());
        }
        self.dedupe = match at.byte()? {
            0 => None,
            1 => Some(Dedupe::parse(&at.text()?)?),
            _ => return Err(super::forward::corrupt("a job's dedupe")),
        };
        self.keep = match at.byte()? {
            0 => None,
            1 => Some(at.span()?),
            _ => return Err(super::forward::corrupt("a job's keep")),
        };
        if at.done() {
            return Ok(());
        }
        self.term = match at.byte()? {
            0 => None,
            1 => Some(Term::Ttl(at.span()?)),
            2 => Some(Term::At(crate::clock::from_unix_millis(at.long()? as i64))),
            _ => return Err(super::forward::corrupt("a job's term")),
        };
        if at.done() {
            return Ok(());
        }
        self.quiet = match at.byte()? {
            0 => None,
            1 => Some(Quiet::kept_as(&at.text()?).map_err(|_| super::forward::corrupt("a job's quiet hours"))?),
            _ => return Err(super::forward::corrupt("a job's quiet hours")),
        };
        Ok(())
    }

    /// What was asked, as a write passed on carries it: each part a byte
    /// saying whether it is there, then it.
    #[cfg(feature = "replication")]
    pub(crate) fn write(&self, out: &mut crate::replication::Out) {
        out.maybe_text(self.id.as_deref()).maybe_text(self.group.as_deref()).maybe_text(self.refused.as_deref());
        match self.when {
            None => out.byte(0),
            Some(When::At(time)) => out.byte(1).long(unix_millis(time) as u64),
            Some(When::After(span)) => out.byte(2).span(span),
        };
        match &self.repeat {
            None => out.byte(0),
            Some(repeat) => repeat.write(out),
        };
    }

    #[cfg(feature = "replication")]
    pub(crate) fn read(at: &mut crate::replication::In<'_>) -> Result<Asked> {
        let (id, group, refused) = (at.maybe_text()?, at.maybe_text()?, at.maybe_text()?);
        let when = match at.byte()? {
            0 => None,
            1 => Some(When::At(crate::clock::from_unix_millis(at.long()? as i64))),
            2 => Some(When::After(at.span()?)),
            _ => return Err(super::forward::corrupt("a job's time")),
        };
        let repeat = match at.byte()? {
            0 => None,
            tag => Some(Repeat::read(tag, at)?),
        };
        Ok(Asked { id, when, group, repeat, refused, ..Asked::default() })
    }

    pub(crate) fn with_id(id: &str) -> Asked {
        Asked { id: Some(id.to_owned()), ..Asked::default() }
    }

    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// A gather's call checked and its value encoded, as [`Asked::prepare`]
    /// does, its time the settle's.
    pub(crate) fn prepare_gather<V: Serialize>(
        self,
        items: &V,
        asked: Gather,
        (now, queue): (i64, Option<&Quiet>),
    ) -> Result<Prepared> {
        let beside = match (&self.id, &self.when, &self.repeat) {
            (None, _, _) => Some("a gather needs an id, which names the job it gathers into"),
            (_, Some(_), _) => Some("a gather's time is its settle's: no at or delay beside it"),
            (_, _, Some(_)) => Some("a gathered job runs once it settles: no repeat beside it"),
            _ => None,
        };
        if let Some(why) = beside {
            return Err(Error::invalid(why));
        }
        let (settled, latest) = asked.times(now)?;
        let mut call = self.prepare(items, (now, queue))?;
        let count = gather::count(call.value.bytes())?;
        call.at = call.held(latest.map_or(settled, |latest| latest.min(settled)));
        if !in_years(call.at) {
            return Err(Error::invalid("a time outside the years 1 to 9999"));
        }
        call.gather = Some(Gathering { settled, latest, count });
        Ok(call)
    }

    /// Checks what was asked and gathers what the write needs, the value
    /// encoded, before the write waits for the writer.
    pub(crate) fn prepare<V: Serialize>(self, value: &V, (now, queue): (i64, Option<&Quiet>)) -> Result<Prepared> {
        if let Some(refused) = self.refused {
            return Err(Error::invalid(refused));
        }
        let (quiet, hours) = quiet_of(self.quiet.as_ref(), queue)?;
        for (what, text) in [("an id", &self.id), ("a group", &self.group)] {
            if let Some(text) = text
                && (text.is_empty() || text.len() > MAX_ID)
            {
                return Err(Error::invalid(format!("{what} of {} bytes, not 1 to {MAX_ID}", text.len())));
            }
        }
        let repeat = match (&self.repeat, &self.id) {
            (Some(_), None) => return Err(Error::invalid("a repeat without an id, which alone could stop it")),
            (Some(repeat), Some(id)) => Some(repeat.of(id)?),
            (None, _) => None,
        };
        let unheld = match self.when {
            Some(When::At(time)) => unix_millis(time),
            Some(When::After(span)) => now.saturating_add(millis(span)),
            None => repeat.as_ref().and_then(|repeat| repeat.next(now)).unwrap_or(now),
        };
        let at = hours.as_ref().map_or(unheld, |hours| hours.hold(unheld));
        if !in_years(at) {
            return Err(Error::invalid("a time outside the years 1 to 9999"));
        }
        if let Some(dedupe) = &self.dedupe {
            dedupe.check(false)?;
        }
        let keep = self.keep.map(millis);
        if keep.is_some_and(|keep| keep > LONGEST_SPAN) {
            return Err(Error::invalid("a keep longer than the years 1 to 9999"));
        }
        let (expires, ttl) = match self.term {
            None => (None, None),
            Some(Term::Ttl(span)) if span.is_zero() => {
                return Err(Error::invalid("a ttl of 0, which ends the job at once"));
            }
            Some(Term::Ttl(span)) => (Some(now.saturating_add(millis(span))), Some(millis(span))),
            Some(Term::At(time)) => (Some(unix_millis(time)), None),
        };
        if expires.is_some_and(|expires| !in_years(expires)) {
            return Err(Error::invalid("a term outside the years 1 to 9999"));
        }
        Ok(Prepared {
            key: self.id,
            at,
            unheld,
            timed: self.when.is_some(),
            repeated: repeat.as_ref().and_then(|repeat| repeat.next(now)),
            repeat: repeat.map(|repeat| repeat.text()),
            group: self.group,
            dedupe: self.dedupe.map(|dedupe| dedupe.text()),
            keep,
            expires,
            ttl,
            value: Kept::of(values::encode(value)?),
            gather: None,
            quiet,
            hours,
            now,
        })
    }
}

/// What a job keeps of a call's quiet hours, and the hours that hold its
/// time: the call's own, or its queue's when it gives none.
fn quiet_of(own: Option<&Quiet>, queue: Option<&Quiet>) -> Result<(Option<String>, Option<Hours>)> {
    match own {
        Some(own) => Ok((Some(own.kept()?), own.checked()?)),
        None => Ok((None, queue.map(Quiet::checked).transpose()?.flatten())),
    }
}

#[cfg(all(test, feature = "replication"))]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use crate::replication::{In, Out};

    #[test]
    fn a_write_passed_on_refuses_hours_that_do_not_check_before_it_leaves() {
        for quiet in [Quiet::hours("7:00", "08:00", "UTC"), Quiet::hours("23:00", "07:00", "Mars/Olympus")] {
            let asked = Asked { quiet: Some(quiet), ..Asked::default() };
            let refused = asked.write_own(&mut Out(Vec::new())).unwrap_err();
            assert_eq!(refused.kind(), ErrorKind::Invalid, "{refused}");
        }
        let night = Quiet::hours("23:00", "07:00", "UTC");
        let mut out = Out(Vec::new());
        Asked { quiet: Some(night.clone()), ..Asked::default() }.write_own(&mut out).unwrap();
        let mut read = Asked::default();
        read.read_own(&mut In { bytes: &out.0, at: 0 }).unwrap();
        assert_eq!(read.quiet, Some(night));
    }
}
