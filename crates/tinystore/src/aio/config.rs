//! The async API's handles of the Config engine, beside the blocking ones of
//! [`crate::config`]: a config's changes are awaited, and its watch is a
//! [`Stream`] of its settings.
//!
//! ```no_run
//! # use serde::{Deserialize, Serialize};
//! # use tinystore::config::{File, Live};
//! # #[derive(Clone, Default, Serialize, Deserialize)]
//! # struct Settings { rps: Live<u32> }
//! # mod futures {
//! #     pub trait StreamExt: futures_core::Stream + Unpin + Sized {
//! #         async fn next(&mut self) -> Option<Self::Item> {
//! #             std::future::poll_fn(|cx| std::pin::Pin::new(&mut *self).poll_next(cx)).await
//! #         }
//! #     }
//! #     impl<S: futures_core::Stream + Unpin> StreamExt for S {}
//! # }
//! # async fn example() -> tinystore::Result<()> {
//! use futures::StreamExt; // next() on a Stream
//! use tinystore::Options;
//! use tinystore::aio::Store;
//!
//! let store = Store::open("data", Options::default())?;
//! let config = store.config("chat", Settings::default()).file(File::optional("chat.toml")).open()?;
//!
//! let rps = *config.get().rps; // memory: not awaited
//! config.update(|s| *s.rps = 200).await?; // kept in kv.db, and every handle sees it
//! config.reset("rps").await?;
//!
//! let mut watching = config.watch(); // the settings now, then after each change
//! while let Some(settings) = watching.next().await {
//!     println!("{} requests a second", *settings?.rps); // Err(Closed) once the store closed
//! }
//! # Ok(())
//! # }
//! ```
//!
//! | A call                                    | Runs                                                                   | Its future dropped once polled                  |
//! |-------------------------------------------|------------------------------------------------------------------------|-------------------------------------------------|
//! | `get`, `explain`                          | memory, plain calls                                                    | —                                               |
//! | `update`, `if_version(…).update`, `reset` | the closure and `validate` in the poll, the commit on the store's pool | the try submitted commits, every handle sees it |
//! | `watch`, a `Stream`                       | woken by each change, on no thread                                     | the watch stops                                 |
//!
//! Opening a config is a plain call, made once at start: it reads the
//! config's files and the changes kv.db keeps. A watch with a callback is
//! the blocking handle's, [`Config::blocking`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::oneshot::Answer;
use super::run;
use crate::config::{Env, Explained, File, Snapshot};
use crate::pool::Pool;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// A config being opened, as [`crate::config::ConfigBuilder`]: its
/// declaration, then its layers in the order written.
#[must_use = "a config opens with open()"]
pub struct ConfigBuilder<T> {
    builder: crate::config::ConfigBuilder<T>,
    pool: Arc<Pool>,
}

impl super::Store {
    /// The program's settings as one typed value, declared as
    /// [`crate::Store::config`] declares them, its changes awaited.
    pub fn config<T>(&self, name: &str, declaration: T) -> ConfigBuilder<T>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    {
        ConfigBuilder { builder: self.blocking().config(name, declaration), pool: Arc::clone(self.pool()) }
    }
}

impl<T> ConfigBuilder<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// A file's settings over what came before, as
    /// [`crate::config::ConfigBuilder::file`].
    pub fn file(self, file: impl Into<File>) -> Self {
        ConfigBuilder { builder: self.builder.file(file), pool: self.pool }
    }

    /// The environment's variables over what came before, as
    /// [`crate::config::ConfigBuilder::env`].
    pub fn env(self, env: impl Into<Env>) -> Self {
        ConfigBuilder { builder: self.builder.env(env), pool: self.pool }
    }

    /// The program's own check of the whole value, at open and before a
    /// change is kept. It runs on the thread that polls a change's future, or
    /// on the store's pool for a change whose future was dropped.
    pub fn validate(self, validate: impl Fn(&T) -> std::result::Result<(), String> + Send + Sync + 'static) -> Self {
        ConfigBuilder { builder: self.builder.validate(validate), pool: self.pool }
    }

    /// Refuses to open while a kept change no longer fits.
    pub fn strict(self) -> Self {
        ConfigBuilder { builder: self.builder.strict(), pool: self.pool }
    }

    /// Opens the config as the blocking builder does: a plain call, made once
    /// at start, which reads its files and the changes kv.db keeps.
    pub fn open(self) -> Result<Config<T>> {
        Ok(Config { config: self.builder.open()?, pool: Some(self.pool) })
    }
}

/// A program's settings as one typed value, as [`crate::config::Config`]:
/// read from memory, its changes awaited.
pub struct Config<T> {
    config: crate::config::Config<T>,
    /// None for a handle made after its store went.
    pool: Option<Arc<Pool>>,
}

impl<T> Config<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// The settings as they are now: a read of memory, not awaited.
    pub fn get(&self) -> Snapshot<T> {
        self.config.get()
    }

    /// Changes live settings as [`crate::config::Config::update`] does: kept
    /// in kv.db over every layer until they are reset, and seen by every
    /// handle of the config before the future answers. Another change made
    /// meanwhile stays: `change` runs again on the settings it made, eight
    /// tries at most.
    ///
    /// `change` and the config's `validate` run in a poll, on the thread that
    /// polls the future, and the commit on the store's pool. The poll after
    /// the commit hands the change to every handle, which runs each handle's
    /// `validate` and the blocking watchers on that thread too, or on the
    /// store's pool when the future was dropped before that poll. Once
    /// polled, the try submitted commits and reaches every handle whether or
    /// not the future is still awaited; a try after a conflict is made only
    /// while it is polled.
    pub fn update(
        &self,
        change: impl FnMut(&mut T) + Send + 'static,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        changed(self.config.clone(), self.pool.clone(), None, change)
    }

    /// A change made only while the config's changes are still at `version`,
    /// as a snapshot shows it; another change made since is `conflict`.
    pub fn if_version(&self, version: i64) -> IfVersion<'_, T> {
        IfVersion { config: self, version }
    }

    /// Drops the change kept of the setting at `path`, or of every setting of
    /// the group at it, so that the layers give them again. The path is
    /// checked in the first poll, and the commit runs on the store's pool, as
    /// `update`'s does.
    pub fn reset(&self, path: &str) -> impl Future<Output = Result<()>> + Send + 'static {
        let (config, pool, path) = (self.config.clone(), self.pool.clone(), path.to_owned());
        async move {
            if !config.resets(&path)? {
                return Ok(());
            }
            reset(config, pool, path).await
        }
    }

    /// Drops every change kept of the config, its commit on the store's pool.
    pub fn reset_all(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        reset(self.config.clone(), self.pool.clone(), String::new())
    }

    /// The settings now, then the newest after each change, as a [`Stream`]
    /// that ends once the store closed, its last item `Err(Closed)`. The
    /// watch starts here, not at the first poll, and one that falls behind
    /// gets the newest, never every version between. It outlives the handle
    /// it was made from; dropped, it stops.
    pub fn watch(&self) -> Watching<T> {
        let latest = Arc::new(Mutex::new(Latest { newest: None, closed: false, ended: false, waker: None }));
        let told = Arc::clone(&latest);
        let closing: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let waker = {
                let mut latest = lock(&told);
                latest.closed = true;
                latest.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        if !self.config.on_close(Arc::downgrade(&closing)) {
            lock(&latest).closed = true;
        }
        let seen = Arc::clone(&latest);
        let watching = self.config.watch(move |next, _| {
            let waker = {
                let mut latest = lock(&seen);
                latest.newest = Some(next.clone());
                latest.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        let name = self.config.name().to_owned();
        Watching { latest, name, _watching: watching, _config: self.config.clone(), _closing: closing }
    }

    /// Every setting with its value, the layer that gave it and the values it
    /// went over: a read of memory.
    pub fn explain(&self) -> Explained {
        self.config.explain()
    }

    pub fn name(&self) -> &str {
        self.config.name()
    }

    /// The same config, for its blocking calls, a watch with a callback among
    /// them.
    pub fn blocking(&self) -> &crate::config::Config<T> {
        &self.config
    }
}

impl<T> From<crate::config::Config<T>> for Config<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// The same config, its changes awaited: a clone of its handle.
    fn from(config: crate::config::Config<T>) -> Config<T> {
        let pool = config.store().ok().map(|store| Pool::of_or_stopped(&store));
        Config { config, pool }
    }
}

/// A change made only while the config is still at a version:
/// `config.if_version(shown.version()).update(…)`.
pub struct IfVersion<'a, T> {
    config: &'a Config<T>,
    version: i64,
}

impl<T> IfVersion<'_, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// As [`Config::update`], one try: `conflict` once the config moved
    /// since the version, and `change` is not run again.
    pub fn update(
        self,
        change: impl FnMut(&mut T) + Send + 'static,
    ) -> impl Future<Output = Result<()>> + Send + 'static {
        changed(self.config.config.clone(), self.config.pool.clone(), Some(self.version), change)
    }
}

/// Tries a change until it is kept, or until `if_version` finds the config
/// moved: each try made in a poll, its commit awaited, and what it wrote
/// published on the polling thread before the next.
async fn changed<T>(
    config: crate::config::Config<T>,
    pool: Option<Arc<Pool>>,
    if_version: Option<i64>,
    mut change: impl FnMut(&mut T) + Send,
) -> Result<()>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let pool = alive(config.name(), pool)?;
    for _ in 0..crate::config::Config::<T>::ATTEMPTS {
        let Some(proposal) = config.proposal(if_version, &mut change)? else { return Ok(()) };
        let (submitting, elsewhere) = (config.clone(), run::lead_on(&pool));
        let what = format!("config {}: a change", config.name());
        let submit = move |answer: Answer<_>| {
            submitting.submit_change(proposal, elsewhere, move |written| answer.send(written));
        };
        match run::write(&pool, what, submit).await?.publish() {
            Err(error) if error.kind() == ErrorKind::Conflict && if_version.is_none() => {}
            done => return done,
        }
    }
    Err(config.raced())
}

/// Drops the changes kept at `path`, every change for an empty one, and
/// publishes what was dropped on the polling thread.
async fn reset<T>(config: crate::config::Config<T>, pool: Option<Arc<Pool>>, path: String) -> Result<()>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let pool = alive(config.name(), pool)?;
    let (submitting, elsewhere) = (config.clone(), run::lead_on(&pool));
    let what = format!("config {}: a reset", config.name());
    let submit = move |answer: Answer<_>| {
        submitting.submit_reset(&path, elsewhere, move |written| answer.send(written));
    };
    run::write(&pool, what, submit).await?.publish()
}

/// The pool of a config's store, which a handle made after its store went
/// does not have.
fn alive(name: &str, pool: Option<Arc<Pool>>) -> Result<Arc<Pool>> {
    pool.ok_or_else(|| Error::closed(format!("config {name}: its store closed")))
}

/// A watch of a config's settings: the settings when it was made, then the
/// newest after each change, until the store closes, read as a [`Stream`].
/// It stops when dropped.
#[must_use = "a watch is read as a Stream, and stops when dropped"]
pub struct Watching<T> {
    latest: Arc<Mutex<Latest<T>>>,
    name: String,
    /// The blocking watch that hands each change to `latest`.
    _watching: crate::config::Watching,
    /// Kept, so that the watch outlives the program's handle: the blocking
    /// watch holds the handle weakly, and its hub, which hands out each
    /// change and the store's close, goes with the last handle. It keeps no
    /// store open, since kv holds its store weakly.
    _config: crate::config::Config<T>,
    /// What the config's hub holds weakly to say that the store closed.
    _closing: Arc<dyn Fn() + Send + Sync>,
}

/// What a watch has not handed out yet, and the task that waits for it.
struct Latest<T> {
    newest: Option<Snapshot<T>>,
    closed: bool,
    /// `Closed` was handed out: what follows is the stream's end.
    ended: bool,
    waker: Option<Waker>,
}

impl<T> Stream for Watching<T> {
    type Item = Result<Snapshot<T>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut latest = lock(&self.latest);
        if let Some(newest) = latest.newest.take() {
            return Poll::Ready(Some(Ok(newest)));
        }
        if latest.closed {
            if std::mem::replace(&mut latest.ended, true) {
                return Poll::Ready(None);
            }
            return Poll::Ready(Some(Err(Error::closed(format!("config {}: the store closed", self.name)))));
        }
        if !latest.waker.as_ref().is_some_and(|waker| waker.will_wake(context.waker())) {
            latest.waker = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

impl<T> Clone for Config<T> {
    fn clone(&self) -> Self {
        Config { config: self.config.clone(), pool: self.pool.clone() }
    }
}

impl<T> fmt::Debug for ConfigBuilder<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl<T> fmt::Debug for Config<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.config, f)
    }
}

impl<T> fmt::Debug for IfVersion<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IfVersion").field("config", &self.config.config).field("version", &self.version).finish()
    }
}

impl<T> fmt::Debug for Watching<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watching").field("config", &self.name).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
