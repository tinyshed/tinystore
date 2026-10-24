use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::named;
use crate::Result;
use crate::aio::{Store, run};
use crate::cancel;
use crate::kv::{Allowance, Key};
use crate::pool::Pool;

/// A rate limit being opened, as [`crate::kv::RateLimitBuilder`] is: its
/// name, its rate and its burst.
#[must_use = "a rate limit opens with open()"]
pub struct RateLimitBuilder {
    builder: crate::kv::RateLimitBuilder,
    pool: Arc<Pool>,
}

impl Store {
    /// Requests let through at a steady rate a key, with room for a burst, in
    /// the store's kv.db, whose calls are awaited.
    pub fn rate_limit(&self, name: &str) -> RateLimitBuilder {
        RateLimitBuilder { builder: self.blocking().rate_limit(name), pool: Arc::clone(self.pool()) }
    }
}

impl RateLimitBuilder {
    /// Lets `count` requests a key through every `per`.
    pub fn rate(self, count: u64, per: Duration) -> Self {
        RateLimitBuilder { builder: self.builder.rate(count, per), ..self }
    }

    /// How many requests may pass at once before the rate holds the next; the
    /// rate's count unless said.
    pub fn burst(self, burst: u64) -> Self {
        RateLimitBuilder { builder: self.builder.burst(burst), ..self }
    }

    /// Opens the rate limit: a plain call, made once at start.
    pub fn open(self) -> Result<RateLimit> {
        Ok(RateLimit { limit: self.builder.open()?, pool: self.pool })
    }
}

/// Requests let through at a steady rate a key, with room for a burst, their
/// calls awaited: the same rate limit as [`crate::kv::RateLimit`].
///
/// Its times live in memory, so each call is a step there, answered in its
/// first poll. A key memory does not hold yet is read from kv.db first, on
/// the store's pool when the read would wait in the poll, and an `allow` or
/// a `reset` polled once applies whether or not its future is still awaited.
///
/// On a node that copies kv.db, `allow` and `reset` pass on to the file's
/// home and wait for its answer on a thread of its own, and `peek` reads the
/// node's copy.
pub struct RateLimit {
    limit: crate::kv::RateLimit,
    pool: Arc<Pool>,
}

impl RateLimit {
    /// Asks for one request of `key`.
    pub fn allow(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        self.allow_n(key, 1)
    }

    /// Asks for `n` requests of `key` at once: all of them pass or none does.
    /// More than the burst never passes, and is `Invalid`.
    pub fn allow_n(&self, key: impl Key, n: u64) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        let key = key.text().into_owned();
        let what = named(self.limit.scope.shown_key(&key), "an allow");
        self.step(what, move |limit| limit.allow_n(&*key, n))
    }

    /// Whether one request of `key` would pass now, using nothing.
    pub fn peek(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        let (limit, key) = (self.limit.clone(), key.text().into_owned());
        let what = named(limit.scope.shown_key(&key), "a peek");
        run::point(&self.pool, what, move || limit.peek(&*key))
    }

    /// Forgets `key`'s requests, so that its next one has the whole burst.
    pub fn reset(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static {
        let key = key.text().into_owned();
        let what = named(self.limit.scope.shown_key(&key), "a reset");
        self.step(what, move |limit| limit.reset(&*key))
    }

    /// The same rate limit seen from the branch `owner` names under this one,
    /// whose keys are counted apart.
    pub fn under(&self, owner: impl Key) -> RateLimit {
        RateLimit { limit: self.limit.under(owner), pool: Arc::clone(&self.pool) }
    }

    /// The same rate limit, for its blocking calls.
    pub fn blocking(&self) -> &crate::kv::RateLimit {
        &self.limit
    }

    /// A call that changes what memory holds: a step there, or on a copy a
    /// call passed on to the file's home.
    fn step<T: Send + 'static>(
        &self,
        what: String,
        step: impl Fn(&crate::kv::RateLimit) -> Result<T> + Send + 'static,
    ) -> impl Future<Output = Result<T>> + Send + 'static {
        let (limit, pool, tokens) = (self.limit.clone(), Arc::clone(&self.pool), cancel::captured());
        async move {
            if limit.on_a_copy() {
                return run::passed_on(what, move || step(&limit)).await;
            }
            tokens.run(None, || run::step(&pool, what, move || step(&limit))).await
        }
    }
}

impl From<crate::kv::RateLimit> for RateLimit {
    /// The same rate limit, its calls awaited.
    fn from(limit: crate::kv::RateLimit) -> RateLimit {
        let pool = limit.scope.pool();
        RateLimit { limit, pool }
    }
}

impl Clone for RateLimit {
    fn clone(&self) -> Self {
        RateLimit { limit: self.limit.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl fmt::Debug for RateLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.limit, f)
    }
}

impl fmt::Debug for RateLimitBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod tests;
