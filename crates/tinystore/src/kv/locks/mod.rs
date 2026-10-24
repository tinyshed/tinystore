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
//! # Ok(())
//! # }
//! ```

mod engine;
mod lease;
mod state;

use std::cell::RefCell;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Weak};
use std::time::Duration;

pub(crate) use self::engine::Locks;
pub use self::lease::Fence;
pub(crate) use self::lease::{Answer, Granted};
use self::lease::{Ask, Ending, Kind, Place};
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
        SemaphoreBuilder { store: self.clone(), name: name.to_owned(), limit: None, max_hold: None }
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
    /// How many may hold a key at once, from 1 to 1000.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
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
        Ok(Semaphore { opened: Opened::open(&self.store, Kind::Semaphore, &self.name, limit, self.max_hold)? })
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
        LockKey { opened: &self.opened, key: key.text().into_owned(), wait: WAIT }
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

    pub fn key(&self, key: impl Key) -> SemaphoreKey<'_> {
        SemaphoreKey { opened: &self.opened, key: key.text().into_owned(), wait: WAIT }
    }
}

/// A key of a lock, for a hold that says how long it waits.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct LockKey<'a> {
    opened: &'a Opened,
    key: String,
    wait: Duration,
}

/// A key of a semaphore, for a hold that says how long it waits.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct SemaphoreKey<'a> {
    opened: &'a Opened,
    key: String,
    wait: Duration,
}

impl LockKey<'_> {
    /// How long the hold waits in line; ten seconds unless said.
    pub fn wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Lease) -> Result<T, E>) -> Result<T, E> {
        let holding = self.opened.hold(&self.key, self.wait)?;
        let lease = Lease { fence: holding.granted.fence, holding: &holding };
        holding.run(|| work(&lease))
    }

    pub fn try_hold<T, E: From<Error>>(self, work: impl FnOnce(&Lease) -> Result<T, E>) -> Result<Option<T>, E> {
        let Some(holding) = self.opened.try_hold(&self.key)? else { return Ok(None) };
        let lease = Lease { fence: holding.granted.fence, holding: &holding };
        holding.run(|| work(&lease)).map(Some)
    }
}

impl SemaphoreKey<'_> {
    pub fn wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    pub fn hold<T, E: From<Error>>(self, work: impl FnOnce(&Permit) -> Result<T, E>) -> Result<T, E> {
        let holding = self.opened.hold(&self.key, self.wait)?;
        holding.run(|| work(&Permit { holding: &holding }))
    }

    pub fn try_hold<T, E: From<Error>>(self, work: impl FnOnce(&Permit) -> Result<T, E>) -> Result<Option<T>, E> {
        let Some(holding) = self.opened.try_hold(&self.key)? else { return Ok(None) };
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
}

/// A holding of a semaphore's key: one of its limit.
pub struct Permit<'a> {
    holding: &'a Holding,
}

impl Permit<'_> {
    pub fn lost(&self) -> bool {
        self.holding.ended()
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
        Ok(Opened { locks, kind, name: Arc::from(name), limit, max_hold })
    }

    fn place(&self, key: &str) -> Result<Place> {
        let place = Place::new(self.kind, Arc::clone(&self.name), key.to_owned());
        if key.is_empty() || key.len() > MOST_KEY {
            return Err(Error::invalid(format!("a key is 1 to {MOST_KEY} bytes")).within(place));
        }
        Ok(place)
    }

    /// Holds `key` once it is free, `wait` at most.
    fn hold(&self, key: &str, wait: Duration) -> Result<Holding> {
        let place = self.place(key)?;
        match self.ask(&place, wait)? {
            Some(holding) => Ok(holding),
            None => Err(Error::new(ErrorKind::InUse, format!("held past a wait of {wait:?}")).within(place)),
        }
    }

    fn try_hold(&self, key: &str) -> Result<Option<Holding>> {
        let place = self.place(key)?;
        self.ask(&place, Duration::ZERO)
    }

    /// Asks for `place` and waits for the answer; none when it stayed busy.
    fn ask(&self, place: &Place, wait: Duration) -> Result<Option<Holding>> {
        crate::polled::may_wait(|| format!("{place}: a hold"))?;
        refuse_where_a_hold_would_wait_for_itself(&self.locks, place)?;
        let mark = Mark::enter(&self.locks, place);
        let ask =
            Ask { place: place.clone(), limit: self.limit, ttl: None, max_hold: self.max_hold, wait, request: None };
        let (answered, answer) = mpsc::sync_channel(1);
        self.locks.ask(ask, Box::new(move |answer| drop(answered.send(answer))));
        let answer = answer.recv().map_err(|_| Error::internal("a lock's answer was dropped").within(place))?;
        Ok(match answer? {
            Answer::Granted(granted) => {
                Some(Holding { locks: Arc::downgrade(&self.locks), place: place.clone(), granted, _mark: mark })
            }
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
    let ask = Ask {
        place,
        limit: remote.limit,
        ttl: Some(remote.ttl),
        max_hold: remote.max_hold,
        wait: remote.wait,
        request: Some(remote.request),
    };
    Ok((opened.locks, ask))
}

/// A key this thread holds; dropping it, a panic's unwinding included, lets
/// the key go to the next in line.
struct Holding {
    locks: Weak<Locks>,
    place: Place,
    granted: Granted,
    _mark: Mark,
}

impl Holding {
    fn ended(&self) -> bool {
        self.granted.watch.ending() != Ending::Live
    }

    /// Runs the holder's function: a lease that ended meanwhile fails the
    /// hold, as the function's own error would. The key goes when the holding
    /// is dropped.
    fn run<T, E: From<Error>>(&self, work: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let value = work()?;
        match self.granted.watch.ending() {
            Ending::Live => Ok(value),
            Ending::Lost => {
                let lost = "the lease ended while its function ran, revoked at its max_hold";
                Err(E::from(Error::new(ErrorKind::Lost, lost).within(&self.place)))
            }
            Ending::Closed => Err(E::from(Error::closed(self.place.to_string()))),
        }
    }
}

impl Drop for Holding {
    fn drop(&mut self) {
        if let Some(locks) = self.locks.upgrade() {
            locks.release(&self.granted.lease);
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
        write!(f, "Lease({}, {:?})", self.holding.place, self.fence)
    }
}

impl fmt::Debug for Permit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Permit({})", self.holding.place)
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
        let address = Arc::as_ptr(locks) as usize;
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
