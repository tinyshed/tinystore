//! Stopping the reads a call makes, from outside the thread that makes them.
//!
//! [`CancellationToken::run`] puts its token in the scope of the thread that
//! runs the call, and every read the call makes on that thread looks at it:
//!
//! | where the read is        | what stops it                                                    |
//! |--------------------------|------------------------------------------------------------------|
//! | before it takes a reader | the read looks at the tokens first                               |
//! | waiting for a reader     | `cancel` wakes the thread, which leaves the queue                |
//! | in a statement           | `cancel` interrupts the reader; a timeout, the progress handler  |
//! | decoding what it read    | metrics looks between series, records before each page           |
//!
//! A reader's progress handler reads the tokens of the thread that steps its
//! statement, which is the thread whose call it is, every [`STEPS`] of
//! SQLite's steps.
//!
//! SQLite's interrupt stops whatever its connection runs at that moment, so a
//! read gives its reader to the tokens only from its first statement to its
//! last, and a token interrupts under the lock the read takes back its reader
//! with: an interrupt never reaches the next read the reader serves. One that
//! lands after the read's last statement is lost harmlessly, since SQLite
//! clears it when a statement starts while none of the connection runs, as
//! the reader's next one does. For the same reason one that lands just before
//! a statement starts is lost to that statement, which the progress handler
//! then stops at its first look.

use std::cell::RefCell;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// How many of SQLite's steps a reader runs between two looks at its call's
/// tokens: a look reads a thread-local, and a thousand steps take
/// microseconds.
pub(crate) const STEPS: std::ffi::c_int = 1000;

/// Stops the reads of a call from another thread, at once or after a
/// timeout: a read under way stops, gives its reader back and fails
/// `Cancelled`, and one that has not started fails before it takes one.
///
/// The token stops what [`run`](CancellationToken::run) runs, on its thread:
///
/// ```no_run
/// # use std::time::Duration;
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// # let db = store.database("app").open()?;
/// use tinystore::CancellationToken;
///
/// let token = CancellationToken::new();
/// let counting = std::thread::spawn({
///     let (db, token) = (db.clone(), token.clone());
///     move || token.run(|| db.scalar::<i64>("select count(*) from events"))
/// });
/// token.cancel(); // the visitor left: the count stops with `Cancelled`, its reader back
///
/// let timed = CancellationToken::timeout(Duration::from_secs(2));
/// let total: i64 = timed.run(|| db.scalar("select count(*) from events"))?;
/// # Ok(())
/// # }
/// ```
///
/// Only reads stop: a write runs to its end, and so does a read inside a
/// transaction, which its own bound stops. Clones share the token.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<Inner>,
}

struct Inner {
    cancelled: AtomicBool,
    /// When the token stops by itself, and the timeout that said so.
    timeout: Option<(Instant, Duration)>,
    /// What stops the reads under the token now: a reader to interrupt, a
    /// thread waiting for one to wake.
    stops: Mutex<Stops>,
}

#[derive(Default)]
struct Stops {
    next: u64,
    each: Vec<(u64, Stop)>,
}

/// What one read under way does when its token is cancelled. It runs under
/// the token's lock, so it must not take the token's lock nor wait.
type Stop = Arc<dyn Fn() + Send + Sync>;

thread_local! {
    /// The tokens of the call this thread runs, the innermost last.
    static SCOPE: RefCell<Vec<Arc<Inner>>> = const { RefCell::new(Vec::new()) };
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::stopping_at(None)
    }

    /// A token that stops its reads once `after` has passed since it was
    /// made, or earlier when it is cancelled.
    ///
    /// A timeout is read as a statement runs and as a read waits; a statement
    /// SQLite runs as one step, as a `count(*)` of a whole table, finishes that
    /// step first, where `cancel` stops it at once.
    pub fn timeout(after: Duration) -> Self {
        Self::stopping_at(Instant::now().checked_add(after).map(|at| (at, after)))
    }

    fn stopping_at(timeout: Option<(Instant, Duration)>) -> Self {
        let inner = Inner { cancelled: AtomicBool::new(false), timeout, stops: Mutex::default() };
        Self { inner: Arc::new(inner) }
    }

    /// Stops the reads under the token now and from now on.
    pub fn cancel(&self) {
        if self.inner.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let stops = lock(&self.inner.stops);
        for (_, stop) in &stops.each {
            stop();
        }
    }

    /// Whether the token was cancelled or its timeout passed.
    pub fn is_cancelled(&self) -> bool {
        self.inner.stopped()
    }

    /// Runs `call` on this thread with the token over every read it makes:
    /// once the token is cancelled, each read under way fails `Cancelled` and
    /// each one after it fails before it starts. Tokens nest: a read stops
    /// when any token around it does.
    pub fn run<T>(&self, call: impl FnOnce() -> T) -> T {
        let _scope = Scope::enter(Arc::clone(&self.inner));
        call()
    }

    /// Whether `other` is a clone of this token.
    #[cfg(feature = "wire")]
    pub(crate) fn same(&self, other: &CancellationToken) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("CancellationToken");
        debug.field("cancelled", &self.inner.cancelled.load(Ordering::SeqCst));
        if let Some((_, after)) = self.inner.timeout {
            debug.field("timeout", &after);
        }
        debug.finish()
    }
}

impl Inner {
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) || self.timeout.is_some_and(|(at, _)| Instant::now() >= at)
    }

    /// Why `what` stops, when the token has stopped.
    fn refusal(&self, what: impl FnOnce() -> String) -> Option<Error> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Some(Error::new(ErrorKind::Cancelled, what()));
        }
        let (at, after) = self.timeout?;
        let past = Instant::now() >= at;
        past.then(|| Error::new(ErrorKind::Cancelled, format!("{} past its timeout of {after:?}", what())))
    }

    fn add(&self, stop: Stop) -> u64 {
        let mut stops = lock(&self.stops);
        let id = stops.next;
        stops.next += 1;
        stops.each.push((id, stop));
        id
    }

    fn remove(&self, id: u64) {
        lock(&self.stops).each.retain(|(kept, _)| *kept != id);
    }
}

/// This thread's call under a token, until it is dropped.
struct Scope;

impl Scope {
    fn enter(inner: Arc<Inner>) -> Scope {
        SCOPE.with(|scope| scope.borrow_mut().push(inner));
        Scope
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        SCOPE.with(|scope| scope.borrow_mut().pop());
    }
}

/// What a read under way gave the tokens of its call, taken back when it is
/// dropped: from then on a cancel no longer reaches it.
pub(crate) struct Given {
    stops: Vec<(Arc<Inner>, u64)>,
}

impl Drop for Given {
    fn drop(&mut self) {
        for (inner, id) in &self.stops {
            inner.remove(*id);
        }
    }
}

/// Whether this thread's call runs under a token.
pub(crate) fn in_scope() -> bool {
    SCOPE.with(|scope| scope.try_borrow().is_ok_and(|scope| !scope.is_empty()))
}

/// Whether a token of this thread's call has stopped: what a reader's
/// progress handler asks as SQLite steps, and true interrupts the statement.
pub(crate) fn stopped() -> bool {
    SCOPE.with(|scope| scope.try_borrow().is_ok_and(|scope| scope.iter().any(|inner| inner.stopped())))
}

/// Fails `what` as `Cancelled` when a token of this thread's call has
/// stopped; `what` is built only then.
pub(crate) fn check(what: impl FnOnce() -> String) -> Result<()> {
    let refused = SCOPE.with(|scope| {
        let scope = scope.try_borrow().ok()?;
        let stopped = scope.iter().find(|inner| inner.stopped())?;
        stopped.refusal(what)
    });
    refused.map_or(Ok(()), Err)
}

/// The soonest a token of this thread's call times out, which a read that
/// waits wakes at.
pub(crate) fn deadline() -> Option<Instant> {
    SCOPE.with(|scope| {
        let scope = scope.try_borrow().ok()?;
        scope.iter().filter_map(|inner| inner.timeout.map(|(at, _)| at)).min()
    })
}

/// Has every token of this thread's call run `stop` when it is cancelled,
/// until what this returns is dropped; none when the call has no token.
pub(crate) fn on_cancel(stop: impl Fn() + Send + Sync + 'static) -> Option<Given> {
    let scope: Vec<Arc<Inner>> = SCOPE.with(|scope| scope.try_borrow().map(|scope| scope.clone()).unwrap_or_default());
    if scope.is_empty() {
        return None;
    }
    let stop: Stop = Arc::new(stop);
    let stops = scope.into_iter().map(|inner| (Arc::clone(&inner), inner.add(Arc::clone(&stop)))).collect();
    Some(Given { stops })
}

/// A failure of `what` the call's own token caused, said as the token's:
/// SQLite's interrupt reads `interrupted`, where the caller wants to know it
/// was its cancel or its timeout.
pub(crate) fn named(error: Error, what: impl FnOnce() -> String) -> Error {
    if error.kind() != ErrorKind::Cancelled {
        return error;
    }
    check(what).err().unwrap_or(error)
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod tests;
