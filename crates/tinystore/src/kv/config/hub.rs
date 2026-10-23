//! What every handle of one config in a process shares: the changes kept in
//! kv.db and the config's version, read once and then moved by each change,
//! and the handles that follow them. A change reaches every handle from here,
//! once, however many handles and watchers the process holds.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::declaration::Declaration;
use crate::kv::engine::{Kv, next_version};
use crate::sqlite::sql_error;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

const SELECT_CONFIG: &str = "select version, declaration from _tinystore_kv_configs where name = ?1";
const SELECT_CHANGES: &str = "select path, value, version, at from _tinystore_kv_config_changes where name = ?1";
const KEEP_VERSION: &str = "insert into _tinystore_kv_configs (name, version) values (?1, ?2)
    on conflict (name) do update set version = excluded.version";
const KEEP_DECLARATION: &str = "insert into _tinystore_kv_configs (name, version, declaration) values (?1, 0, ?2)
    on conflict (name) do update set declaration = excluded.declaration";
const UPSERT_CHANGE: &str = "insert into _tinystore_kv_config_changes (name, path, value, version, at)
    values (?1, ?2, ?3, ?4, ?5)
    on conflict (name, path) do update set value = excluded.value, version = excluded.version, at = excluded.at";
const DELETE_UNDER: &str = "delete from _tinystore_kv_config_changes
    where name = ?1 and (?2 = '' or path = ?2 or substr(path, 1, length(?2) + 1) = ?2 || '.')";
const KEPT_BYTES: &str = "select coalesce(sum(length(cast(path as blob)) + length(cast(value as blob))), 0)
    from _tinystore_kv_config_changes where name = ?1";

/// What a config keeps of its changes, all of them together.
const MOST_KEPT: i64 = 256 << 10;

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
    kv: Arc<Kv>,
    name: String,
    state: Mutex<State>,
}

struct State {
    kept: Arc<Kept>,
    /// The declaration kv.db keeps, the last a program opened the config with.
    remembered: Option<String>,
    followers: Vec<Weak<dyn Follower>>,
}

/// What a change found: it was made, or another came first.
enum Written {
    Made(Kept),
    Moved(Kept),
}

impl Hub {
    /// The hub of `name` in this process: the one open, or one read from kv.db.
    pub(super) fn of(kv: &Arc<Kv>, name: &str) -> Result<Arc<Hub>> {
        let mut hubs = lock(kv.configs());
        if let Some(hub) = hubs.get(name).and_then(Weak::upgrade) {
            return Ok(hub);
        }
        let owned = name.to_owned();
        let (kept, remembered) = kv.file().read(move |connection| read(connection, &owned))?;
        let state = State { kept: Arc::new(kept), remembered, followers: Vec::new() };
        let hub = Arc::new(Hub { kv: Arc::clone(kv), name: name.to_owned(), state: Mutex::new(state) });
        hubs.retain(|_, hub| hub.strong_count() > 0);
        hubs.insert(name.to_owned(), Arc::downgrade(&hub));
        Ok(hub)
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
        let (name, revision, now) = (self.name.clone(), self.kv.revision(), self.kv.now());
        let bytes = leaves.iter().map(|(path, value)| path.len() + value.to_string().len()).sum();
        let written = self.kv.file().write(bytes, move |tx| {
            let (kept, _) = read(tx, &name)?;
            if kept.version != expected {
                return Ok(Written::Moved(kept));
            }
            let version = next_version(&revision, tx);
            for (path, value) in &leaves {
                let value = value.to_string();
                execute(tx, UPSERT_CHANGE, params![name, path, value, version, now], "a change")?;
            }
            refuse_past_bound(tx, &name)?;
            execute(tx, KEEP_VERSION, params![name, version], "its version")?;
            Ok(Written::Made(read(tx, &name)?.0))
        })?;
        match written {
            Written::Made(kept) => Ok(self.publish(kept)),
            Written::Moved(kept) => {
                self.publish(kept);
                Err(Error::new(ErrorKind::Conflict, "changed since the version the change was made on"))
            }
        }
    }

    /// Drops the changes kept at `path` and under it, every change for an
    /// empty path, and says whether there were any.
    pub(super) fn reset(&self, path: &str) -> Result<bool> {
        let (name, revision, path) = (self.name.clone(), self.kv.revision(), path.to_owned());
        let (kept, dropped) = self.kv.file().write(path.len(), move |tx| {
            let dropped = tx
                .prepare_cached(DELETE_UNDER)
                .and_then(|mut delete| delete.execute(params![name, path]))
                .map_err(|error| sql_error("its changes", error))?;
            if dropped > 0 {
                let version = next_version(&revision, tx);
                execute(tx, KEEP_VERSION, params![name, version], "its version")?;
            }
            Ok((read(tx, &name)?.0, dropped > 0))
        })?;
        self.publish(kept);
        Ok(dropped)
    }

    /// Keeps the declaration a program opened the config with, when it is
    /// not the one kv.db keeps already.
    pub(super) fn remember(&self, declaration: &Declaration) -> Result<()> {
        let text = declaration.to_json().to_string();
        if lock(&self.state).remembered.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        let (name, kept) = (self.name.clone(), text.clone());
        self.kv
            .file()
            .write(text.len(), move |tx| execute(tx, KEEP_DECLARATION, params![name, kept], "its declaration"))?;
        lock(&self.state).remembered = Some(text);
        Ok(())
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

/// The changes `name` keeps, and the declaration kept with them.
fn read(connection: &Connection, name: &str) -> Result<(Kept, Option<String>)> {
    let config: Option<(i64, Option<String>)> = connection
        .prepare_cached(SELECT_CONFIG)
        .and_then(|mut select| select.query_row([name], |row| Ok((row.get(0)?, row.get(1)?))).optional())
        .map_err(|error| sql_error("its version", error))?;
    let (version, remembered) = config.unwrap_or((0, None));
    let mut select = connection.prepare_cached(SELECT_CHANGES).map_err(|error| sql_error("its changes", error))?;
    let rows = select
        .query_map([name], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get(3)?)))
        .map_err(|error| sql_error("its changes", error))?;
    let mut changes = BTreeMap::new();
    for row in rows {
        let (path, text, changed_at_version, at) = row.map_err(|error| sql_error("its changes", error))?;
        let value = serde_json::from_str(&text)
            .map_err(|error| Error::corrupt(format!("the change of {path} is not JSON: {error}")))?;
        changes.insert(path, Change { value, version: changed_at_version, at });
    }
    Ok((Kept { version, changes }, remembered))
}

fn refuse_past_bound(connection: &Connection, name: &str) -> Result<()> {
    let bytes: i64 = connection
        .prepare_cached(KEPT_BYTES)
        .and_then(|mut select| select.query_row([name], |row| row.get(0)))
        .map_err(|error| sql_error("its changes' size", error))?;
    if bytes > MOST_KEPT {
        return Err(Error::limit(format!("its changes would hold {bytes} bytes, past the {MOST_KEPT} a config keeps")));
    }
    Ok(())
}

use crate::sqlite::execute_cached as execute;
