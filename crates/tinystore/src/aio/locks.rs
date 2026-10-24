//! The async API's handles of the Locks engine, beside the blocking ones of
//! [`crate::locks`]: a hold is awaited, and its function is a future.
//!
//! ```no_run
//! # async fn charge(invoice: &str, fence: tinystore::locks::Fence) -> tinystore::Result<String> { Ok(String::new()) }
//! # async fn example() -> tinystore::Result<()> {
//! use std::time::Duration;
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let invoices = store.lock("invoices").max_hold(Duration::from_secs(120)).open()?;
//!
//! let receipt = invoices.hold("inv-42", |lease| async move { charge("inv-42", lease.fence()).await }).await?;
//! let tried = invoices.try_hold("inv-42", |lease| async move { charge("inv-42", lease.fence()).await }).await?;
//! let waited = invoices
//!     .key("inv-42")
//!     .wait(Duration::from_secs(30))
//!     .hold(|lease| async move { charge("inv-42", lease.fence()).await })
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! | its future dropped    | what happens                                                      |
//! |-----------------------|-------------------------------------------------------------------|
//! | before its first poll | nothing is asked                                                  |
//! | waiting in line       | it leaves the line; a grant that raced the drop is let go at once |
//! | holding               | `work` stops at its await, and the key goes to the next in line   |
//!
//! The ask is made in the first poll. A free key is given there; a held one
//! puts the hold in the key's line, first come first served, where it parks
//! no thread: a release, the locks' timer at the end of its `wait` or the
//! store's close wakes it. `work` runs inside the hold's future, on whatever
//! thread polls it, so a `tokio::time::timeout` around a hold stops its wait
//! or its work. A hold of a key inside its own hold, in a poll of `work`, is
//! refused `invalid`, and so is a hold inside a transaction's closure.

use std::fmt;
use std::future::Future;
use std::pin::pin;
use std::time::Duration;

use crate::kv::locks::{Ends, Held};
use crate::locks::Fence;
use crate::{Error, Key, Result};

/// A lock being opened, as [`crate::locks::LockBuilder`].
#[must_use = "a lock opens with open()"]
pub struct LockBuilder {
    builder: crate::locks::LockBuilder,
}

/// A semaphore being opened, as [`crate::locks::SemaphoreBuilder`].
#[must_use = "a semaphore opens with open()"]
pub struct SemaphoreBuilder {
    builder: crate::locks::SemaphoreBuilder,
}

impl super::Store {
    /// Keys held one holder at a time, across every process of the store, as
    /// [`crate::Store::lock`]: a hold is awaited.
    pub fn lock(&self, name: &str) -> LockBuilder {
        LockBuilder { builder: self.blocking().lock(name) }
    }

    /// Keys held by up to `limit` holders at a time, as
    /// [`crate::Store::semaphore`].
    pub fn semaphore(&self, name: &str) -> SemaphoreBuilder {
        SemaphoreBuilder { builder: self.blocking().semaphore(name) }
    }
}

impl LockBuilder {
    /// How long one lease may last from its grant: past it the key is given
    /// to the next in line and the hold answers `lost`. Off unless given.
    pub fn max_hold(self, bound: Duration) -> Self {
        LockBuilder { builder: self.builder.max_hold(bound) }
    }

    /// Opens the lock: a plain call, made once at start. The first lock or
    /// semaphore of a store writes the locks' epoch to kv.db.
    pub fn open(self) -> Result<Lock> {
        Ok(Lock { lock: self.builder.open()? })
    }
}

impl SemaphoreBuilder {
    /// How many may hold a key at once, from 1 to 1000.
    pub fn limit(self, limit: u32) -> Self {
        SemaphoreBuilder { builder: self.builder.limit(limit) }
    }

    pub fn max_hold(self, bound: Duration) -> Self {
        SemaphoreBuilder { builder: self.builder.max_hold(bound) }
    }

    /// Opens the semaphore: a plain call, made once at start.
    pub fn open(self) -> Result<Semaphore> {
        Ok(Semaphore { semaphore: self.builder.open()? })
    }
}

/// A lock's keys, each held by one holder at a time, as
/// [`crate::locks::Lock`].
#[derive(Clone)]
pub struct Lock {
    lock: crate::locks::Lock,
}

/// A semaphore's keys, each held by up to its limit at a time, as
/// [`crate::locks::Semaphore`].
#[derive(Clone)]
pub struct Semaphore {
    semaphore: crate::locks::Semaphore,
}

impl Lock {
    /// Waits for `key`, ten seconds at most, holds it while the future `work`
    /// makes runs and lets it go, answering what that future answered. A key
    /// still held after the wait is `in use`; a lease lost meanwhile is
    /// `lost`, even when `work` succeeded.
    ///
    /// The future is as `'static` as `work` and what it borrows, so a hold
    /// whose `work` owns what it uses is spawned.
    pub fn hold<'w, T, E, F>(
        &self,
        key: impl Key,
        work: impl FnOnce(Lease) -> F + Send + 'w,
    ) -> impl Future<Output = Result<T, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        self.key(key).hold(work)
    }

    /// Holds `key` while `work` runs if nobody holds it, and answers `None`
    /// without waiting if somebody does.
    pub fn try_hold<'w, T, E, F>(
        &self,
        key: impl Key,
        work: impl FnOnce(Lease) -> F + Send + 'w,
    ) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        self.key(key).try_hold(work)
    }

    /// A key, for a hold that says how long it waits.
    pub fn key(&self, key: impl Key) -> LockKey<'_> {
        LockKey { key: self.lock.key(key) }
    }

    /// The same lock, for its blocking calls.
    pub fn blocking(&self) -> &crate::locks::Lock {
        &self.lock
    }
}

impl From<crate::locks::Lock> for Lock {
    /// The same lock, its holds awaited: a clone of its handle.
    fn from(lock: crate::locks::Lock) -> Lock {
        Lock { lock }
    }
}

impl Semaphore {
    /// As [`Lock::hold`], one of the key's `limit` holders.
    pub fn hold<'w, T, E, F>(
        &self,
        key: impl Key,
        work: impl FnOnce(Permit) -> F + Send + 'w,
    ) -> impl Future<Output = Result<T, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        self.key(key).hold(work)
    }

    /// As [`Lock::try_hold`]: `None` at once while `limit` hold the key.
    pub fn try_hold<'w, T, E, F>(
        &self,
        key: impl Key,
        work: impl FnOnce(Permit) -> F + Send + 'w,
    ) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        self.key(key).try_hold(work)
    }

    pub fn key(&self, key: impl Key) -> SemaphoreKey<'_> {
        SemaphoreKey { key: self.semaphore.key(key) }
    }

    /// The same semaphore, for its blocking calls.
    pub fn blocking(&self) -> &crate::locks::Semaphore {
        &self.semaphore
    }
}

impl From<crate::locks::Semaphore> for Semaphore {
    /// The same semaphore, its holds awaited: a clone of its handle.
    fn from(semaphore: crate::locks::Semaphore) -> Semaphore {
        Semaphore { semaphore }
    }
}

/// A key of a lock, for a hold that says how long it waits.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct LockKey<'a> {
    key: crate::locks::LockKey<'a>,
}

/// A key of a semaphore, for a hold that says how long it waits.
#[must_use = "a key is held with hold() or try_hold()"]
pub struct SemaphoreKey<'a> {
    key: crate::locks::SemaphoreKey<'a>,
}

impl LockKey<'_> {
    /// How long the hold waits in line; ten seconds unless said. The wait
    /// ends on the locks' timer, with no thread parked.
    pub fn wait(self, wait: Duration) -> Self {
        LockKey { key: self.key.wait(wait) }
    }

    /// As [`Lock::hold`].
    pub fn hold<'w, T, E, F>(
        self,
        work: impl FnOnce(Lease) -> F + Send + 'w,
    ) -> impl Future<Output = Result<T, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        let awaited = self.key.awaited();
        async move {
            let held = awaited.hold().await?;
            let lease = Lease { fence: held.fence(), ends: held.ends() };
            holding(held, |held| held.marked(|| work(lease))).await
        }
    }

    /// As [`Lock::try_hold`].
    pub fn try_hold<'w, T, E, F>(
        self,
        work: impl FnOnce(Lease) -> F + Send + 'w,
    ) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        let awaited = self.key.awaited();
        async move {
            let Some(held) = awaited.try_hold().await? else { return Ok(None) };
            let lease = Lease { fence: held.fence(), ends: held.ends() };
            holding(held, |held| held.marked(|| work(lease))).await.map(Some)
        }
    }
}

impl SemaphoreKey<'_> {
    /// How long the hold waits in line; ten seconds unless said.
    pub fn wait(self, wait: Duration) -> Self {
        SemaphoreKey { key: self.key.wait(wait) }
    }

    /// As [`Semaphore::hold`].
    pub fn hold<'w, T, E, F>(
        self,
        work: impl FnOnce(Permit) -> F + Send + 'w,
    ) -> impl Future<Output = Result<T, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        let awaited = self.key.awaited();
        async move {
            let held = awaited.hold().await?;
            let permit = Permit { ends: held.ends() };
            holding(held, |held| held.marked(|| work(permit))).await
        }
    }

    /// As [`Semaphore::try_hold`].
    pub fn try_hold<'w, T, E, F>(
        self,
        work: impl FnOnce(Permit) -> F + Send + 'w,
    ) -> impl Future<Output = Result<Option<T>, E>> + Send + 'w
    where
        F: Future<Output = Result<T, E>> + Send + 'w,
        T: Send + 'w,
        E: From<Error> + Send + 'w,
    {
        let awaited = self.key.awaited();
        async move {
            let Some(held) = awaited.try_hold().await? else { return Ok(None) };
            let permit = Permit { ends: held.ends() };
            holding(held, |held| held.marked(|| work(permit))).await.map(Some)
        }
    }
}

/// Polls the future `start` makes while `held` holds its key, each poll with
/// the polling thread marked as holding it, then answers as a blocking hold
/// does: the future's error, or `lost` once the lease ended meanwhile. The
/// key goes when `held` is dropped, the future's drop included.
async fn holding<T, E, F>(held: Held, start: impl FnOnce(&Held) -> F) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    E: From<Error>,
{
    let mut work = pin!(start(&held));
    let value = std::future::poll_fn(|context| held.marked(|| work.as_mut().poll(context))).await?;
    held.kept(value)
}

/// A holding of a lock's key, given to the function that holds it, and owned
/// by it: the future it makes may keep it across its awaits.
pub struct Lease {
    fence: Fence,
    ends: Ends,
}

impl Lease {
    /// The lease's fence: hand it to whatever must refuse a late holder.
    pub fn fence(&self) -> Fence {
        self.fence
    }

    /// Whether the lease ended: revoked at its `max_hold`, the store closed,
    /// or, for a lease kept past its hold, the hold over. What is done with
    /// it from then on may run beside another holder.
    pub fn lost(&self) -> bool {
        self.ends.lost()
    }
}

/// A holding of a semaphore's key, one of its limit, owned by the function
/// that holds it.
pub struct Permit {
    ends: Ends,
}

impl Permit {
    /// As [`Lease::lost`]: true too once its hold is over.
    pub fn lost(&self) -> bool {
        self.ends.lost()
    }
}

impl fmt::Debug for LockBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl fmt::Debug for SemaphoreBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.lock, f)
    }
}

impl fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.semaphore, f)
    }
}

impl fmt::Debug for LockKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.key, f)
    }
}

impl fmt::Debug for SemaphoreKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.key, f)
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease").field("fence", &self.fence).field("lost", &self.lost()).finish()
    }
}

impl fmt::Debug for Permit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Permit").field("lost", &self.lost()).finish()
    }
}

#[cfg(test)]
#[path = "locks_tests.rs"]
mod tests;
