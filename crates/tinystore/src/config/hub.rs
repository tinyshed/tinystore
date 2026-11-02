//! What every handle of one config in a process shares: the changes kept of
//! it and the config's version, read once and then moved by each change, and
//! the handles that follow them. A change reaches every handle from here,
//! once, however many handles and watchers the process holds.
//!
//! A hub's keeper keeps the changes: kv.db, for a config opened in a store
//! (`store.rs`). A config loaded without a store has no keeper: it keeps no
//! change, and a change it is asked to keep is refused.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use serde_json::Value;

use super::Keeper;
use super::declaration::Declaration;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// The changes a config keeps, by the setting's path, and the version that
/// last moved them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Kept {
    pub(crate) version: i64,
    pub(crate) changes: BTreeMap<String, Change>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Change {
    pub(crate) value: Value,
    /// The config's version that made it.
    pub(crate) version: i64,
    /// When, in unix milliseconds.
    pub(crate) at: i64,
}

impl Kept {
    /// The changes in the order they were made.
    pub(crate) fn in_order(&self) -> Vec<(&String, &Change)> {
        let mut changes: Vec<_> = self.changes.iter().collect();
        changes.sort_by_key(|(_, change)| change.version);
        changes
    }
}

/// A handle of the config, as its hub sees it.
pub(super) trait Follower: Send + Sync {
    fn declaration(&self) -> &Declaration;

    /// Takes the changes as they are once committed; a handle that has seen
    /// a later version ignores them.
    fn follow(&self, kept: &Arc<Kept>);
}

pub(crate) struct Hub {
    /// What keeps the changes; none for a config loaded without a store.
    keeper: Option<Keeper>,
    state: Mutex<State>,
}

struct State {
    kept: Arc<Kept>,
    followers: Vec<Weak<dyn Follower>>,
}

/// What a change found: it was made, or another came first.
#[cfg_attr(not(feature = "config"), expect(dead_code, reason = "only kv.db keeps a change"))]
pub(super) enum Written {
    Made(Kept),
    Moved(Kept),
}

impl Hub {
    /// The hub of a config whose `keeper` read `kept` of it, and keeps the
    /// changes that follow.
    pub(super) fn new(keeper: Option<Keeper>, kept: Kept) -> Arc<Hub> {
        let state = State { kept: Arc::new(kept), followers: Vec::new() };
        Arc::new(Hub { keeper, state: Mutex::new(state) })
    }

    /// The hub of a config loaded without a store, which keeps no change.
    pub(super) fn detached() -> Arc<Hub> {
        Hub::new(None, Kept::default())
    }

    /// What keeps the changes; refused for a config loaded without a store.
    pub(super) fn keeper(&self) -> Result<&Keeper> {
        self.keeper.as_ref().ok_or_else(|| {
            Error::invalid("a config loaded without a store keeps no change: open it with store.config to change it")
        })
    }

    pub(super) fn kept(&self) -> Arc<Kept> {
        Arc::clone(&lock(&self.state).kept)
    }

    pub(super) fn join(&self, follower: Weak<dyn Follower>) {
        let mut state = lock(&self.state);
        state.followers.retain(|follower| follower.strong_count() > 0);
        state.followers.push(follower);
    }

    /// The handles of the config open in this process.
    pub(super) fn followers(&self) -> Vec<Arc<dyn Follower>> {
        lock(&self.state).followers.iter().filter_map(Weak::upgrade).collect()
    }

    /// Keeps `leaves` as the config's next version while its version is still
    /// `expected`, and is `conflict` when another change came first; either
    /// way the changes as they are then reach every handle.
    pub(super) fn change(&self, expected: i64, leaves: Vec<(String, Value)>) -> Result<Arc<Kept>> {
        let written = self.keeper()?.change(expected, leaves)?;
        self.published(written)
    }

    /// Drops the changes kept at `path` and under it, every change for an
    /// empty path, and says whether there were any: never, for a config
    /// loaded without a store.
    pub(super) fn reset(&self, path: &str) -> Result<bool> {
        let Some(keeper) = &self.keeper else { return Ok(false) };
        let (kept, dropped) = keeper.reset(path)?;
        self.publish(kept);
        Ok(dropped)
    }

    /// Keeps the declaration a program opened the config with, as JSON, when
    /// it is not the one its keeper keeps already.
    pub(super) fn remember(&self, declaration: &Declaration) -> Result<()> {
        let keeper = self.keeper()?;
        keeper.remember(declaration.to_json().to_string())
    }

    /// Hands what a change wrote to every handle: `conflict` when another
    /// change came first.
    pub(super) fn published(&self, written: Written) -> Result<Arc<Kept>> {
        match written {
            Written::Made(kept) => Ok(self.publish(kept)),
            Written::Moved(kept) => {
                self.publish(kept);
                Err(Error::new(ErrorKind::Conflict, "changed since the version the change was made on"))
            }
        }
    }

    /// The changes as they are now, for every handle. Commits may answer out
    /// of order, so an older version than the hub has is not taken.
    fn publish(&self, kept: Kept) -> Arc<Kept> {
        let (kept, followers) = {
            let mut state = lock(&self.state);
            if kept.version > state.kept.version {
                state.kept = Arc::new(kept);
            }
            (Arc::clone(&state.kept), state.followers.iter().filter_map(Weak::upgrade).collect::<Vec<_>>())
        };
        for follower in &followers {
            follower.follow(&kept);
        }
        kept
    }
}
