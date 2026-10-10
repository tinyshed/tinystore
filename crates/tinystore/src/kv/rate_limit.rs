use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::allowance::Allowance;
use super::buffer::{Buffer, Change, Live};
use super::path::Key;
use super::scope::{Kind, Scope};
use crate::engine::Home;
use crate::{Error, Result, Store, unix_millis, unix_nanos};

/// How often a rate limit's times reach kv.db, and so what a crash forgets.
const FLUSH: Duration = Duration::from_secs(1);

/// A rate limit being opened: its name, its rate and its burst.
#[must_use = "a rate limit opens with open()"]
pub struct RateLimitBuilder {
    store: Store,
    name: String,
    rate: Option<(u64, Duration)>,
    burst: Option<u64>,
}

impl Store {
    /// Requests let through at a steady rate a key, with room for a burst, in
    /// the store's kv.db: `store.rate_limit("api").rate(100, Duration::from_secs(1))`.
    pub fn rate_limit(&self, name: &str) -> RateLimitBuilder {
        RateLimitBuilder { store: self.clone(), name: name.to_owned(), rate: None, burst: None }
    }
}

impl RateLimitBuilder {
    /// Lets `count` requests a key through every `per`.
    pub fn rate(mut self, count: u64, per: Duration) -> Self {
        self.rate = Some((count, per));
        self
    }

    /// How many requests may pass at once before the rate holds the next; the
    /// rate's count unless said.
    pub fn burst(mut self, burst: u64) -> Self {
        self.burst = Some(burst);
        self
    }

    pub fn open(self) -> Result<RateLimit> {
        let (interval, burst) =
            check_rate(self.rate, self.burst).map_err(|error| error.within(format!("kv rate limit {}", self.name)))?;
        let scope = Scope::open(&Home::Store(self.store.clone()), &self.name, Kind::RateLimit)?;
        let buffer =
            scope.kv.buffer(scope.id, Some(FLUSH), &scope.shown()).map_err(|error| error.within(scope.shown()))?;
        let buffer = buffer.ok_or_else(|| Error::internal("a rate limit without its memory").within(scope.shown()))?;
        Ok(RateLimit { scope, buffer, interval, burst })
    }
}

/// Requests let through at a steady rate a key, with room for a burst.
///
/// It is the generic cell rate algorithm: a key holds one time, when its next
/// request is due, so no window has an edge that lets twice the rate through,
/// and a key costs one number.
///
/// ```text
/// rate(3, 3 s), burst 3: three pass at once, and the fourth may pass 1 s later
/// ```
///
/// The times live in memory and reach kv.db every second and at close: a
/// crash forgets at most the last second, which lets at most one burst more
/// through. A key whose time has come is as one never seen.
pub struct RateLimit {
    scope: Scope,
    buffer: Arc<Buffer>,
    /// The nanoseconds one request takes of the rate.
    interval: i64,
    burst: i64,
}

impl RateLimit {
    /// The same rate limit seen from the branch `owner` names under this one,
    /// whose keys are counted apart.
    pub fn under(&self, owner: impl Key) -> RateLimit {
        RateLimit { scope: self.scope.under(owner), ..self.clone() }
    }

    /// Asks for one request of `key`.
    pub fn allow(&self, key: impl Key) -> Result<Allowance> {
        self.allow_n(key, 1)
    }

    /// Asks for `n` requests of `key` at once: all of them pass or none does.
    /// More than the burst never passes, and is `Invalid`.
    pub fn allow_n(&self, key: impl Key, n: u64) -> Result<Allowance> {
        let key = key.text();
        let shown = || self.scope.shown_key(&key);
        let asked = i64::try_from(n).ok().filter(|&n| n >= 1 && n <= self.burst).ok_or_else(|| {
            Error::invalid(format!("{n} requests at once, past a burst of {}", self.burst)).within(shown())
        })?;
        let path = self.scope.path(&key)?;
        let now = self.scope.kv.clock().now();
        let step = |live: Option<Live>| {
            let due = live.map_or(0, |live| live.value);
            let (allowance, next) = self.decide(due, now, asked);
            let change = if allowance.ok {
                Change::Set(Live { value: next, expires: Some(millis_after(next)) })
            } else {
                Change::Leave
            };
            Ok((change, allowance))
        };
        self.buffer.step(&self.scope.kv, &path, unix_millis(now), step).map_err(|error| error.within(shown()))
    }

    /// Whether one request of `key` would pass now, using nothing.
    pub fn peek(&self, key: impl Key) -> Result<Allowance> {
        let key = key.text();
        let path = self.scope.path(&key)?;
        let now = self.scope.kv.clock().now();
        let live = self.buffer.get(&self.scope.kv, &path, unix_millis(now));
        let due = live.map_err(|error| error.within(self.scope.shown_key(&key)))?.map_or(0, |live| live.value);
        Ok(self.decide(due, now, 1).0)
    }

    /// Forgets `key`'s requests, so that its next one has the whole burst.
    pub fn reset(&self, key: impl Key) -> Result<()> {
        let key = key.text();
        let path = self.scope.path(&key)?;
        let now = unix_millis(self.scope.kv.clock().now());
        let step = |live: Option<Live>| Ok((if live.is_some() { Change::Remove } else { Change::Leave }, ()));
        self.buffer.step(&self.scope.kv, &path, now, step).map_err(|error| error.within(self.scope.shown_key(&key)))
    }

    /// The algorithm: `due` is when the key's next request is due, in unix
    /// nanoseconds, 0 for a key never seen or long quiet. Requests pass when,
    /// taken, they leave the key due no later than a burst from now.
    ///
    /// ```text
    /// interval 10 ms, burst 3, now 0, due 0:     next 10 ms ≤ 30 ms → ok, 2 left
    ///                          now 0, due 30 ms: next 40 ms > 30 ms → retry at 10 ms
    /// ```
    fn decide(&self, due: i64, now: SystemTime, n: i64) -> (Allowance, i64) {
        let nanos = unix_nanos(now);
        let start = due.max(nanos);
        let (next, limit) = (start + n * self.interval, nanos + self.burst * self.interval);
        if next > limit {
            let wait = Duration::from_nanos((next - limit).unsigned_abs());
            let left = ((limit - start) / self.interval).unsigned_abs();
            return (Allowance { ok: false, left, retry_at: Some(now + wait), windows: Vec::new() }, due);
        }
        let left = ((limit - next) / self.interval).unsigned_abs();
        (Allowance { ok: true, left, retry_at: None, windows: Vec::new() }, next)
    }
}

impl Clone for RateLimit {
    fn clone(&self) -> Self {
        RateLimit {
            scope: self.scope.clone(),
            buffer: Arc::clone(&self.buffer),
            interval: self.interval,
            burst: self.burst,
        }
    }
}

impl fmt::Debug for RateLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RateLimit({:?})", self.scope)
    }
}

impl fmt::Debug for RateLimitBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RateLimitBuilder({})", self.name)
    }
}

/// The nanoseconds a request takes and the burst, refused when they are no
/// rate, or a burst would pass what a time holds.
fn check_rate(rate: Option<(u64, Duration)>, burst: Option<u64>) -> Result<(i64, i64)> {
    let Some((count, per)) = rate else {
        return Err(Error::invalid("a rate limit needs a rate: .rate(100, Duration::from_secs(1))"));
    };
    let per_nanos = i64::try_from(per.as_nanos()).unwrap_or(i64::MAX);
    let count_most = i64::try_from(count).unwrap_or(i64::MAX);
    if count == 0 || per.is_zero() {
        return Err(Error::invalid("a rate of at least one request every positive span"));
    }
    if per_nanos < count_most {
        return Err(Error::invalid(format!("a rate of {count} every {per:?} is past one a nanosecond")));
    }
    let interval = per_nanos / count_most;
    let burst = i64::try_from(burst.unwrap_or(count)).unwrap_or(i64::MAX);
    if burst < 1 {
        return Err(Error::invalid("a burst of 0"));
    }
    if burst > i64::MAX / 4 / interval {
        return Err(Error::invalid(format!("a burst of {burst} at {interval} ns a request is past what a time holds")));
    }
    Ok((interval, burst))
}

/// A time in unix nanoseconds as an expiry: the first millisecond at or after it.
fn millis_after(nanos: i64) -> i64 {
    nanos.div_euclid(1_000_000) + i64::from(nanos.rem_euclid(1_000_000) != 0)
}

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod tests;
