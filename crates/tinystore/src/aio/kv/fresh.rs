use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::time::Duration;

use super::named;
use super::once::{Run, keep};
use crate::aio::run;
use crate::kv::{Answer, Key, Value, WriteOptions};
use crate::pool::Pool;
use crate::{Error, Result};

/// A build in the background of a stale answer, as the program's runtime
/// runs it: `tokio::spawn(build)`.
pub type Build = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What a program gives its builds in the background to: the library runs no
/// async runtime of its own, and a build's future may need the program's.
type Spawn = Arc<dyn Fn(Build) + Send + Sync>;

/// Answers being opened with `fresh`, as [`crate::kv::FreshBuilder`] are,
/// and where their builds in the background run.
#[must_use = "once's answers open with open()"]
pub struct FreshBuilder<V> {
    builder: crate::kv::FreshBuilder<V>,
    pool: Arc<Pool>,
    spawn: Option<Spawn>,
}

impl<V: Value> FreshBuilder<V> {
    pub(super) fn new(builder: crate::kv::FreshBuilder<V>, pool: Arc<Pool>) -> Self {
        FreshBuilder { builder, pool, spawn: None }
    }

    /// How long an answer is kept from when it was made; a day unless said.
    pub fn keep(self, keep: Duration) -> Self {
        FreshBuilder { builder: self.builder.keep(keep), ..self }
    }

    /// Where a build in the background runs: `.spawn(|build| { tokio::spawn(build); })`.
    /// A build dropped before its end keeps nothing and lets its key go.
    pub fn spawn(self, spawn: impl Fn(Build) + Send + Sync + 'static) -> Self {
        FreshBuilder { spawn: Some(Arc::new(spawn)), ..self }
    }

    /// Opens the answers: a plain call, made once at start. Without `spawn`
    /// it is `Invalid`, since a stale answer would have nowhere to be made.
    pub fn open(self) -> Result<Fresh<V>> {
        let once = self.builder.open()?;
        let Some(spawn) = self.spawn else {
            let shown = once.once().answers().scope.shown();
            return Err(Error::invalid("awaited answers opened with fresh and no spawn").within(shown));
        };
        Ok(Fresh { once, pool: self.pool, spawn })
    }
}

/// Once's answers opened with `fresh`, awaited: the same answers and terms
/// as [`crate::kv::Fresh`], whose builds in the background run where `spawn`
/// puts them.
///
/// ```no_run
/// # use std::time::Duration;
/// # async fn book(user: u64) -> tinystore::Result<String> { Ok(user.to_string()) }
/// # async fn example(store: tinystore::aio::Store) -> tinystore::Result<()> {
/// let books = store
///     .once::<String>("cabinet-books")
///     .fresh(Duration::from_mins(10))
///     .keep(Duration::from_hours(12))
///     .spawn(|build| drop(tokio::spawn(build)))
///     .open()?;
/// let answer = books.answer(7, || book(7)).await?; // at once, stale or not, once one is kept
/// # Ok(())
/// # }
/// ```
pub struct Fresh<V> {
    once: crate::kv::Fresh<V>,
    pool: Arc<Pool>,
    spawn: Spawn,
}

impl<V: Value> Fresh<V> {
    /// The answer kept under `key`, with how old it is, or the answer of the
    /// future `work` gives once none is kept, as [`crate::kv::Fresh::answer`]
    /// gives it. A kept answer older than `fresh` is answered at once,
    /// `stale`, and `work`'s future, given to `spawn`, makes the new one,
    /// unless a build of the key is under way or failed within its pause.
    pub fn answer<K, E, W, F>(
        &self,
        key: K,
        work: W,
    ) -> impl Future<Output = Result<Answer<V>, E>> + Send + use<K, V, E, W, F>
    where
        K: Key,
        E: From<Error> + Send + 'static,
        W: FnOnce() -> F + Send + 'static,
        F: Future<Output = Result<V, E>> + Send + 'static,
    {
        let run = Run::new(self.once.once(), &self.pool, &key.text());
        let spawn = Arc::clone(&self.spawn);
        async move {
            match run.kept().await? {
                Some(answer) if answer.stale => {
                    run.once().stale_answered();
                    refresh(&run, &spawn, work)?;
                    Ok(answer)
                }
                found => run.build(found, work).await,
            }
        }
    }

    /// [`Fresh::answer`]'s value alone.
    pub fn run<K, E, W, F>(&self, key: K, work: W) -> impl Future<Output = Result<V, E>> + Send + use<K, V, E, W, F>
    where
        K: Key,
        E: From<Error> + Send + 'static,
        W: FnOnce() -> F + Send + 'static,
        F: Future<Output = Result<V, E>> + Send + 'static,
    {
        let answer = self.answer(key, work);
        async move { answer.await.map(|answer| answer.value) }
    }

    /// The answer kept under `key`, stale or not.
    pub fn get(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
        let (once, key) = (self.once.clone(), key.text().into_owned());
        let what = named(self.once.once().answers().scope.shown_key(&key), "a get");
        run::point(&self.pool, what, move || once.get(&*key))
    }

    /// Forgets the answer kept under `key`; says whether there was one.
    pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (answers, key) = (self.once.once().answers().clone(), key.text().into_owned());
        let what = named(answers.scope.shown_key(&key), "a delete");
        run::write(&self.pool, what, move |answer| {
            answers.remove_then(&key, WriteOptions::default(), move |found| answer.send(found));
        })
    }

    /// Removes every answer of this branch and of the branches under it, in
    /// one commit, as [`crate::kv::Fresh::clear`] does.
    pub fn clear(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let answers = self.once.once().answers().clone();
        let what = named(answers.scope.shown_branch(), "a clear");
        run::write(&self.pool, what, move |answer| answers.clear_then(move |cleared| answer.send(cleared)))
    }

    /// The same answers seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Fresh<V> {
        Fresh { once: self.once.under(owner), pool: Arc::clone(&self.pool), spawn: Arc::clone(&self.spawn) }
    }

    /// The same answers, for their blocking calls.
    pub fn blocking(&self) -> &crate::kv::Fresh<V> {
        &self.once
    }
}

/// Gives `spawn` the build of the run's key in the background, unless a run
/// holds the key or its pause does. The build keeps its answer, or, failed,
/// keeps the stale one and holds off the next; dropped before its end, it
/// keeps nothing and lets the key go.
fn refresh<V, E, W, F>(run: &Run<V>, spawn: &Spawn, work: W) -> Result<()>
where
    V: Value,
    E: Send + 'static,
    W: FnOnce() -> F + Send + 'static,
    F: Future<Output = Result<V, E>> + Send + 'static,
{
    let Some(handed) = run.once().claim_refresh(run.key())? else { return Ok(()) };
    let (once, pool, key) = (run.once().clone(), Arc::clone(run.pool()), run.key().to_owned());
    spawn(Box::pin(async move {
        let mut function = pin!(work());
        match poll_fn(|context| handed.polling(|| function.as_mut().poll(context))).await {
            Ok(answer) => keep(&once, &pool, (handed, &key), &answer).await,
            Err(_) => handed.failed(),
        }
    }));
    Ok(())
}

impl<V> Clone for Fresh<V> {
    fn clone(&self) -> Self {
        Fresh { once: self.once.clone(), pool: Arc::clone(&self.pool), spawn: Arc::clone(&self.spawn) }
    }
}

impl<V> fmt::Debug for Fresh<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.once, f)
    }
}

impl<V> fmt::Debug for FreshBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

#[cfg(test)]
#[path = "fresh_tests.rs"]
mod tests;
