//! Reading ahead: the pages that follow one another from where `all` or a
//! count stands, read at once on several threads while they come back
//! holding fewer events than a page, as a search for a rare word does.
//!
//! ```text
//! this thread  cut ranges from the index of times ─► hand them out ─► take each back, in order ─► a page's worth
//! threads      take the next range ─► read it page after page, each page in a snapshot of its own ─► the next
//! ```
//!
//! A range holds the rows one page fetches, so it is read as a page is, and
//! holds the store's memory as a page does while it is read. The ranges
//! follow one another in time and no time is in two: each event is in one
//! range, as it is in one of the pages read one after another, and they are
//! taken back in the order of their times.
//!
//! The ranges handed out are `AHEAD` a thread at most, and only as many as
//! the events a page still wants need at the rate the ranges taken back
//! held them: a page's worth of events comes back with little more beside
//! it. What waits to be taken back holds none of the store's memory, as a
//! page handed back holds none. The threads are the call's own and end
//! with it, and read under its cancellation tokens: a cancel stops their
//! reads as it stops the call's own, rather than after every range handed
//! out was read.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::Scope;

use super::engine::{Records, Stream};
use super::event::Event;
use super::read::{Asked, Page, Query, written};
use crate::cancel;
use crate::{Error, Result};

/// The ranges handed out at once for each thread that reads them, so that a
/// thread done with one finds another while the one before it is read.
const AHEAD: usize = 2;
/// The ranges one read of the index cuts, for each handed out at once.
const CUT: usize = 2;

/// The ranges a thread read ahead under a token of its call, which a test
/// looks at: whether a thread reads under them shows only in a cancel that
/// lands while it reads.
#[cfg(test)]
static RANGES_UNDER_TOKENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Records {
    /// The pages that follow one another from where a read stands, as one,
    /// and how many ranges the next read of them begins with at once.
    ///
    /// With `wide` 1 it reads one page alone. Otherwise it hands out `wide`
    /// ranges, then after each range taken back as many as the events still
    /// wanted need at the rate the ranges taken back held them. Once a page's
    /// worth is back it hands out no more, and takes back those still being
    /// read rather than read them again next time.
    pub(super) fn read_ahead(&self, stream: Option<&Stream>, query: &Query, wide: usize) -> Result<(Page, usize)> {
        let (limit, most) = (query.page_events(), AHEAD * self.threads());
        if wide <= 1 {
            let page = self.page(stream, query)?;
            let wide = ranges_for(limit, page.events.len(), 1, most);
            return Ok((page, wide));
        }
        let Some(asked) = self.asked(stream, query)? else { return Ok((Page { events: Vec::new(), next: None }, 1)) };
        let (handed, mut cut, tokens) = (Handed::default(), Cut::new(&asked), cancel::captured());
        let events = std::thread::scope(|scope| {
            let _closed = handed.closing();
            let mut threads = Threads { scope, started: 0, most: self.threads() };
            let mut events = Vec::new();
            loop {
                let wanted = limit.saturating_sub(events.len());
                let wide =
                    if cut.back == 0 { wide.min(most) } else { ranges_for(wanted, events.len(), cut.back, most) };
                while wanted > 0
                    && cut.out < wide
                    && let Some(range) = cut.hand(self, wide)?
                {
                    handed.give(range);
                    threads.start_for(cut.out, || tokens.run(None, || self.take_ranges(&handed, stream, query)))?;
                }
                if cut.out == 0 {
                    return Ok(events);
                }
                match handed.take_back() {
                    Ok(more) => {
                        events.extend(more);
                        cut.taken();
                    }
                    Err(error) if events.is_empty() => return Err(error),
                    // what came before it comes back first, as from pages one after another, and the next read fails
                    Err(_) => return Ok(events),
                }
            }
        })?;
        let next = cut.after().map(|start| written(asked.from(start)));
        let wide = ranges_for(limit, events.len(), cut.back, most);
        Ok((Page { events, next }, wide))
    }

    /// Reads the ranges handed out, one after another, until none is left.
    fn take_ranges(&self, handed: &Handed, stream: Option<&Stream>, query: &Query) {
        while let Some((place, range)) = handed.take() {
            #[cfg(test)]
            if cancel::in_scope() {
                RANGES_UNDER_TOKENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            let mut reading = Reading { handed, place, read: None };
            reading.read = Some(self.read_range(stream, query, range));
        }
    }

    /// The events of a range, both ends in it, read page after page.
    fn read_range(&self, stream: Option<&Stream>, query: &Query, range: (i64, i64)) -> Result<Vec<Event>> {
        let mut query = query.over(written(range));
        let mut events = Vec::new();
        loop {
            let page = self.page(stream, &query)?;
            events.extend(page.events);
            let Some(next) = page.next else { return Ok(events) };
            query = query.over(next);
        }
    }

    /// The threads one read takes at most: one a processor, and no more than
    /// the readers of records.db, since each holds one while it fetches.
    pub(super) fn threads(&self) -> usize {
        static PROCESSORS: OnceLock<usize> = OnceLock::new();
        let processors =
            *PROCESSORS.get_or_init(|| std::thread::available_parallelism().map_or(1, std::num::NonZero::get));
        processors.min(self.file.readers_at_most())
    }
}

/// The ranges to read at once for `wanted` events when `taken` ranges held
/// `found`: as many as hold them at that rate, and `most` while none held
/// any.
fn ranges_for(wanted: usize, found: usize, taken: usize, most: usize) -> usize {
    if found == 0 { most } else { (wanted * taken).div_ceil(found).clamp(1, most) }
}

/// The threads a read ahead starts, one as each range handed out needs one.
struct Threads<'scope, 'env> {
    scope: &'scope Scope<'scope, 'env>,
    started: usize,
    /// The threads it may start: the read's, or those the system started
    /// once it refused another.
    most: usize,
}

impl<'scope> Threads<'scope, '_> {
    /// Starts a thread doing `work` while fewer have started than the `out`
    /// ranges handed out.
    fn start_for(&mut self, out: usize, work: impl FnOnce() + Send + 'scope) -> Result<()> {
        if self.started >= out.min(self.most) {
            return Ok(());
        }
        let thread = std::thread::Builder::new().name("tinystore-records-read".to_owned());
        match thread.spawn_scoped(self.scope, work) {
            Ok(_) => self.started += 1,
            Err(error) if self.started == 0 => return Err(Error::io("records: a thread to read on", error)),
            // the ranges wait for the threads already reading
            Err(_) => self.most = self.started,
        }
        Ok(())
    }
}

/// The ranges a read is cut into from where it stands, a few at a time from
/// the index of times.
struct Cut<'a> {
    asked: &'a Asked,
    /// The ranges cut and not taken back yet, the next to take back first,
    /// each after the time it begins at.
    ranges: VecDeque<(i64, (i64, i64))>,
    /// How many of them are handed out, and how many were taken back before
    /// them.
    out: usize,
    back: usize,
    /// Where the range after them begins; none once one reaches the end of
    /// the read.
    next: Option<i64>,
}

impl<'a> Cut<'a> {
    fn new(asked: &'a Asked) -> Cut<'a> {
        Cut { asked, ranges: VecDeque::new(), out: 0, back: 0, next: Some(asked.start()) }
    }

    /// The next range to hand out, `CUT` times `wide` more cut when none is
    /// left; none once the read is cut to its end and all are handed out.
    fn hand(&mut self, records: &Records, wide: usize) -> Result<Option<(i64, i64)>> {
        if self.out == self.ranges.len()
            && let Some(start) = self.next
        {
            let bounds = records.bounds(self.asked, start, CUT * wide)?;
            let mut begins = start;
            for bound in &bounds {
                self.ranges.push_back((begins, self.asked.between(begins, *bound)));
                begins = *bound;
            }
            self.next = Some(begins);
            if bounds.len() < CUT * wide {
                self.ranges.push_back((begins, self.asked.from(begins)));
                self.next = None;
            }
        }
        let range = self.ranges.get(self.out).map(|(_, range)| *range);
        self.out += usize::from(range.is_some());
        Ok(range)
    }

    /// Forgets the first range, taken back.
    fn taken(&mut self) {
        self.ranges.pop_front();
        self.out -= 1;
        self.back += 1;
    }

    /// Where the first range not taken back begins; none past the end.
    fn after(&self) -> Option<i64> {
        self.ranges.front().map(|(begins, _)| *begins).or(self.next)
    }
}

/// The ranges handed to the threads that read them, and what each read.
#[derive(Default)]
struct Handed {
    hands: Mutex<Hands>,
    changed: Condvar,
}

#[derive(Default)]
struct Hands {
    /// The ranges handed out and not taken by a thread yet, each after its
    /// place among them.
    waiting: VecDeque<(usize, (i64, i64))>,
    /// What each range read holds, by its place, until it is taken back.
    read: HashMap<usize, Result<Vec<Event>>>,
    /// The places of the next range handed out and of the next taken back.
    given: usize,
    returned: usize,
    /// No range is handed out after it: a thread leaves once none waits.
    closed: bool,
}

impl Handed {
    fn give(&self, range: (i64, i64)) {
        let mut hands = self.lock();
        let place = hands.given;
        hands.given += 1;
        hands.waiting.push_back((place, range));
        drop(hands);
        self.changed.notify_all();
    }

    /// The next range for a thread to read, waiting for one: none once the
    /// hand-out is closed.
    fn take(&self) -> Option<(usize, (i64, i64))> {
        let mut hands = self.lock();
        loop {
            if let Some(next) = hands.waiting.pop_front() {
                return Some(next);
            }
            if hands.closed {
                return None;
            }
            hands = self.changed.wait(hands).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn put(&self, place: usize, read: Result<Vec<Event>>) {
        self.lock().read.insert(place, read);
        self.changed.notify_all();
    }

    /// What the first range handed out and not taken back holds, waiting
    /// until it is read.
    fn take_back(&self) -> Result<Vec<Event>> {
        let mut hands = self.lock();
        loop {
            let place = hands.returned;
            if let Some(read) = hands.read.remove(&place) {
                hands.returned += 1;
                return read;
            }
            hands = self.changed.wait(hands).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// What closes the hand-out when it is dropped, however the read ends,
    /// so that every thread leaves and the read's scope can end.
    fn closing(&self) -> Closing<'_> {
        Closing(self)
    }

    fn lock(&self) -> MutexGuard<'_, Hands> {
        self.hands.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Closing<'h>(&'h Handed);

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        let mut hands = self.0.lock();
        hands.closed = true;
        hands.waiting.clear();
        drop(hands);
        self.0.changed.notify_all();
    }
}

/// A range a thread reads, put back when dropped: what it read, or a failure
/// when the thread unwound before it was read, so that the thread taking
/// the ranges back never waits for it.
struct Reading<'h> {
    handed: &'h Handed,
    place: usize,
    read: Option<Result<Vec<Event>>>,
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        let unread = || Err(Error::internal("records: a thread reading ahead stopped before its range was read"));
        self.handed.put(self.place, self.read.take().unwrap_or_else(unread));
    }
}

#[cfg(test)]
#[path = "ahead_tests.rs"]
mod tests;
