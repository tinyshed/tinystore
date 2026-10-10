use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::Duration;

use super::allowance::{Allowance, Window as WindowUse};
use super::cells;
use super::engine::next_version;
use super::path::Key;
use super::scope::{Kind, Scope};
use super::value::Raw;
use crate::clock::{from_unix_millis, millis};
use crate::engine::Home;
use crate::sqlite::Tx;
use crate::{Error, Result, Store};

/// The windows one quota counts.
const MOST_WINDOWS: usize = 8;

/// The most a key's row of windows adds to a commit.
const ROW: usize = 2 << 10;

/// A quota being opened: its name and its windows.
#[must_use = "a quota opens with open()"]
pub struct QuotaBuilder {
    store: Store,
    name: String,
    windows: Vec<Window>,
}

impl Store {
    /// Uses a key may make in windows that each reset on their own, in the
    /// store's kv.db: `store.quota("ai").window("daily", 100, DAY)`.
    pub fn quota(&self, name: &str) -> QuotaBuilder {
        QuotaBuilder { store: self.clone(), name: name.to_owned(), windows: Vec::new() }
    }
}

impl QuotaBuilder {
    /// Lets a key use up to `limit` every `span` from its first use. A name is
    /// a lowercase letter, then up to 31 of them, digits and '_'; a quota has
    /// one to eight windows.
    pub fn window(mut self, name: &str, limit: u64, span: Duration) -> Self {
        self.windows.push(Window { name: name.to_owned(), limit, span: millis(span) });
        self
    }

    pub fn open(self) -> Result<Quota> {
        check_windows(&self.windows).map_err(|error| error.within(format!("kv quota {}", self.name)))?;
        let least = self.windows.iter().map(|window| window.limit).min().unwrap_or(0);
        let scope = Scope::open(&Home::Store(self.store.clone()), &self.name, Kind::Quota)?;
        Ok(Quota { scope, windows: self.windows.into(), least })
    }
}

/// Uses a key may make in each of its windows, such as 100 messages every five
/// hours and 300 a week, counted by one `allow`.
///
/// A window starts at a key's first use after the last one ended and lasts its
/// span, so each key resets on its own:
///
/// ```text
/// window("session", 100, 5 h): first use 13:42 → resets at 18:42;
///                              a use at 19:07 starts the next, which resets at 00:07
/// ```
///
/// An `allow` counts in every window or in none, in one durable write: two
/// quotas asked one after the other would leave the first counted when the
/// second refused.
pub struct Quota {
    scope: Scope,
    windows: Arc<[Window]>,
    /// The smallest limit, past which a use never passes.
    least: u64,
}

#[derive(Clone, Debug)]
struct Window {
    name: String,
    limit: u64,
    /// Milliseconds.
    span: i64,
}

/// What a key's row keeps: each window's use and when it resets, in unix
/// milliseconds, by the window's name.
type Held = BTreeMap<String, (u64, i64)>;

impl Quota {
    /// The same quota seen from the branch `owner` names under this one, whose
    /// keys are counted apart.
    pub fn under(&self, owner: impl Key) -> Quota {
        Quota { scope: self.scope.under(owner), windows: Arc::clone(&self.windows), least: self.least }
    }

    /// Uses one of each of `key`'s windows, or none when one has no room left.
    /// `retry_at` is when the windows that refused reset.
    pub fn allow(&self, key: impl Key) -> Result<Allowance> {
        self.allow_n(key, 1)
    }

    /// Uses `n` of each of `key`'s windows at once, or none when one has no
    /// room for `n`. More than a window's limit never passes, and is `Invalid`.
    pub fn allow_n(&self, key: impl Key, n: u64) -> Result<Allowance> {
        let key = key.text();
        if n == 0 || n > self.least {
            let message = format!("{n} at once, past the smallest window's limit of {}", self.least);
            return Err(Error::invalid(message).within(self.scope.shown_key(&key)));
        }
        let now = self.scope.now();
        let (id, path, windows) = (self.scope.id, self.scope.path(&key)?, Arc::clone(&self.windows));
        self.scope.write(&key, ROW, move |tx: &Tx<'_>, revision: &AtomicI64| {
            let (held, spill) = held(tx, id, &path, now)?;
            let (next, usage) = decide(&windows, &held, now, n);
            if usage.ok {
                keep(tx, revision, (id, &path), &next, spill)?;
            }
            Ok(usage)
        })
    }

    /// `key`'s windows, using nothing: `ok` says whether one more use would
    /// pass now.
    pub fn peek(&self, key: impl Key) -> Result<Allowance> {
        let key = key.text();
        let (id, path, now) = (self.scope.id, self.scope.path(&key)?, self.scope.now());
        let (held, _) = self.scope.read(&key, |connection| held(connection, id, &path, now))?;
        Ok(decide(&self.windows, &held, now, 0).1)
    }

    /// Gives one use back to each of `key`'s windows that has not reset since.
    pub fn refund(&self, key: impl Key) -> Result<()> {
        self.refund_n(key, 1)
    }

    /// Gives `n` uses back, never below nothing.
    pub fn refund_n(&self, key: impl Key, n: u64) -> Result<()> {
        let key = key.text();
        let now = self.scope.now();
        let (id, path) = (self.scope.id, self.scope.path(&key)?);
        self.scope.write(&key, ROW, move |tx: &Tx<'_>, revision: &AtomicI64| {
            let (held, spill) = held(tx, id, &path, now)?;
            let next: Held = held
                .into_iter()
                .filter(|(_, (_, resets))| *resets > now)
                .map(|(name, (used, resets))| (name, (used.saturating_sub(n), resets)))
                .collect();
            if next.is_empty() {
                return Ok(());
            }
            keep(tx, revision, (id, &path), &next, spill)
        })
    }

    /// Forgets `key`'s windows, so that its next use starts each anew.
    pub fn reset(&self, key: impl Key) -> Result<()> {
        let key = key.text();
        let now = self.scope.now();
        let (id, path) = (self.scope.id, self.scope.path(&key)?);
        self.scope.write(&key, 0, move |tx: &Tx<'_>, _: &AtomicI64| match cells::current(tx, id, &path, now)? {
            Some(current) => cells::remove(tx, id, &path, current.spill),
            None => Ok(()),
        })
    }
}

impl Clone for Quota {
    fn clone(&self) -> Self {
        Quota { scope: self.scope.clone(), windows: Arc::clone(&self.windows), least: self.least }
    }
}

impl fmt::Debug for Quota {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Quota({:?})", self.scope)
    }
}

impl fmt::Debug for QuotaBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QuotaBuilder({})", self.name)
    }
}

/// The algorithm. A window whose reset has come is one not started; `n`
/// passes when every window has room for it, and is then counted in each, a
/// window not started starting now. A check asks with 0 whether one more
/// would pass, and counts nothing.
///
/// ```text
/// session 100 a 5 h, weekly 300 a 7 d; used 37 and 300 → weekly refuses: nothing
/// is counted, and retry_at is weekly's reset
/// ```
fn decide(windows: &[Window], held: &Held, now: i64, n: u64) -> (Held, Allowance) {
    let asked = n.max(1);
    let mut kept: Vec<(u64, i64)> = windows.iter().map(|window| current(held, window, now)).collect();
    let refusing: Vec<i64> = windows
        .iter()
        .zip(&kept)
        .filter(|(window, (used, _))| used + asked > window.limit)
        .map(|(_, (_, resets))| *resets)
        .collect();
    let ok = refusing.is_empty();
    if ok && n > 0 {
        for (window, (used, resets)) in windows.iter().zip(kept.iter_mut()) {
            *used += n;
            if *resets == 0 {
                *resets = now + window.span;
            }
        }
    }
    let usage = Allowance {
        ok,
        left: windows
            .iter()
            .zip(&kept)
            .map(|(window, (used, _))| window.limit.saturating_sub(*used))
            .min()
            .unwrap_or(0),
        retry_at: refusing.into_iter().max().map(from_unix_millis),
        windows: windows.iter().zip(&kept).map(|(window, kept)| shown(window, *kept)).collect(),
    };
    let next = windows.iter().zip(kept).map(|(window, kept)| (window.name.clone(), kept)).collect();
    (next, usage)
}

/// A window's use and reset as `held` keeps them, `(0, 0)` once it reset.
fn current(held: &Held, window: &Window, now: i64) -> (u64, i64) {
    match held.get(&window.name) {
        Some(&(used, resets)) if resets > now => (used, resets),
        _ => (0, 0),
    }
}

fn shown(window: &Window, (used, resets): (u64, i64)) -> WindowUse {
    WindowUse {
        name: window.name.clone(),
        used,
        limit: window.limit,
        left: window.limit.saturating_sub(used),
        resets_at: (resets != 0).then(|| from_unix_millis(resets)),
    }
}

/// What a key's row keeps of its windows, nothing for a key not used or one
/// whose windows all ended, and the row its value spilled to, which a write
/// replaces.
fn held(connection: &rusqlite::Connection, id: i64, path: &[u8], now: i64) -> Result<(Held, Option<i64>)> {
    let spill = cells::current(connection, id, path, now)?.and_then(|current| current.spill);
    let Some(cell) = cells::live(connection, id, path, now)? else {
        return Ok((Held::new(), spill));
    };
    let Raw::Bytes(bytes) = cell.raw else {
        return Err(Error::corrupt("a quota's row is not the windows it wrote"));
    };
    let held =
        serde_json::from_slice(&bytes).map_err(|_| Error::corrupt("a quota's row is not the windows it wrote"))?;
    Ok((held, spill))
}

/// Writes a key's windows, the row expiring as the last of them resets.
fn keep(tx: &Tx<'_>, revision: &AtomicI64, (id, path): (i64, &[u8]), next: &Held, spill: Option<i64>) -> Result<()> {
    let row = serde_json::to_vec(next).map_err(|error| Error::internal(format!("a quota's row: {error}")))?;
    let expires = next.values().map(|(_, resets)| *resets).max();
    let version = next_version(revision, tx)?;
    cells::put(tx, id, path, (version, expires), &Raw::Bytes(row), spill)
}

fn check_windows(windows: &[Window]) -> Result<()> {
    if windows.is_empty() || windows.len() > MOST_WINDOWS {
        return Err(Error::invalid(format!("{} windows, not one to {MOST_WINDOWS}", windows.len())));
    }
    for (index, window) in windows.iter().enumerate() {
        check_window(window)?;
        if windows[..index].iter().any(|before| before.name == window.name) {
            return Err(Error::invalid(format!("two windows named {:?}", window.name)));
        }
    }
    Ok(())
}

fn check_window(window: &Window) -> Result<()> {
    let bytes = window.name.as_bytes();
    let named = bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.len() <= 32
        && bytes.iter().all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !named {
        return Err(Error::invalid(format!(
            "a window's name is a lowercase letter, then up to 31 of them, digits and '_', not {:?}",
            window.name
        )));
    }
    if window.limit == 0 || window.span < 1 {
        return Err(Error::invalid(format!(
            "window {:?} of {} every {} ms, not at least one every millisecond",
            window.name, window.limit, window.span
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;
