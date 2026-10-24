//! What a call does while an awaited future polls it on the program's
//! executor: it leads no commit there, and it waits for nothing there.
//!
//! The async API runs an engine's code in one of two scopes on the thread
//! that polls a future. The SQLite adapter asks [`lead`] where a thread would
//! lead a commit, and every place a call would park its thread asks
//! [`may_wait`] first:
//!
//! | inside              | runs                     | a write submitted          | a wait                                 |
//! |---------------------|--------------------------|----------------------------|----------------------------------------|
//! | [`leading_on`]      | a write's submit         | its commit led on the pool | fails `Internal`: the engine's mistake |
//! | [`without_waiting`] | a point read, in a poll  | refused                    | refused: the call runs again elsewhere |
//!
//! A wait is a read when no reader is idle, or any read in a submit; memory
//! that does not fit now, or any patient reservation in a submit; a blocking
//! write, transaction, vacuum or migration; a call passed on to the home of a
//! copy; a once's run under way; a lock's answer. A submit encodes its
//! arguments and calls `File::submit`, nothing more: what reads or waits
//! first runs on the pool, or reserves through the async API's waiter, before
//! the submit. Waits no check guards yet are listed where the engines' async
//! calls are built, `aio/run.rs`.
//!
//! The commit's sync of the disk, the checkpoint after it, and the writes
//! queued behind it run on the pool, while the executor's thread goes back to
//! its other tasks.

use std::sync::Arc;

use crate::{Error, ErrorKind, Result};

/// Work handed to another thread.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// Where a commit runs that a write submitted inside [`leading_on`] would
/// lead: a thread of the store's pool, or one of its own once the pool
/// stopped. A job dropped unrun, as only a system that starts no thread
/// makes one, leads where it is dropped, since the writes queued behind the
/// lead have no other leader.
pub(crate) type Lead = Arc<dyn Fn(Job) + Send + Sync>;

#[cfg(feature = "async")]
thread_local! {
    /// The lead of the scope this thread runs in, the innermost.
    static LEAD: std::cell::RefCell<Option<Lead>> = const { std::cell::RefCell::new(None) };
    /// What the scope does with a wait, and whether its call met one.
    static WAITS: std::cell::Cell<Waits> = const { std::cell::Cell::new(Waits::Allowed) };
}

#[cfg(feature = "async")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waits {
    /// Outside the async API's scopes: a call waits as anywhere.
    Allowed,
    /// A write's submit: a wait is the engine's mistake, and fails.
    Forbidden,
    /// A point read, and whether it met a wait it was refused.
    Refused { met: bool },
}

/// Runs `call`, a write's submit, with every commit a write it submits would
/// lead on this thread handed to `lead`, and every wait it would make failed
/// `Internal`: an engine's tests see a submit that reads or waits first.
#[cfg(feature = "async")]
pub(crate) fn leading_on<T>(lead: &Lead, call: impl FnOnce() -> T) -> T {
    let _scope = Scope::enter(Some(Arc::clone(lead)), Waits::Forbidden);
    call()
}

/// Runs `call`, a point read, and refuses what it would wait for here: a
/// reader when none is idle or reads wait in line for one, memory that does
/// not fit now, a write, submitted or blocking, and every other wait
/// [`may_wait`] guards. `None` says it met one: what it answered is not its
/// answer, and it runs again elsewhere.
///
/// So `call` may run twice, here up to a wait and then on the pool: it must
/// submit nothing and change nothing before its last possible wait, nor
/// after a wait it was refused.
#[cfg(feature = "async")]
pub(crate) fn without_waiting<T>(call: impl FnOnce() -> T) -> Option<T> {
    let scope = Scope::enter(None, Waits::Refused { met: false });
    let answer = call();
    let met = WAITS.get() == Waits::Refused { met: true };
    drop(scope);
    (!met).then_some(answer)
}

/// Where a commit this thread would lead goes instead, inside [`leading_on`].
#[cfg(feature = "async")]
pub(crate) fn lead() -> Option<Lead> {
    LEAD.with(|lead| lead.borrow().clone())
}

/// Where a commit this thread would lead goes instead: nowhere, in a library
/// built without the async API.
#[cfg(not(feature = "async"))]
pub(crate) fn lead() -> Option<Lead> {
    None
}

/// Whether this thread runs a point read, inside [`without_waiting`], which
/// takes what it can have now and is refused the rest.
#[cfg(feature = "async")]
pub(crate) fn waits_refused() -> bool {
    matches!(WAITS.get(), Waits::Refused { .. })
}

/// Whether this thread runs a point read: never, in a library built without
/// the async API.
#[cfg(not(feature = "async"))]
pub(crate) fn waits_refused() -> bool {
    false
}

/// Says whether this thread's call may park here for `what`: always, outside
/// the async API's scopes. In a point read it is refused, to run again on the
/// pool; in a write's submit it fails `Internal`, since the engine that made
/// the submit should have read or waited before it, off the executor.
#[cfg(feature = "async")]
pub(crate) fn may_wait(what: impl FnOnce() -> String) -> Result<()> {
    match WAITS.get() {
        Waits::Allowed => Ok(()),
        Waits::Refused { .. } => Err(refused(what)),
        Waits::Forbidden => Err(Error::internal(format!(
            "{}: an awaited write's submit would wait on the thread that polls it: \
             what reads or waits runs before the submit, on the store's pool",
            what()
        ))),
    }
}

/// Says whether this thread's call may park here: always, in a library built
/// without the async API.
#[cfg(not(feature = "async"))]
pub(crate) fn may_wait(_what: impl FnOnce() -> String) -> Result<()> {
    Ok(())
}

/// Refuses `what`, which would wait, inside [`without_waiting`]: the call
/// is marked to run again on the pool, and the error is what its code passes
/// up meanwhile, never its answer.
pub(crate) fn refused(what: impl FnOnce() -> String) -> Error {
    #[cfg(feature = "async")]
    WAITS.set(Waits::Refused { met: true });
    Error::new(ErrorKind::Unavailable, format!("{}: it would wait, and its call runs elsewhere", what()))
}

/// The scope of a lead, until it is dropped, a panic's unwinding included.
#[cfg(feature = "async")]
struct Scope {
    lead: Option<Lead>,
    waits: Waits,
}

#[cfg(feature = "async")]
impl Scope {
    fn enter(lead: Option<Lead>, waits: Waits) -> Scope {
        let outer = LEAD.with(|scope| std::mem::replace(&mut *scope.borrow_mut(), lead));
        Scope { lead: outer, waits: WAITS.replace(waits) }
    }
}

#[cfg(feature = "async")]
impl Drop for Scope {
    fn drop(&mut self) {
        let outer = self.lead.take();
        LEAD.with(|scope| *scope.borrow_mut() = outer);
        WAITS.set(self.waits);
    }
}
