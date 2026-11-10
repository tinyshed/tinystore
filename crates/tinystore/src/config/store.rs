//! A config opened in a store, with the `config` feature: `store.config`, and
//! its keeper, kv.db, which keeps the live settings' changes and the
//! declaration a program last opened the config with. The one file of the
//! config that names an engine: kv lends it its file, the version every
//! write of that file moves, and the configs open in the process.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

#[cfg(doc)]
use super::Declaration;
use super::hub::{Change, Hub, Kept, Written};
#[cfg(any(test, feature = "wire", feature = "records"))]
use super::{Config, data, handle, resolve};
use super::{ConfigBuilder, IntoDeclaration};
use crate::kv::{Kv, next_version};
use crate::limits::CONFIG_BYTES;
#[cfg(feature = "async")]
use crate::polled::Lead;
use crate::sqlite::{Tx, execute_cached as execute, sql_error};
use crate::sync::lock;
use crate::{Error, Result, Store};

const SELECT_CONFIG: &str = "select version, declaration from _tinystore_kv_configs where name = ?1";
#[cfg(feature = "client")]
const SELECT_NAMES: &str = "select name from _tinystore_kv_configs where declaration is not null order by name";
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
const MOST_KEPT: i64 = CONFIG_BYTES.bound as i64;

impl Store {
    /// The program's settings as one typed value. `declaration` is every
    /// setting with its default, a `serde` struct whose fields may be marked
    /// `Secret`, `Required` or `Live`, or a [`Declaration`] as data, read as
    /// a `serde_json::Value`; the layers the builder adds go over it in the
    /// order written.
    pub fn config<D: IntoDeclaration>(&self, name: &str, declaration: D) -> ConfigBuilder<D::Value> {
        ConfigBuilder { store: Some(self.clone()), ..super::load(name, declaration) }
    }
}

/// kv.db, as the hub of a config opened in a store sees it: what keeps the
/// config's changes, and the declaration a program last opened it with.
pub(super) struct Keeper {
    kv: Arc<Kv>,
    name: String,
    /// The declaration kv.db keeps, the last a program opened the config with.
    remembered: Mutex<Option<String>>,
    #[cfg(feature = "async")]
    closing: Mutex<Closing>,
}

impl Hub {
    /// The hub of `name` in this process: the one open, or one read from kv.db.
    pub(super) fn of(store: &Store, name: &str) -> Result<Arc<Hub>> {
        let kv = Kv::of(store)?;
        let mut hubs = lock(kv.configs());
        if let Some(hub) = hubs.get(name).and_then(Weak::upgrade) {
            return Ok(hub);
        }
        let owned = name.to_owned();
        let (kept, remembered) = kv.file().read(move |connection| read(connection, &owned))?;
        let keeper = Keeper {
            kv: Arc::clone(&kv),
            name: name.to_owned(),
            remembered: Mutex::new(remembered),
            #[cfg(feature = "async")]
            closing: Mutex::default(),
        };
        let hub = Hub::new(Some(keeper), kept);
        hubs.retain(|_, hub| hub.strong_count() > 0);
        hubs.insert(name.to_owned(), Arc::downgrade(&hub));
        Ok(hub)
    }
}

impl Keeper {
    /// Keeps `leaves` as the config's next version while its version is still
    /// `expected`; what is kept then, either way.
    pub(super) fn change(&self, expected: i64, leaves: Vec<(String, Value)>) -> Result<Written> {
        let (bytes, work) = self.change_work(expected, leaves);
        self.kv.file().write(bytes, work)
    }

    /// The write that keeps `leaves` while the config's version is still
    /// `expected`, and the bytes it adds to its commit.
    fn change_work(
        &self,
        expected: i64,
        leaves: Vec<(String, Value)>,
    ) -> (usize, impl FnOnce(&Tx<'_>) -> Result<Written> + Send + 'static) {
        let (name, revision, now) = (self.name.clone(), self.kv.revision(), self.kv.now());
        let bytes = leaves.iter().map(|(path, value)| path.len() + value.to_string().len()).sum();
        let work = move |tx: &Tx<'_>| {
            let (kept, _) = read(tx, &name)?;
            if kept.version != expected {
                return Ok(Written::Moved(kept));
            }
            let version = next_version(&revision, tx)?;
            for (path, value) in &leaves {
                let value = value.to_string();
                execute(tx, UPSERT_CHANGE, params![name, path, value, version, now], "a change")?;
            }
            refuse_past_bound(tx, &name)?;
            execute(tx, KEEP_VERSION, params![name, version], "its version")?;
            Ok(Written::Made(read(tx, &name)?.0))
        };
        (bytes, work)
    }

    /// Drops the changes kept at `path` and under it, every change for an
    /// empty path: what is kept then, and whether there were any.
    pub(super) fn reset(&self, path: &str) -> Result<(Kept, bool)> {
        let (bytes, work) = self.reset_work(path);
        self.kv.file().write(bytes, work)
    }

    /// The write that drops the changes kept at `path` and under it, and the
    /// bytes it adds to its commit.
    fn reset_work(&self, path: &str) -> (usize, impl FnOnce(&Tx<'_>) -> Result<(Kept, bool)> + Send + 'static) {
        let (name, revision, path) = (self.name.clone(), self.kv.revision(), path.to_owned());
        let bytes = path.len();
        let work = move |tx: &Tx<'_>| {
            let dropped = tx
                .prepare_cached(DELETE_UNDER)
                .and_then(|mut delete| delete.execute(params![name, path]))
                .map_err(|error| sql_error("its changes", error))?;
            if dropped > 0 {
                let version = next_version(&revision, tx)?;
                execute(tx, KEEP_VERSION, params![name, version], "its version")?;
            }
            Ok((read(tx, &name)?.0, dropped > 0))
        };
        (bytes, work)
    }

    /// Keeps `text`, the declaration a program opened the config with, when it
    /// is not the one kv.db keeps already.
    pub(super) fn remember(&self, text: String) -> Result<()> {
        if lock(&self.remembered).as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        let (name, kept) = (self.name.clone(), text.clone());
        self.kv
            .file()
            .write(text.len(), move |tx| execute(tx, KEEP_DECLARATION, params![name, kept], "its declaration"))?;
        *lock(&self.remembered) = Some(text);
        Ok(())
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
        let refused = format!("its changes would hold {bytes} bytes, past the {MOST_KEPT} a config keeps");
        return Err(CONFIG_BYTES.refuse(bytes as u64, refused));
    }
    Ok(())
}

/// The configs kv.db keeps a declaration of, by name, for the command line's
/// completion and `status --json`: none while the store has no kv.db.
#[cfg(feature = "client")]
pub(crate) fn names(store: &Store) -> Result<Vec<String>> {
    if !store.dir().join("kv.db").exists() {
        return Ok(Vec::new());
    }
    Kv::of(store)?.file().read(|connection| {
        let mut select = connection.prepare_cached(SELECT_NAMES).map_err(|error| sql_error("its configs", error))?;
        let rows = select.query_map([], |row| row.get(0));
        rows.and_then(Iterator::collect).map_err(|error| sql_error("its configs", error))
    })
}

/// A config as a program's SDK opens it over the wire, and a store's logger
/// its own: its declaration and its layers as data, as `data` reads them.
#[cfg(any(test, feature = "wire", feature = "records"))]
pub(crate) struct Sent<'a> {
    pub(crate) name: &'a str,
    pub(crate) declaration: &'a Value,
    pub(crate) layers: &'a Value,
    pub(crate) strict: bool,
    /// Whether kv.db keeps the declaration: not for a connection that may
    /// only read.
    pub(crate) remember: bool,
}

#[cfg(any(test, feature = "wire", feature = "records"))]
impl Store {
    /// Opens the config an SDK sent, its value given back as data.
    pub(crate) fn config_sent(&self, sent: &Sent<'_>) -> Result<Config<Value>> {
        let within = |why: String| Error::invalid(why).within(format!("config {}", sent.name));
        let declaration = data::declaration(sent.declaration).map_err(within)?;
        let layers = data::layers(sent.layers).map_err(within)?;
        handle::open_with(handle::Opening {
            store: Some(self.clone()),
            name: sent.name.to_owned(),
            declaration,
            layers,
            validate: None,
            strict: sent.strict,
            view: resolve::View::Data,
            remember: sent.remember,
            kept: false,
        })
    }

    /// Opens the config an SDK sent, which remembers nothing, for a connection
    /// that may only read: a store without kv.db, which keeps a config's
    /// changes, is `NotFound` rather than given one.
    #[cfg(feature = "wire")]
    pub(crate) fn config_sent_as_it_is(&self, sent: &Sent<'_>) -> Result<Config<Value>> {
        self.holds("kv.db").map_err(|error| error.within(format!("config {}", sent.name)))?;
        self.config_sent(sent)
    }

    /// The config `name` as the store keeps it, for a command: the last
    /// declaration a program opened it with and the changes kv.db keeps,
    /// with no layer of the program's own, its required settings left out;
    /// nothing is remembered. None when no program opened it here.
    #[cfg(feature = "wire")]
    pub(crate) fn config_kept(&self, name: &str) -> Result<Option<Config<Value>>> {
        let within = |why: String| Error::corrupt(why).within(format!("config {name}"));
        let Some(text) = self.kept_declaration(name)? else { return Ok(None) };
        let value: Value =
            serde_json::from_str(&text).map_err(|error| within(format!("its kept declaration: {error}")))?;
        let declaration = data::declaration(&value).map_err(within)?;
        let opened = handle::open_with(handle::Opening {
            store: Some(self.clone()),
            name: name.to_owned(),
            declaration,
            layers: Vec::new(),
            validate: None,
            strict: false,
            view: resolve::View::Data,
            remember: false,
            kept: true,
        });
        opened.map(Some)
    }

    /// The declaration kv.db keeps of the config `name`, the last a program
    /// opened it with; none when no program opened it, or the store has no
    /// kv.db yet, which a store opened beside would otherwise try to make.
    #[cfg(feature = "wire")]
    fn kept_declaration(&self, name: &str) -> Result<Option<String>> {
        if !self.dir().join("kv.db").exists() {
            return Ok(None);
        }
        let hub = Hub::of(self, name)?;
        let remembered = lock(&hub.keeper()?.remembered).clone();
        Ok(remembered)
    }
}

/// What is told when the store closes: an awaited watch, which then ends.
#[cfg(feature = "async")]
pub(crate) type Hook = Weak<dyn Fn() + Send + Sync>;

#[cfg(feature = "async")]
#[derive(Default)]
struct Closing {
    closed: bool,
    hooks: Vec<Hook>,
}

/// What the async API's handle of a config awaits: a change or a reset
/// submitted to kv.db's writer without waiting, and the store's close.
#[cfg(feature = "async")]
impl Hub {
    /// Whether the config was loaded without a store, and keeps no change.
    pub(super) fn keeps_nothing(&self) -> bool {
        self.keeper().is_err()
    }

    /// The store the config was opened in, while it is open.
    pub(super) fn store(&self) -> Result<Store> {
        self.keeper()?.kv.store()
    }

    /// `change` without waiting, for an awaited call: `done` gets what was
    /// written, on the thread that commits it, and publishes nothing there,
    /// since publishing runs the program's `validate` and its watchers while
    /// the commits behind it wait. A write refused before it is queued is
    /// answered at once.
    pub(super) fn change_then(
        self: &Arc<Self>,
        (expected, leaves): (i64, Vec<(String, Value)>),
        elsewhere: Lead,
        done: impl FnOnce(Result<Unpublished>) + Send + 'static,
    ) {
        let keeper = match self.keeper() {
            Ok(keeper) => keeper,
            Err(error) => return done(Err(error)),
        };
        let (bytes, work) = keeper.change_work(expected, leaves);
        let (hub, name) = (Arc::clone(self), keeper.name.clone());
        let written = move |written: Result<Written>| {
            done(written.map(|written| Unpublished::of(hub, name, written, elsewhere)));
        };
        keeper.kv.file().submit(bytes, work, written);
    }

    /// `reset` without waiting, as [`Hub::change_then`] is `change`.
    pub(super) fn reset_then(
        self: &Arc<Self>,
        path: &str,
        elsewhere: Lead,
        done: impl FnOnce(Result<Unpublished>) + Send + 'static,
    ) {
        let keeper = match self.keeper() {
            Ok(keeper) => keeper,
            Err(error) => return done(Err(error)),
        };
        let (bytes, work) = keeper.reset_work(path);
        let (hub, name) = (Arc::clone(self), keeper.name.clone());
        let written = move |written: Result<(Kept, bool)>| {
            done(written.map(|(kept, _)| Unpublished::of(hub, name, Written::Made(kept), elsewhere)));
        };
        keeper.kv.file().submit(bytes, work, written);
    }

    /// Calls `hook` once, when the store closes, unless it was dropped first;
    /// false, and nothing called, once the store has closed. A config loaded
    /// without a store never closes.
    pub(super) fn on_close(&self, hook: Hook) -> bool {
        let Ok(keeper) = self.keeper() else { return true };
        let mut closing = lock(&keeper.closing);
        if closing.closed {
            return false;
        }
        closing.hooks.retain(|hook| hook.strong_count() > 0);
        closing.hooks.push(hook);
        true
    }

    /// Tells every hook that the store closes, once, outside the lock.
    pub(crate) fn close(&self) {
        let Ok(keeper) = self.keeper() else { return };
        let hooks = {
            let mut closing = lock(&keeper.closing);
            closing.closed = true;
            std::mem::take(&mut closing.hooks)
        };
        for hook in hooks.iter().filter_map(Weak::upgrade) {
            hook();
        }
    }
}

/// What an awaited change wrote, until it reaches the config's handles. The
/// future that awaits it publishes it once its commit answered, on the
/// thread that polls it, before the future answers. One dropped first, its
/// answer unread, hands it to `elsewhere`, the store's pool: a handle that
/// missed it would lag behind kv.db until the next change.
#[cfg(feature = "async")]
pub(crate) struct Unpublished {
    hub: Arc<Hub>,
    name: String,
    /// None once published.
    written: Option<Written>,
    elsewhere: Lead,
}

#[cfg(feature = "async")]
impl Unpublished {
    fn of(hub: Arc<Hub>, name: String, written: Written, elsewhere: Lead) -> Unpublished {
        Unpublished { hub, name, written: Some(written), elsewhere }
    }

    /// Hands what was written to every handle and its watchers, then answers
    /// as the blocking change does: `conflict` when another change came first.
    pub(crate) fn publish(mut self) -> Result<()> {
        let Some(written) = self.written.take() else { return Ok(()) };
        self.hub.published(written).map(drop).map_err(|error| error.within(format!("config {}", self.name)))
    }
}

#[cfg(feature = "async")]
impl Drop for Unpublished {
    fn drop(&mut self) {
        if let Some(written) = self.written.take() {
            let hub = Arc::clone(&self.hub);
            (self.elsewhere)(Box::new(move || drop(hub.published(written))));
        }
    }
}
