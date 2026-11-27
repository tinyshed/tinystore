//! Locks and semaphores: a key that one holder at a time, or `limit`, may
//! hold while a function runs, across every process of a store. They live in
//! the memory of the process that holds the store, which sees every holder;
//! kv.db keeps the epoch their fences start from. docs/guides/locks/README.md is the
//! book.
//!
//! ```no_run
//! # fn main() -> tinystore::Result<()> {
//! # let store = tinystore::Store::open("data", Default::default())?;
//! let invoices = store.lock("invoices").open()?;
//! let charged = invoices.hold("42", |lease| -> tinystore::Result<String> {
//!     Ok(format!("charged with fence {}", lease.fence())) // nobody else holds key 42 meanwhile
//! })?;
//!
//! // eight places, background work on four of them at most, behind every urgent hold
//! let cabinet = store.semaphore("cabinet").limit(8).background(4).open()?;
//! let synced = cabinet
//!     .key("requests")
//!     .background()
//!     .timeout(std::time::Duration::from_secs(15)) // from the grant: past it the hold is lost
//!     .hold(|_| -> tinystore::Result<&str> { Ok("synced") })?;
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "async")]
mod awaited;
mod breaker;
mod engine;
mod lease;
mod state;

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

#[cfg(feature = "async")]
pub(crate) use self::awaited::{Ends, Held};
pub use self::breaker::KeyState;
pub(crate) use self::breaker::Outcome;
use self::breaker::Rule;
pub(crate) use self::engine::Locks;
use self::engine::Shape;
pub use self::lease::Fence;
pub(crate) use self::lease::{Answer, Granted};
use self::lease::{Ask, Ending, Kind, Place, Priority};
use super::scope::check_name;
use crate::{Error, ErrorKind, Key, Result, Store, sqlite};

/// How long a hold waits in line unless `wait` says.
const WAIT: Duration = Duration::from_secs(10);
/// The longest key, in bytes.
const MOST_KEY: usize = 1024;
/// The most holders a semaphore's key takes.
const MOST_LIMIT: u32 = 1000;

/// A lock being opened.
#[must_use = "a lock opens with open()"]
pub struct LockBuilder {
    store: Store,
    name: String,
    max_hold: Option<Duration>,
}

/// A semaphore being opened.
#[must_use = "a semaphore opens with open()"]
pub struct SemaphoreBuilder {
    store: Store,
    name: String,
    limit: Option<u32>,
    background: Option<u32>,
    breaker: Option<Rule>,
    max_hold: Option<Duration>,
}

impl Store {
    /// Keys held one holder at a time, across every process of the store:
    /// `hold` waits for a key, runs a function and lets the key go.
    pub fn lock(&self, name: &str) -> LockBuilder {
        LockBuilder { store: self.clone(), name: name.to_owned(), max_hold: None }
    }

    /// Keys held by up to `limit` holders at a time.
    pub fn semaphore(&self, name: &str) -> SemaphoreBuilder {
        SemaphoreBuilder {
            store: self.clone(),
            name: name.to_owned(),
            limit: None,
            background: None,
            breaker: None,
            max_hold: None,
        }
    }
}

impl LockBuilder {
    /// How long one lease may last from its grant: past it the key is given
    /// to the next in line and the holder told it lost it. Off unless given.
    pub fn max_hold(mut self, bound: Duration) -> Self {
        self.max_hold = Some(bound);
        self
    }

    pub fn open(self) -> Result<Lock> {
        Ok(Lock { opened: Opened::open(&self.store, Kind::Lock, &self.name, 1, self.max_hold)? })
    }
}

impl SemaphoreBuilder {
    /// How many may hold a key at once, from 1 to 1000. A name has one limit
    /// in a program: an open with another limit than the name's first open
    /// in the program gave is `Invalid`. Another program's holds, a client's
    /// over a connection, are each admitted by their own limit.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// How many of a key's places background holds may take at once, from 1
    /// to the limit, so that urgent holds always find the rest; background
    /// holds may take every place unless given. A name keeps one in a
    /// program, as it keeps its limit.
    pub fn background(mut self, places: u32) -> Self {
        self.background = Some(places);
        self
    }

    /// Puts a key down for `pause` once `failures` holds of it failed in a
    /// row: every ask of it, waiting or new, is then refused at once
    /// `Unavailable`. When the pause is over, one ask goes through. Its
    /// success puts the key up again, and its failure pauses it again.
    ///
    /// A hold failed when its work answered an error, unless its permit said
    /// [`Permit::healthy`], or when it ran past its timeout or `max_hold`. A
    /// success resets the count. The breaker lives in the memory of the
    /// process that holds the store, beside the lines: every program of the
    /// store sees one answer, and a restart of that process starts every key
    /// up. A name keeps one breaker in a program, as it keeps its limit.
    pub fn breaker(mut self, failures: u32, pause: Duration) -> Self {
        self.breaker = Some(Rule { failures, pause });
        self
    }

    pub fn max_hold(mut self, bound: Duration) -> Self {
        self.max_hold = Some(bound);
        self
    }

    pub fn open(self) -> Result<Semaphore> {
        let shown = || format!("semaphore {}", self.name);
        let limit = self.limit.ok_or_else(|| Error::invalid("its limit is given with limit()").within(shown()))?;
        if !(1..=MOST_LIMIT).contains(&limit) {
            return Err(Error::invalid(format!("a limit of {limit}, not 1 to {MOST_LIMIT}")).within(shown()));
        }
        if let Some(places) = self.background.filter(|places| !(1..=limit).contains(places)) {
            return Err(Error::invalid(format!("a background of {places}, not 1 to its limit {limit}")).within(shown()));
        }
        check_breaker(self.breaker).map_err(|error| error.within(shown()))?;
        let mut opened = Opened::open(&self.store, Kind::Semaphore, &self.name, limit, self.max_hold)?;
        opened.background = self.background;
        opened.breaker = self.breaker;
        let shape = Shape { limit, background: self.background, breaker: self.breaker };
        opened.locks.keep_shape(&self.name, shape).map_err(|error| error.within(shown()))?;
        Ok(Semaphore { opened })
    }
}

/// A lock's keys, each held by one holder at a time.
#[derive(Clone)]
pub struct Lock {
    opened: Opened,
}

/// A semaphore's keys, each held by up to its limit at a time.
#[derive(Clone)]
pub struct Semaphore {
    opened: Opened,
}

impl Lock {
    /// Waits for `key`, ten seconds at most, holds it while `work` runs and
    /// lets it go, answering what `work` answered. A key still held after the
    /// wait is `in use`; a lease lost while `work` ran is `lost`, even when
    /// `work` succeeded.
    pub fn hold<T, E: From<Error>>(&self, key: impl Key, work: impl FnOnce(&Lease) -> Result<T, E>) -> Result<T, E> {
        self.key(key).hold(work)
    }

    /// Holds `key` while `work` runs if nobody holds it, and answers `None`
    /// without waiting if somebody does.
    pub fn try_hold<T, E: From<Error>>(
        &self,
        key: impl Key,
        work: impl FnOnce(&Lease) -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        self.key(key).try_hold(work)
    }

    /// A key, for a hold that says how long it waits.
    pub fn key(&self, key: impl Key) -> LockKey<'_> {
        LockKey { opened: &self.opened, key: key.text().into_owned(), terms: Terms::default() }
    }
}

impl Semaphore {
    pub fn hold<T, E: From<Error>>(&self, key: impl Key, work: impl FnOnce(&Permit) -> Result<T, E>) -> Result<T, E> {
        self.key(key).hold(work)
    }

    pub fn try_hold<T, E: From<Error>>(
        &self,
        key: impl Key,
        work: impl FnOnce(&Permit) -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        self.key(key).try_hold(work)
    }

    /// A key, for a hold that says how long it waits, whether it waits in
    /// the background and how long it may keep its place.
    pub fn key(&self, key: impl Key) -> SemaphoreKey<'_> {
        SemaphoreKey { opened: &self.opened, key: key.text().into_owned(), terms: Terms::default() }
    }

    /// How `key` stands now: up or down and since when, its last failed
    /// hold, and how many wait for it and hold it. Up always without a
    /// breaker.
    pub fn state(&self, key: impl Key) -> Result<KeyState> {
        let place = self.opened.place(&key.text())?;
        Ok(self.opened.locks.state(&place))
    }
}

/// Refuses a breaker that would never trip or never pause.
fn check_breaker(breaker: Option<Rule>) -> Result<()> {
    match breaker {
        Some(rule) if rule.failures == 0 => Err(Error::invalid("a breaker of zero failures")),
        Some(rule) if rule.pause.is_zero() => Err(Error::invalid("a breaker's pause of zero")),
        _ => Ok(()),
    }
}

/// A key of a lock, for a hold that says how long it waits.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct LockKey<'a> {
    opened: &'a Opened,
    key: String,
    terms: Terms,
}

/// A key of a semaphore, for a hold that says how long it waits, at which
/// priority, and how long it may keep its place.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct SemaphoreKey<'a> {
    opened: &'a Opened,
    key: String,
    terms: Terms,
}

/// How a hold asks for its key.
#[derive(Clone, Copy)]
struct Terms {
    wait: Duration,
    priority: Priority,
    /// How long the hold may keep its place from its grant: a `max_hold` of
    /// this hold alone.
    timeout: Option<Duration>,
}

impl Default for Terms {
    fn default() -> Terms {
        Terms { wait: WAIT, priority: Priority::Urgent, timeout: None }
    }
}

impl LockKey<'_> {
    /// How long the hold waits in line; ten seconds unless said.
    pub fn wait(mut self, wait: Duration) -> Self {
        self.terms.wait = wait;
        self
    }

    pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Lease) -> Result<T, E>) -> Result<T, E> {
        let holding = self.opened.hold(&self.key, self.terms)?;
        let lease = Lease { fence: holding.granted.fence, holding: &holding };
        holding.run(|| work(&lease))
    }

    pub fn try_hold<T, E: From<Error>>(self, work: impl FnOnce(&Lease) -> Result<T, E>) -> Result<Option<T>, E> {
        let Some(holding) = self.opened.try_hold(&self.key, self.terms)? else { return Ok(None) };
        let lease = Lease { fence: holding.granted.fence, holding: &holding };
        holding.run(|| work(&lease)).map(Some)
    }
}

impl SemaphoreKey<'_> {
    pub fn wait(mut self, wait: Duration) -> Self {
        self.terms.wait = wait;
        self
    }

    /// Waits behind every urgent hold of the key, and takes no more of its
    /// places than the semaphore's `background`. Nothing promises it a place:
    /// while urgent holds keep coming and take the room, it waits, `wait` at
    /// most.
    pub fn background(mut self) -> Self {
        self.terms.priority = Priority::Background;
        self
    }

    /// How long the hold may keep its place, from its grant: past it the
    /// place goes to the next in line and the hold fails `lost`, as at the
    /// semaphore's `max_hold`, the shorter of the two counting.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.terms.timeout = Some(timeout);
        self
    }

    pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Permit) -> Result<T, E>) -> Result<T, E> {
        let holding = self.opened.hold(&self.key, self.terms)?;
        holding.run(|| work(&Permit { holding: &holding }))
    }

    pub fn try_hold<T, E: From<Error>>(self, work: impl FnOnce(&Permit) -> Result<T, E>) -> Result<Option<T>, E> {
        let Some(holding) = self.opened.try_hold(&self.key, self.terms)? else { return Ok(None) };
        holding.run(|| work(&Permit { holding: &holding })).map(Some)
    }
}

/// A holding of a lock's key, given to the function that holds it.
pub struct Lease<'a> {
    fence: Fence,
    holding: &'a Holding,
}

impl Lease<'_> {
    /// The lease's fence: hand it to whatever must refuse a late holder.
    pub fn fence(&self) -> Fence {
        self.fence
    }

    /// Whether the lease ended while its function runs: revoked at its
    /// `max_hold`, or the store closed. What the function does from then on
    /// may run beside another holder.
    pub fn lost(&self) -> bool {
        self.holding.ended()
    }

    /// How long the hold waited for the key, from its call to the grant.
    pub fn waited(&self) -> Duration {
        self.holding.waited
    }
}

/// A holding of a semaphore's key: one of its limit.
pub struct Permit<'a> {
    holding: &'a Holding,
}

impl Permit<'_> {
    pub fn lost(&self) -> bool {
        self.holding.ended()
    }

    /// Says the guarded service is healthy, so that the semaphore's breaker
    /// does not count the hold a failure though its work then answers an
    /// error: a 404 that is the caller's, not the service's.
    pub fn healthy(&self) {
        self.holding.granted.watch.healthy();
    }

    /// How long the hold waited for its place, from its call to the grant.
    pub fn waited(&self) -> Duration {
        self.holding.waited
    }
}

/// What a lock and a semaphore share: the store's locks, the name, and what
/// a lease of theirs is.
#[derive(Clone)]
struct Opened {
    locks: Arc<Locks>,
    kind: Kind,
    name: Arc<str>,
    limit: u32,
    background: Option<u32>,
    breaker: Option<Rule>,
    max_hold: Option<Duration>,
}

impl Opened {
    fn open(store: &Store, kind: Kind, name: &str, limit: u32, max_hold: Option<Duration>) -> Result<Opened> {
        let shown = || format!("{} {name}", if kind == Kind::Lock { "lock" } else { "semaphore" });
        check_name(name).map_err(|error| error.within(shown()))?;
        if max_hold.is_some_and(|bound| bound.is_zero()) {
            return Err(Error::invalid("a max_hold of zero").within(shown()));
        }
        let locks = Locks::of(store)?;
        Ok(Opened { locks, kind, name: Arc::from(name), limit, background: None, breaker: None, max_hold })
    }

    fn place(&self, key: &str) -> Result<Place> {
        let place = Place::new(self.kind, Arc::clone(&self.name), key.to_owned());
        if key.is_empty() || key.len() > MOST_KEY {
            return Err(Error::invalid(format!("a key is 1 to {MOST_KEY} bytes")).within(place));
        }
        Ok(place)
    }

    /// Holds `key` once it is free, its terms' `wait` at most.
    fn hold(&self, key: &str, terms: Terms) -> Result<Holding> {
        let place = self.place(key)?;
        match self.ask(&place, terms)? {
            Some(holding) => Ok(holding),
            None => {
                let wait = terms.wait;
                Err(Error::new(ErrorKind::InUse, format!("held past a wait of {wait:?}")).within(place))
            }
        }
    }

    fn try_hold(&self, key: &str, terms: Terms) -> Result<Option<Holding>> {
        let place = self.place(key)?;
        self.ask(&place, Terms { wait: Duration::ZERO, ..terms })
    }

    /// What a hold of `place` on these terms asks the store's locks for.
    fn ask_of(&self, place: &Place, terms: Terms) -> Result<Ask> {
        if terms.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(Error::invalid("a timeout of zero").within(place));
        }
        let max_hold = match (self.max_hold, terms.timeout) {
            (Some(bound), Some(timeout)) => Some(bound.min(timeout)),
            (bound, timeout) => bound.or(timeout),
        };
        Ok(Ask {
            place: place.clone(),
            limit: self.limit,
            priority: terms.priority,
            background: self.background,
            ttl: None,
            max_hold,
            wait: terms.wait,
            request: None,
            breaker: self.breaker,
            probe: false,
        })
    }

    /// Asks for `place` and waits for the answer; none when it stayed busy.
    fn ask(&self, place: &Place, terms: Terms) -> Result<Option<Holding>> {
        let asked = Instant::now();
        let ask = self.ask_of(place, terms)?;
        crate::polled::may_wait(|| format!("{place}: a hold"))?;
        refuse_where_a_hold_would_wait_for_itself(&self.locks, place)?;
        let mark = Mark::enter(&self.locks, place);
        let (answered, answer) = mpsc::sync_channel(1);
        self.locks.ask(ask, Box::new(move |answer| drop(answered.send(answer))));
        let answer = answer.recv().map_err(|_| Error::internal("a lock's answer was dropped").within(place))?;
        Ok(match answer? {
            Answer::Granted(granted) => Some(Holding {
                locks: Arc::downgrade(&self.locks),
                place: place.clone(),
                granted,
                waited: asked.elapsed(),
                finished: Finished::new(Outcome::Failed),
                _mark: Some(mark),
            }),
            Answer::Busy => None,
        })
    }
}

/// What a holder through a connection asks for, as the wire carries it.
#[cfg(feature = "wire")]
pub(crate) struct Remote<'a> {
    pub(crate) kind: &'a str,
    pub(crate) name: &'a str,
    pub(crate) key: &'a str,
    pub(crate) limit: u32,
    /// `urgent` or `background`; empty is urgent.
    pub(crate) priority: &'a str,
    /// Zero for none.
    pub(crate) background: u32,
    /// The breaker's failures in a row, zero for none, and its pause.
    pub(crate) failures: u32,
    pub(crate) pause: Option<Duration>,
    pub(crate) ttl: Duration,
    pub(crate) max_hold: Option<Duration>,
    pub(crate) wait: Duration,
    pub(crate) request: [u8; 16],
}

/// The ask of a holder through a connection, checked as a handle's is, with
/// the store's locks it goes to.
#[cfg(feature = "wire")]
pub(crate) fn remote_ask(store: &Store, remote: &Remote<'_>) -> Result<(Arc<Locks>, Ask)> {
    let kind = match remote.kind {
        "lock" if remote.limit == 1 => Kind::Lock,
        "semaphore" if (1..=MOST_LIMIT).contains(&remote.limit) => Kind::Semaphore,
        other => return Err(Error::invalid(format!("a {other} of limit {}", remote.limit))),
    };
    let opened = Opened::open(store, kind, remote.name, remote.limit, remote.max_hold)?;
    let place = opened.place(remote.key)?;
    if !(Duration::from_secs(1)..=Duration::from_secs(60 * 60)).contains(&remote.ttl) {
        return Err(Error::invalid(format!("a ttl of {:?}, not 1 s to 1 hour", remote.ttl)).within(place));
    }
    let priority = match remote.priority {
        "" | "urgent" => Priority::Urgent,
        "background" => Priority::Background,
        other => return Err(Error::invalid(format!("a priority of {other:?}, not urgent or background")).within(place)),
    };
    if remote.background > remote.limit {
        let shown = format!("a background of {}, not 1 to its limit {}", remote.background, remote.limit);
        return Err(Error::invalid(shown).within(place));
    }
    let breaker = match (remote.failures, remote.pause) {
        (0, None) => None,
        (failures, Some(pause)) => Some(Rule { failures, pause }),
        (_, None) => return Err(Error::invalid("a breaker's failures without its pause").within(place)),
    };
    check_breaker(breaker).map_err(|error| error.within(&place))?;
    let ask = Ask {
        place,
        limit: remote.limit,
        priority,
        background: (remote.background > 0).then_some(remote.background),
        ttl: Some(remote.ttl),
        max_hold: remote.max_hold,
        wait: remote.wait,
        request: Some(remote.request),
        breaker,
        probe: false,
    };
    Ok((opened.locks, ask))
}

/// How the key a client through a connection names stands now.
#[cfg(feature = "wire")]
pub(crate) fn key_state(store: &Store, kind: &str, name: &str, key: &str) -> Result<KeyState> {
    let kind = match kind {
        "lock" => Kind::Lock,
        "semaphore" => Kind::Semaphore,
        other => return Err(Error::invalid(format!("a kind of {other:?}, not lock or semaphore"))),
    };
    let opened = Opened::open(store, kind, name, 1, None)?;
    let place = opened.place(key)?;
    Ok(opened.locks.state(&place))
}

/// A key held; dropping it, a panic's unwinding included, lets the key go to
/// the next in line. A blocking hold marks its thread for the whole hold; an
/// awaited one, whose future may move between threads, marks the thread
/// that polls it around each poll instead, and keeps no mark here.
struct Holding {
    locks: Weak<Locks>,
    place: Place,
    granted: Granted,
    /// From the hold's ask to its grant, as its caller saw it.
    waited: Duration,
    finished: Finished,
    _mark: Option<Mark>,
}

/// How a hold's work ended, which its key's breaker counts when the key is
/// let go: a success, a failure, or what it is when the work never answered,
/// a panic's unwinding or an awaited hold's future dropped.
struct Finished(AtomicU8);

impl Finished {
    fn new(unanswered: Outcome) -> Finished {
        Finished(AtomicU8::new(unanswered as u8))
    }

    fn answered(&self, succeeded: bool) {
        let outcome = if succeeded { Outcome::Succeeded } else { Outcome::Failed };
        self.0.store(outcome as u8, Ordering::Release);
    }

    /// The outcome, a success whatever the work answered once the holder
    /// said so.
    fn outcome(&self, watch: &lease::Watch) -> Outcome {
        match self.0.load(Ordering::Acquire) {
            _ if watch.said_healthy() => Outcome::Succeeded,
            code if code == Outcome::Succeeded as u8 => Outcome::Succeeded,
            code if code == Outcome::Failed as u8 => Outcome::Failed,
            _ => Outcome::Abandoned,
        }
    }
}

impl Holding {
    fn ended(&self) -> bool {
        self.granted.watch.ending() != Ending::Live
    }

    /// Runs the holder's function: a lease that ended meanwhile fails the
    /// hold, as the function's own error would. The key goes when the holding
    /// is dropped.
    fn run<T, E: From<Error>>(&self, work: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let worked = work();
        self.finished.answered(worked.is_ok());
        self.kept(worked?)
    }

    /// What a hold answers once its function answered `value`: the value,
    /// unless the lease ended meanwhile.
    fn kept<T, E: From<Error>>(&self, value: T) -> Result<T, E> {
        match self.granted.watch.ending() {
            Ending::Live => Ok(value),
            Ending::Lost => {
                let lost = "the lease ended while its function ran, revoked at its max_hold or its timeout";
                Err(E::from(Error::new(ErrorKind::Lost, lost).within(&self.place)))
            }
            Ending::Closed => Err(E::from(Error::closed(self.place.to_string()))),
        }
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        if let Some(locks) = self.locks.upgrade() {
            locks.release(&self.granted.lease, self.finished.outcome(&self.granted.watch));
        }
    }
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lock({})", self.opened.name)
    }
}

impl fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Semaphore({}, {})", self.opened.name, self.opened.limit)
    }
}

impl fmt::Debug for LockBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LockBuilder({})", self.name)
    }
}

impl fmt::Debug for SemaphoreBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SemaphoreBuilder({})", self.name)
    }
}

impl fmt::Debug for LockKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LockKey({}: {:?})", self.opened.name, self.key)
    }
}

impl fmt::Debug for SemaphoreKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SemaphoreKey({}: {:?})", self.opened.name, self.key)
    }
}

impl fmt::Debug for Lease<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lease({}, {:?}, waited {:?})", self.holding.place, self.fence, self.holding.waited)
    }
}

impl fmt::Debug for Permit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Permit({}, waited {:?})", self.holding.place, self.holding.waited)
    }
}

thread_local! {
    /// The keys this thread waits for or holds, with the locks they are of.
    static HOLDING: RefCell<Vec<(usize, Place)>> = const { RefCell::new(Vec::new()) };
}

/// Refuses a hold that would wait for what its own thread holds: a writer in
/// a transaction, which the hold would keep while it waits, or the same key.
fn refuse_where_a_hold_would_wait_for_itself(locks: &Arc<Locks>, place: &Place) -> Result<()> {
    if sqlite::in_a_transaction() {
        let why = "a hold inside a transaction would keep its writer while the key is waited for: hold first";
        return Err(Error::invalid(why).within(place));
    }
    let address = Arc::as_ptr(locks) as usize;
    if HOLDING.with(|holding| holding.borrow().iter().any(|(at, held)| *at == address && held == place)) {
        return Err(Error::invalid("a hold of a key inside its own hold would wait for itself").within(place));
    }
    Ok(())
}

/// A key this thread waits for or holds, from its ask to its letting go.
struct Mark(usize, Place);

impl Mark {
    fn enter(locks: &Arc<Locks>, place: &Place) -> Mark {
        Mark::at(Arc::as_ptr(locks) as usize, place)
    }

    /// The mark of `place` of the locks at `address`.
    fn at(address: usize, place: &Place) -> Mark {
        HOLDING.with(|holding| holding.borrow_mut().push((address, place.clone())));
        Mark(address, place.clone())
    }
}

impl Drop for Mark {
    fn drop(&mut self) {
        HOLDING.with(|holding| {
            let mut holding = holding.borrow_mut();
            if let Some(at) = holding.iter().rposition(|(address, place)| *address == self.0 && place == &self.1) {
                holding.remove(at);
            }
        });
    }
}

#[cfg(test)]
#[path = "locks_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "priority_tests.rs"]
mod priority_tests;

#[cfg(test)]
#[path = "breaker_tests.rs"]
mod breaker_tests;
