//! Once's answers opened with `fresh`: an answer kept is answered at once
//! while a new one is made, and kept when the new one fails.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread;
use std::time::Duration;

use super::once::{Answer, BUILDS, FAILURES, Once, OnceBuilder, counted, not_kept, pause_after};
use super::path::Key;
use super::value::{Raw, Value};
use crate::clock::millis;
use crate::{Error, Result};

/// Answers being opened with `fresh`: their name and type, how long one is
/// answered without making a new one, and how long one is kept at all.
#[must_use = "once's answers open with open()"]
pub struct FreshBuilder<V> {
    once: OnceBuilder<V>,
    fresh: Duration,
}

impl<V: Value> FreshBuilder<V> {
    pub(super) fn new(once: OnceBuilder<V>, fresh: Duration) -> Self {
        FreshBuilder { once, fresh }
    }

    /// How long an answer is kept from when it was made; a day unless said.
    pub fn keep(self, keep: Duration) -> Self {
        FreshBuilder { once: self.once.keep(keep), ..self }
    }

    /// Opens the answers; a `fresh` of zero, or as long as `keep` or longer,
    /// is `Invalid`.
    pub fn open(self) -> Result<Fresh<V>> {
        Ok(Fresh { once: self.once.open_with(Some(self.fresh))? })
    }
}

/// Once's answers, each answered as it is while younger than `fresh`, then
/// answered at once while one new answer is made in the background, until
/// `keep` ends:
///
/// ```no_run
/// # use std::time::Duration;
/// # fn book(user: u64) -> tinystore::Result<String> { Ok(user.to_string()) }
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// let (fresh, keep) = (Duration::from_mins(10), Duration::from_hours(12));
/// let books = store.once::<String>("cabinet-books").fresh(fresh).keep(keep).open()?;
/// let user = 7;
/// let answer = books.answer(user, move || book(user))?;
/// println!("{} made {:?} ago, stale: {}", answer.value, answer.age, answer.stale);
/// # Ok(())
/// # }
/// ```
///
/// | The kept answer            | What the caller gets | What is built                                |
/// |----------------------------|----------------------|----------------------------------------------|
/// | younger than `fresh`       | it, at once          | nothing                                      |
/// | between `fresh` and `keep` | it at once, stale    | one new answer in the background             |
/// | past `keep`, or none       | the new answer       | one, which every caller of the key waits for |
///
/// A build in the background runs on the store's threads, so its function
/// is `Send + 'static`; its answer replaces the kept one and starts both
/// terms again, and its error keeps the kept one until `keep` ends, while no
/// other build of its key starts for a tenth of `fresh`, a second at least
/// and a minute at most. One build of a key runs at a time across every
/// client of the store; on a node that copies kv.db, one a node.
pub struct Fresh<V> {
    once: Once<V>,
}

impl<V: Value> Fresh<V> {
    /// The answer kept under `key`, with how old it is, or `work`'s once
    /// none is kept, which is then kept, as [`Once::run`] keeps it. A kept
    /// answer older than `fresh` is answered at once, `stale`, and `work`
    /// makes the new one in the background, unless a build of the key is
    /// under way or failed within its pause.
    pub fn answer<E, W>(&self, key: impl Key, work: W) -> Result<Answer<V>, E>
    where
        E: From<Error> + Send + 'static,
        W: FnOnce() -> Result<V, E> + Send + 'static,
    {
        let key = key.text();
        let Some(cell) = self.once.answers.read_cell(&key)? else {
            return self.once.build(&key, None, work);
        };
        let answer = self.once.answered(&key, &cell)?;
        if answer.stale {
            self.once.stale_answered();
            self.refresh(&key, work);
        }
        Ok(answer)
    }

    /// [`Fresh::answer`]'s value alone.
    pub fn run<E, W>(&self, key: impl Key, work: W) -> Result<V, E>
    where
        E: From<Error> + Send + 'static,
        W: FnOnce() -> Result<V, E> + Send + 'static,
    {
        self.answer(key, work).map(|answer| answer.value)
    }

    /// The answer kept under `key`, stale or not.
    pub fn get(&self, key: impl Key) -> Result<Option<V>> {
        self.once.get(key)
    }

    /// Forgets the answer kept under `key`, so that the next run runs again;
    /// says whether there was one.
    pub fn delete(&self, key: impl Key) -> Result<bool> {
        self.once.delete(key)
    }

    /// Removes every answer of this branch and of the branches under it, in
    /// one commit. A build under way meanwhile keeps its answer once it ends.
    pub fn clear(&self) -> Result<()> {
        self.once.clear()
    }

    /// The same answers seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Fresh<V> {
        Fresh { once: self.once.under(owner) }
    }

    /// The answers, for the async API.
    #[cfg(feature = "async")]
    pub(crate) fn once(&self) -> &Once<V> {
        &self.once
    }

    /// Starts `work` on the store's threads as the build of `key` in the
    /// background, unless a run holds the key or its pause does.
    fn refresh<E, W>(&self, key: &str, work: W)
    where
        E: Send + 'static,
        W: FnOnce() -> Result<V, E> + Send + 'static,
    {
        let scope = &self.once.answers.scope;
        let Ok(path) = scope.path(key) else { return };
        let running = (scope.id, path);
        if !scope.kv.runs().claim_refresh(&running, scope.now()) {
            return;
        }
        let refresh = Refresh { once: self.once.clone(), key: key.to_owned(), running, unkept: None };
        // a store that closed lets the key go as the build drops unrun
        if let Ok(pool) = scope.kv.store().and_then(|store| crate::pool::Pool::of(&store)) {
            pool.run(Box::new(move || refresh.run(work)));
        }
    }
}

/// A build in the background of a key whose kept answer went stale; dropping
/// it, run or not, lets the key go.
struct Refresh<V> {
    once: Once<V>,
    key: String,
    running: (i64, Vec<u8>),
    /// The answer when it could not be kept, for the runs that waited.
    unkept: Option<Raw>,
}

impl<V: Value> Refresh<V> {
    fn run<E>(mut self, work: impl FnOnce() -> Result<V, E>) {
        let scope = &self.once.answers.scope;
        scope.kv.runs().running_on(&self.running, thread::current().id());
        counted(scope, BUILDS);
        match catch_unwind(AssertUnwindSafe(work)) {
            Ok(Ok(answer)) => self.keep(&answer),
            _ => self.failed(),
        }
    }

    /// Replaces the stale answer, whose expiry a write would keep, and starts
    /// its `keep` again.
    fn keep(&mut self, answer: &V) {
        let answers = &self.once.answers;
        let keep = answers.kept_for().unwrap_or_default();
        if let Err(error) = answers.key(&*self.key).ttl(keep).set(answer).map(drop) {
            not_kept(&error);
            self.unkept = self.once.answers.encode(&self.key, answer).ok();
        }
    }

    /// Leaves the kept answer as it is, and holds off the next build.
    fn failed(&self) {
        let scope = &self.once.answers.scope;
        counted(scope, FAILURES);
        let (pause, now) = (pause_after(self.once.fresh.unwrap_or_default()), scope.now());
        scope.kv.runs().pause(&self.running, now.saturating_add(millis(pause)), now);
    }
}

impl<V> Drop for Refresh<V> {
    fn drop(&mut self) {
        self.once.answers.scope.kv.runs().release(&self.running, self.unkept.take());
    }
}

impl<V> Clone for Fresh<V> {
    fn clone(&self) -> Self {
        Fresh { once: self.once.clone() }
    }
}

impl<V> fmt::Debug for Fresh<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fresh({:?})", self.once)
    }
}

impl<V> fmt::Debug for FreshBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fresh{:?}", self.once)
    }
}

#[cfg(test)]
#[path = "fresh_tests.rs"]
mod tests;
