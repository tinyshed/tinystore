use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use super::named;
use crate::aio::oneshot::pending;
use crate::aio::{Store, run};
use crate::cancel::{self, Captured};
use crate::kv::{Handed, Key, Value, WriteOptions};
use crate::pool::Pool;
use crate::{Error, Result};

/// Answers being opened, as [`crate::kv::OnceBuilder`] are: their name and
/// type, and how long one is kept.
#[must_use = "once's answers open with open()"]
pub struct OnceBuilder<V> {
    builder: crate::kv::OnceBuilder<V>,
    pool: Arc<Pool>,
}

impl Store {
    /// Functions run once a key, their answers kept in the store's kv.db, as
    /// [`crate::Store::once`] keeps them, the functions here async.
    pub fn once<V: Value>(&self, name: &str) -> OnceBuilder<V> {
        OnceBuilder { builder: self.blocking().once(name), pool: Arc::clone(self.pool()) }
    }
}

impl<V: Value> OnceBuilder<V> {
    /// How long an answer is kept from when it was made; a day unless said.
    pub fn keep(self, keep: Duration) -> Self {
        OnceBuilder { builder: self.builder.keep(keep), ..self }
    }

    /// Opens the answers: a plain call, made once at start.
    pub fn open(self) -> Result<Once<V>> {
        Ok(Once { once: self.builder.open()?, pool: self.pool })
    }
}

/// Functions run at most once a key, their answers kept, the functions
/// async: the same answers as [`crate::kv::Once`], whose runs and those of
/// the blocking API wait for one another.
pub struct Once<V> {
    once: crate::kv::Once<V>,
    pool: Arc<Pool>,
}

impl<V: Value> Once<V> {
    /// The answer kept under `key`, or the answer of the future `work`
    /// gives, which is then kept, as [`crate::kv::Once::run`] keeps it:
    ///
    /// ```no_run
    /// # async fn charge(order: &str) -> tinystore::Result<String> { Ok(order.to_owned()) }
    /// # async fn example(store: tinystore::aio::Store) -> tinystore::Result<()> {
    /// let charges = store.once::<String>("charges").open()?;
    /// let order = "order-7".to_owned();
    /// let receipt = charges.run("key-7f", || charge(&order)).await?; // charged once, however often it is sent
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// While another run of the key is under way, in this process or through
    /// its server, this one waits for it with no thread held, and returns its
    /// answer. On a node that copies kv.db, the run claims its key at the
    /// file's home, which waits for the run under way there, and this run
    /// waits for the home's answer on a thread of its own.
    ///
    /// `work` is called once the key is claimed, and its future is polled
    /// inside this one's. The future is `Send`, and `'static` when `key`,
    /// `work` and its future are, as `tokio::spawn` asks.
    ///
    /// Dropped while it waits, the run leaves and nothing changes; while its
    /// function runs, the key is let go and nothing is kept, so the next run
    /// runs again; once the function answered, its answer is kept all the
    /// same and the key let go after it. A run of a key inside its own run,
    /// awaited or blocking, is `Invalid`.
    pub fn run<K, E, W, F>(&self, key: K, work: W) -> impl Future<Output = Result<V, E>> + Send + use<K, V, E, W, F>
    where
        K: Key,
        E: From<Error> + Send,
        W: FnOnce() -> F + Send,
        F: Future<Output = Result<V, E>> + Send,
    {
        let run = Run {
            once: self.once.clone(),
            pool: Arc::clone(&self.pool),
            tokens: cancel::captured(),
            key: key.text().into_owned(),
        };
        async move {
            #[cfg(feature = "replication")]
            if run.once.answers().is_copy() {
                return run.at_home(work).await;
            }
            let handed = match run.claim().await? {
                Start::Kept(answer) => return Ok(answer),
                Start::Yours(handed) => *handed,
            };
            let mut function = pin!(work());
            let answer = poll_fn(|context| handed.polling(|| function.as_mut().poll(context))).await?;
            run.keep(handed, &answer).await;
            Ok(answer)
        }
    }

    /// The answer kept under `key`.
    pub fn get(&self, key: impl Key) -> impl Future<Output = Result<Option<V>>> + Send + 'static {
        let (once, key) = (self.once.clone(), key.text().into_owned());
        let what = named(once.answers().scope.shown_key(&key), "a get");
        run::point(&self.pool, what, move || once.get(&*key))
    }

    /// Forgets the answer kept under `key`, so that the next run runs again;
    /// says whether there was one.
    pub fn delete(&self, key: impl Key) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (answers, key) = (self.once.answers().clone(), key.text().into_owned());
        let what = named(answers.scope.shown_key(&key), "a delete");
        run::write(&self.pool, what, move |answer| {
            answers.remove_then(&key, WriteOptions::default(), move |found| answer.send(found));
        })
    }

    /// The same answers seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Once<V> {
        Once { once: self.once.under(owner), pool: Arc::clone(&self.pool) }
    }

    /// The same answers, for their blocking calls.
    pub fn blocking(&self) -> &crate::kv::Once<V> {
        &self.once
    }
}

/// One run of a key: what its steps share.
struct Run<V> {
    once: crate::kv::Once<V>,
    pool: Arc<Pool>,
    /// The tokens around the call that made the run, which reach its reads.
    tokens: Captured,
    key: String,
}

/// How a run's claim ended.
enum Start<V> {
    /// An answer kept, or the one the run waited for could not keep.
    Kept(V),
    Yours(Box<Handed>),
}

impl<V: Value> Run<V> {
    /// The answer kept, or the key claimed for this run: a run under way is
    /// waited for by a waiter the engine calls as it ends, which wakes this
    /// task, and its answer read again.
    async fn claim(&self) -> Result<Start<V>> {
        loop {
            if let Some(answer) = self.kept().await? {
                return Ok(Start::Kept(answer));
            }
            let (answer, ended) = pending(named(self.shown_key(), "a run under way"));
            let Some(handed) = self.once.claim_then(&self.key, Box::new(move |unkept| answer.send(Ok(unkept))))? else {
                // a waiter the store dropped as it closed answers Closed
                match ended.await? {
                    Some(unkept) => return Ok(Start::Kept(self.once.unkept(&unkept)?)),
                    None => continue,
                }
            };
            // a run that kept its answer between the read and the claim let the key go first
            return match self.kept().await? {
                Some(answer) => Ok(Start::Kept(answer)),
                None => Ok(Start::Yours(Box::new(handed))),
            };
        }
    }

    /// Keeps `answer`, and lets the key go once it is kept. A keep queued
    /// commits whether or not the run's future is still awaited; one that
    /// failed is logged, and the run still returns its answer, as a
    /// blocking run does.
    fn keep(&self, handed: Handed, answer: &V) -> impl Future<Output = ()> + Send + 'static {
        let kept = match self.once.answers().encode(&self.key, answer) {
            Ok(raw) => {
                let what = named(self.shown_key(), "a keep");
                Some(run::write(&self.pool, what, move |done| handed.keep_then(raw, move || done.send(Ok(())))))
            }
            Err(error) => {
                handed.not_kept(&error);
                None
            }
        };
        async move {
            if let Some(kept) = kept {
                let _ = kept.await;
            }
        }
    }

    /// The answer kept under the key, read as a point read.
    async fn kept(&self) -> Result<Option<V>> {
        let (once, key) = (self.once.clone(), self.key.clone());
        let what = named(self.shown_key(), "a kept answer");
        self.tokens.run(None, || run::point(&self.pool, what, move || once.get(&*key))).await
    }

    fn shown_key(&self) -> String {
        self.once.answers().scope.shown_key(&self.key)
    }
}

/// A run on a node that copies kv.db: its home holds the key, so that one run
/// of it goes on across every node, and keeps what this one answered. Each
/// call to the home blocks on its answer, so it runs on a thread of its own:
/// a claim waits there for the run under way, up to 25 s, and sixteen such
/// claims would hold every thread of the store's pool.
#[cfg(feature = "replication")]
impl<V: Value> Run<V> {
    async fn at_home<E, W, F>(self, work: W) -> Result<V, E>
    where
        E: From<Error>,
        W: FnOnce() -> F,
        F: Future<Output = Result<V, E>>,
    {
        if let Some(answer) = self.kept().await? {
            return Ok(answer);
        }
        let held = match self.claim_at_home().await? {
            AtHome::Kept(answer) => return Ok(answer),
            AtHome::Held(held) => held,
        };
        let answer = work().await;
        let raw = match &answer {
            Ok(value) => Some(self.once.answers().encode(&self.key, value)?),
            Err(_) => None,
        };
        held.keep(named(self.shown_key(), "a keep at home"), raw).await;
        answer
    }

    /// Claims the key at the home. A run the home handed over once this
    /// future was gone goes back to it, nothing kept, as the claim it came in
    /// drops.
    fn claim_at_home(&self) -> impl Future<Output = Result<AtHome<V>>> + Send + 'static {
        let what = named(self.shown_key(), "a run claimed at home");
        let (once, key) = (self.once.clone(), self.key.clone());
        run::passed_on(what, move || {
            let held = |token| HeldAtHome { once: Some(once.clone()), key: key.clone(), token };
            once.claim_at_home(&key).map(|claimed| match claimed {
                crate::kv::AtHome::Kept(answer) => AtHome::Kept(answer),
                crate::kv::AtHome::Yours(token) => AtHome::Held(held(token)),
            })
        })
    }
}

/// What a run on a copy found at its home: the answer kept there, or the run
/// handed to this node.
#[cfg(feature = "replication")]
enum AtHome<V: Value> {
    Kept(V),
    Held(HeldAtHome<V>),
}

/// A run its home handed to this node: given back with nothing kept, from a
/// thread of its own, should it be dropped before it keeps an answer.
#[cfg(feature = "replication")]
struct HeldAtHome<V: Value> {
    /// None once kept.
    once: Option<crate::kv::Once<V>>,
    key: String,
    token: u64,
}

#[cfg(feature = "replication")]
impl<V: Value> HeldAtHome<V> {
    /// Keeps `raw` at the home, none when the run's function failed; once
    /// polled, the keep runs to its end.
    fn keep(mut self, what: String, raw: Option<crate::kv::Raw>) -> impl Future<Output = ()> + Send + 'static {
        let (once, key, token) = (self.once.take(), self.key.clone(), self.token);
        let kept = run::passed_on(what, move || {
            if let Some(once) = once {
                once.keep_at_home(&key, token, raw);
            }
            Ok(())
        });
        async move {
            let _ = kept.await;
        }
    }
}

#[cfg(feature = "replication")]
impl<V: Value> Drop for HeldAtHome<V> {
    fn drop(&mut self) {
        if let Some(once) = self.once.take() {
            let (key, token) = (std::mem::take(&mut self.key), self.token);
            if let Err(error) = run::own_thread(move || once.keep_at_home(&key, token, None)) {
                tracing::warn!(
                    target: "tinystore",
                    %error,
                    "a once run abandoned on a copy could not give its key back to the home, which holds it meanwhile",
                );
            }
        }
    }
}

impl<V: Value> From<crate::kv::Once<V>> for Once<V> {
    /// The same answers, their calls awaited.
    fn from(once: crate::kv::Once<V>) -> Once<V> {
        let pool = once.answers().scope.pool();
        Once { once, pool }
    }
}

impl<V> Clone for Once<V> {
    fn clone(&self) -> Self {
        Once { once: self.once.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl<V> fmt::Debug for Once<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.once, f)
    }
}

impl<V> fmt::Debug for OnceBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

#[cfg(test)]
#[path = "once_tests.rs"]
mod tests;
