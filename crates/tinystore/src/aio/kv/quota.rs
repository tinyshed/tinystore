use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use super::named;
use crate::Result;
use crate::aio::{Store, run};
use crate::kv::{Allowance, Key};
use crate::pool::Pool;

/// A quota being opened, as [`crate::kv::QuotaBuilder`] is: its name and its
/// windows.
#[must_use = "a quota opens with open()"]
pub struct QuotaBuilder {
    builder: crate::kv::QuotaBuilder,
    pool: Arc<Pool>,
}

impl Store {
    /// Uses a key may make in windows that each reset on their own, in the
    /// store's kv.db, whose calls are awaited.
    pub fn quota(&self, name: &str) -> QuotaBuilder {
        QuotaBuilder { builder: self.blocking().quota(name), pool: Arc::clone(self.pool()) }
    }
}

impl QuotaBuilder {
    /// Lets a key use up to `limit` every `span` from its first use.
    pub fn window(self, name: &str, limit: u64, span: Duration) -> Self {
        QuotaBuilder { builder: self.builder.window(name, limit, span), ..self }
    }

    /// Opens the quota: a plain call, made once at start.
    pub fn open(self) -> Result<Quota> {
        Ok(Quota { quota: self.builder.open()?, pool: self.pool })
    }
}

/// Uses a key may make in each of its windows, their calls awaited: the same
/// quota as [`crate::kv::Quota`]. An `allow` counts in every window or in
/// none, in one durable write.
pub struct Quota {
    quota: crate::kv::Quota,
    pool: Arc<Pool>,
}

impl Quota {
    /// Uses one of each of `key`'s windows, or none when one has no room left.
    pub fn allow(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        self.allow_n(key, 1)
    }

    /// Uses `n` of each of `key`'s windows at once, or none when one has no
    /// room for `n`. More than a window's limit never passes, and is `Invalid`.
    pub fn allow_n(&self, key: impl Key, n: u64) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        let (quota, key) = (self.quota.clone(), key.text().into_owned());
        let what = named(quota.scope.shown_key(&key), "an allow");
        run::write(&self.pool, what, move |answer| quota.allow_n_then(&key, n, move |used| answer.send(used)))
    }

    /// `key`'s windows, using nothing: `ok` says whether one more use would
    /// pass now.
    pub fn peek(&self, key: impl Key) -> impl Future<Output = Result<Allowance>> + Send + 'static {
        let (quota, key) = (self.quota.clone(), key.text().into_owned());
        let what = named(quota.scope.shown_key(&key), "a peek");
        run::point(&self.pool, what, move || quota.peek(&*key))
    }

    /// Gives one use back to each of `key`'s windows that has not reset since.
    pub fn refund(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static {
        self.refund_n(key, 1)
    }

    /// Gives `n` uses back, never below nothing.
    pub fn refund_n(&self, key: impl Key, n: u64) -> impl Future<Output = Result<()>> + Send + 'static {
        let (quota, key) = (self.quota.clone(), key.text().into_owned());
        let what = named(quota.scope.shown_key(&key), "a refund");
        run::write(&self.pool, what, move |answer| quota.refund_n_then(&key, n, move |refunded| answer.send(refunded)))
    }

    /// Forgets `key`'s windows, so that its next use starts each anew.
    pub fn reset(&self, key: impl Key) -> impl Future<Output = Result<()>> + Send + 'static {
        let (quota, key) = (self.quota.clone(), key.text().into_owned());
        let what = named(quota.scope.shown_key(&key), "a reset");
        run::write(&self.pool, what, move |answer| quota.reset_then(&key, move |reset| answer.send(reset)))
    }

    /// The same quota seen from the branch `owner` names under this one,
    /// whose keys are counted apart.
    pub fn under(&self, owner: impl Key) -> Quota {
        Quota { quota: self.quota.under(owner), pool: Arc::clone(&self.pool) }
    }

    /// The same quota, for its blocking calls.
    pub fn blocking(&self) -> &crate::kv::Quota {
        &self.quota
    }
}

impl From<crate::kv::Quota> for Quota {
    /// The same quota, its calls awaited.
    fn from(quota: crate::kv::Quota) -> Quota {
        let pool = quota.scope.pool();
        Quota { quota, pool }
    }
}

impl Clone for Quota {
    fn clone(&self) -> Self {
        Quota { quota: self.quota.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl fmt::Debug for Quota {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.quota, f)
    }
}

impl fmt::Debug for QuotaBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
