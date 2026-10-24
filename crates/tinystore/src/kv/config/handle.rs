//! A config's handle: its value in memory, its changes, its watchers. Every
//! handle of a name in the process follows one hub, which a change reaches
//! once; each handle then makes its own value and calls its own watchers.

use std::collections::HashSet;
use std::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError, Weak};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::ConfigBuilder;
use super::declaration::{Declaration, Kind, Setting, unmarked};
#[cfg(any(test, feature = "wire"))]
use super::declaration::{Group, Node, joined, shown};
use super::explain::{Explained, Given, Line, Origin};
use super::hub::{Change, Follower, Hub, Kept};
#[cfg(feature = "async")]
use super::hub::{Hook, Unpublished};
use super::layers::{Base, Layer, Stacked, set, shown_value};
use super::marks::marked;
use super::resolve::{Making, Resolved, Validate, View};
use super::rules::{self, described};
use crate::clock::from_unix_millis;
use crate::kv::engine::Kv;
use crate::kv::scope::check_name;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result, Store};

/// The tries a change makes while others race it before it gives up.
const ATTEMPTS: usize = 8;
/// The longest JSON one setting's change holds.
const MOST_VALUE: usize = 16 << 10;

/// A program's settings as one typed value, read from memory, with the
/// changes made while the program runs to the settings marked `Live`.
pub struct Config<T> {
    inner: Arc<Inner<T>>,
}

/// The settings as they were at one version of the config's changes: a
/// change makes a new one, and one held keeps what it had.
pub struct Snapshot<T> {
    value: Arc<T>,
    version: i64,
}

impl<T> Snapshot<T> {
    /// The version of the changes it holds, which `if_version` takes.
    pub fn version(&self) -> i64 {
        self.version
    }
}

impl<T> Clone for Snapshot<T> {
    fn clone(&self) -> Self {
        Snapshot { value: Arc::clone(&self.value), version: self.version }
    }
}

impl<T> Deref for Snapshot<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Snapshot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot").field("version", &self.version).field("value", &self.value).finish()
    }
}

/// A change made only while the config is still at a version:
/// `config.if_version(shown.version()).update(…)`.
pub struct IfVersion<'a, T> {
    config: &'a Config<T>,
    version: i64,
}

/// A watch of a config, which stops when it is dropped.
#[must_use = "a watch stops when its guard is dropped"]
pub struct Watching {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

type WatchFn<T> = dyn Fn(&Snapshot<T>, Option<&Snapshot<T>>) + Send + Sync;

/// A watcher a delivery calls: its id, its function and the value it saw last.
type Due<T> = (u64, Arc<WatchFn<T>>, Option<Snapshot<T>>);

struct Inner<T> {
    name: String,
    hub: Arc<Hub>,
    declaration: Declaration,
    base: Base,
    /// The value the layers alone make, and the config's keeping of it.
    layered: (Arc<T>, Value),
    validate: Option<Validate<T>>,
    view: View,
    current: Mutex<Current<T>>,
    watchers: Watchers<T>,
    /// The ignored changes said in the log already, by path and version.
    warned: Mutex<HashSet<(String, i64)>>,
}

/// The value at the version of the changes it took them from.
struct Current<T> {
    version: i64,
    resolved: Resolved<T>,
}

/// What a config opens with: its declaration as the core reads it, its
/// layers, the program's check, and how its value reaches the program.
pub(super) struct Opening<T> {
    pub(super) store: Store,
    pub(super) name: String,
    pub(super) declaration: Declaration,
    pub(super) layers: Vec<Layer>,
    pub(super) validate: Option<Validate<T>>,
    pub(super) strict: bool,
    pub(super) view: View,
    /// Whether kv.db keeps the declaration: not for a connection that may
    /// only read.
    pub(super) remember: bool,
}

/// Opens the config a Rust program's builder describes.
pub(super) fn open<T>(builder: ConfigBuilder<T>) -> Result<Config<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let within = |error: Error| error.within(format!("config {}", builder.name));
    check_name(&builder.name).map_err(within)?;
    let declaration = declared(&builder.declaration).map_err(within)?;
    let (store, name, layers, validate, strict) =
        (builder.store, builder.name, builder.layers, builder.validate, builder.strict);
    open_with(Opening { store, name, declaration, layers, validate, strict, view: View::Rust, remember: true })
}

/// Opens a config: its layers read, its value made and checked, then the
/// changes kept in kv.db applied.
pub(super) fn open_with<T>(opening: Opening<T>) -> Result<Config<T>>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let shown = format!("config {}", opening.name);
    let within = |error: Error| error.within(&shown);
    check_name(&opening.name).map_err(within)?;
    let making = Making { declaration: &opening.declaration, validate: opening.validate.as_ref(), view: opening.view };
    let base = Base::read(&opening.declaration, &opening.layers).map_err(|problems| within(refused(problems)))?;
    let (value, tree) = making.layered(&base).map_err(|problems| within(refused(problems)))?;
    let hub = Hub::of(&Kv::of(&opening.store).map_err(within)?, &opening.name).map_err(within)?;
    let layered = (Arc::new(value), tree);
    let kept = hub.kept();
    let resolved = making.resolve((&layered.0, &layered.1), &kept);
    if opening.strict && !resolved.ignored.is_empty() {
        let lines = resolved.ignored.iter().map(|(path, change, why)| ignored(path, change, why));
        return Err(within(refused(lines.collect())));
    }
    if opening.remember {
        hub.remember(&opening.declaration).map_err(within)?;
    }
    let current = Mutex::new(Current { version: kept.version, resolved });
    let inner = Arc::new(Inner {
        name: opening.name,
        hub,
        declaration: opening.declaration,
        base,
        layered,
        validate: opening.validate,
        view: opening.view,
        current,
        watchers: Watchers::default(),
        warned: Mutex::default(),
    });
    inner.join();
    Ok(Config { inner })
}

/// The declaration a Rust program's value and its marks make.
fn declared<T: Serialize>(declaration: &T) -> Result<Declaration> {
    let value = marked(declaration).map_err(|error| Error::invalid(format!("the declaration: {error}")))?;
    Declaration::of_marked(&value).map_err(Error::invalid)
}

/// One error that says every problem at once.
fn refused(problems: Vec<String>) -> Error {
    Error::invalid(problems.join("; "))
}

fn ignored(path: &str, change: &Change, why: &str) -> String {
    format!("{path}: the change of version {} is ignored: {why}", change.version)
}

impl<T> Config<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// The settings as they are now: a read of memory, so a program reads
    /// them where it uses them, on every request.
    pub fn get(&self) -> Snapshot<T> {
        self.inner.snapshot()
    }

    /// Changes live settings: the ones `change` changes are kept in kv.db,
    /// go over every layer until they are reset, and reach every handle of
    /// the config before this returns. All of them are kept or none is, and
    /// another change made meanwhile stays: `change` runs again on the
    /// settings it made, so `|s| s.rps += 1` racing another counts both.
    pub fn update(&self, change: impl FnMut(&mut T)) -> Result<()> {
        self.change(None, change)
    }

    /// A change made only while the config's changes are still at `version`,
    /// as a snapshot shows it; another change made since is `conflict`.
    pub fn if_version(&self, version: i64) -> IfVersion<'_, T> {
        IfVersion { config: self, version }
    }

    /// Drops the change kept of the setting at `path`, or of every setting
    /// of the group at it, so that the layers give them again.
    pub fn reset(&self, path: &str) -> Result<()> {
        if self.resets(path)? {
            let within = |error: Error| error.within(format!("config {}: reset {path}", self.inner.name));
            self.inner.hub.reset(path).map_err(within)?;
        }
        Ok(())
    }

    /// Whether a change is kept at `path` or under it, which a reset drops;
    /// `invalid` when `path` names no setting and no change is kept there.
    pub(crate) fn resets(&self, path: &str) -> Result<bool> {
        let kept = self.inner.hub.kept();
        let under = format!("{path}.");
        let has_kept = kept.changes.keys().any(|changed| changed == path || changed.starts_with(&under));
        if path.is_empty() || (!has_kept && self.inner.declaration.node(path).is_none()) {
            let unknown = Error::invalid("no such setting, and no change of it is kept");
            return Err(unknown.within(format!("config {}: reset {path}", self.inner.name)));
        }
        Ok(has_kept)
    }

    /// Drops every change kept of the config.
    pub fn reset_all(&self) -> Result<()> {
        self.inner.hub.reset("").map(drop).map_err(|error| error.within(format!("config {}", self.inner.name)))
    }

    /// Calls `watcher` with the settings now, without a previous value, then
    /// with each change and the value before it. It runs on the thread that
    /// made the change, after its commit, and must not block; one that falls
    /// behind gets the newest.
    pub fn watch(&self, watcher: impl Fn(&Snapshot<T>, Option<&Snapshot<T>>) + Send + Sync + 'static) -> Watching {
        let id = self.inner.watchers.add(Arc::new(watcher));
        self.inner.deliver();
        let inner = Arc::downgrade(&self.inner);
        let stop = move || {
            if let Some(inner) = inner.upgrade() {
                inner.watchers.remove(id);
            }
        };
        Watching { stop: Some(Box::new(stop)) }
    }

    /// Every setting with its value, the layer that gave it and the values it
    /// went over, then what was read and not used.
    pub fn explain(&self) -> Explained {
        self.inner.explain()
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Changes the live settings `changes` names, a partial value in the
    /// declaration's shape, while the config is still at `version`: what a
    /// program's SDK sends once its own `validate` took the value they make.
    #[cfg(any(test, feature = "wire"))]
    pub(crate) fn update_data(&self, version: i64, changes: &Value) -> Result<()> {
        let within = |error: Error| error.within(format!("config {}", self.inner.name));
        let mut leaves = Vec::new();
        self.inner.flatten(&self.inner.declaration.root, changes, "", &mut leaves).map_err(within)?;
        if leaves.is_empty() {
            return Ok(());
        }
        let (_, tree) = self.inner.state();
        self.inner.candidate(&tree, &leaves).map_err(within)?;
        self.inner.hub.change(version, leaves).map(drop).map_err(within)
    }

    fn change(&self, if_version: Option<i64>, mut change: impl FnMut(&mut T)) -> Result<()> {
        let within = |error: Error| error.within(format!("config {}", self.inner.name));
        for _ in 0..ATTEMPTS {
            let Some((expected, leaves)) = self.proposal(if_version, &mut change)? else { return Ok(()) };
            match self.inner.hub.change(expected, leaves) {
                Err(error) if error.kind() == ErrorKind::Conflict && if_version.is_none() => {}
                done => return done.map(drop).map_err(within),
            }
        }
        Err(self.raced())
    }

    /// One try of a change: the live settings `change` changes, each checked
    /// against every handle's declaration, the value they make checked by the
    /// program's `validate`, and the version the change is made on; none when
    /// `change` changes nothing.
    pub(crate) fn proposal(&self, if_version: Option<i64>, change: impl FnOnce(&mut T)) -> Result<Option<Proposal>> {
        let within = |error: Error| error.within(format!("config {}", self.inner.name));
        let (version, tree) = self.inner.state();
        let leaves = self.inner.leaves(change).map_err(within)?;
        if leaves.is_empty() {
            return Ok(None);
        }
        self.inner.candidate(&tree, &leaves).map_err(within)?;
        Ok(Some((if_version.unwrap_or(version), leaves)))
    }

    /// What a change answers once others came first each of its tries.
    pub(crate) fn raced(&self) -> Error {
        let raced = Error::new(ErrorKind::Conflict, "other changes came first each time it was tried");
        raced.within(format!("config {}", self.inner.name))
    }
}

/// A change's settings with their values, and the version it is made on.
pub(crate) type Proposal = (i64, Vec<(String, Value)>);

/// What the async API's handle of a config awaits: each try of a change made
/// on the polling thread by [`Config::proposal`], its commit submitted to
/// kv.db's writer, and what it wrote handed back to be published there.
#[cfg(feature = "async")]
impl<T> Config<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// The tries a change makes while others race it, as `update`'s.
    pub(crate) const ATTEMPTS: usize = ATTEMPTS;

    /// Submits a try to kv.db's writer. `done` gets what it wrote, on the
    /// thread that commits it, for the caller to publish; dropped unpublished,
    /// it is published on `elsewhere`.
    pub(crate) fn submit_change(
        &self,
        proposal: Proposal,
        elsewhere: crate::polled::Lead,
        done: impl FnOnce(Result<Unpublished>) + Send + 'static,
    ) {
        let shown = format!("config {}", self.inner.name);
        let named = move |written: Result<_>| done(written.map_err(|error| error.within(shown)));
        self.inner.hub.change_then(proposal, elsewhere, named);
    }

    /// Submits a reset of `path`, every change for an empty one, as
    /// [`Config::submit_change`] submits a change.
    pub(crate) fn submit_reset(
        &self,
        path: &str,
        elsewhere: crate::polled::Lead,
        done: impl FnOnce(Result<Unpublished>) + Send + 'static,
    ) {
        let shown = match path {
            "" => format!("config {}", self.inner.name),
            path => format!("config {}: reset {path}", self.inner.name),
        };
        let named = move |written: Result<_>| done(written.map_err(|error| error.within(shown)));
        self.inner.hub.reset_then(path, elsewhere, named);
    }

    /// Calls `hook` once, when the store closes; false, and nothing called,
    /// once it has closed.
    pub(crate) fn on_close(&self, hook: Hook) -> bool {
        self.inner.hub.on_close(hook)
    }

    /// The store the config was opened in, while it is open.
    pub(crate) fn store(&self) -> Result<Store> {
        self.inner.hub.store()
    }
}

impl<T> IfVersion<'_, T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    pub fn update(self, change: impl FnMut(&mut T)) -> Result<()> {
        self.config.change(Some(self.version), change)
    }
}

impl<T> Inner<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn snapshot(&self) -> Snapshot<T> {
        let current = lock(&self.current);
        Snapshot { value: Arc::clone(&current.resolved.value), version: current.version }
    }

    fn state(&self) -> (i64, Value) {
        let current = lock(&self.current);
        (current.version, current.resolved.tree.clone())
    }

    /// Follows the hub, then takes a change it published before this handle
    /// joined; and says in the log what the layers read and did not use.
    fn join(self: &Arc<Self>) {
        let me: Weak<dyn Follower> = Arc::downgrade(self) as Weak<Self>;
        self.hub.join(me);
        self.follow(&self.hub.kept());
        for file in &self.base.absent {
            tracing::info!(target: "tinystore", "config {}: {file}: absent", self.name);
        }
        for unused in &self.base.unused {
            tracing::warn!(target: "tinystore", "config {}: {unused}", self.name);
        }
        self.warn_ignored();
        self.warn_differences();
    }

    /// The live settings `change` changes, each with its new value, checked
    /// against this declaration and every other one open on the config.
    fn leaves(&self, change: impl FnOnce(&mut T)) -> Result<Vec<(String, Value)>> {
        let before = self.snapshot();
        let mut after = (*before.value).clone();
        change(&mut after);
        let (before_tree, after_tree) = (plain(&*before.value)?, plain(&after)?);
        let mut leaves = Vec::new();
        for (path, setting) in self.declaration.settings() {
            let (was, is) = (at(&before_tree, &path), at(&after_tree, &path));
            if was == is {
                continue;
            }
            let value = is.cloned().unwrap_or(Value::Null);
            leaves.push((path.clone(), self.checked(&path, setting, &value)?));
        }
        Ok(leaves)
    }

    fn checked(&self, path: &str, setting: &Setting, value: &Value) -> Result<Value> {
        if setting.secret {
            return Err(Error::invalid(format!("{path} is a secret, which only a layer gives")));
        }
        if !setting.live {
            return Err(Error::invalid(format!("{path} is not live")));
        }
        let size = value.to_string().len();
        if size > MOST_VALUE {
            return Err(Error::limit(format!("{path}: {size} bytes, past the {MOST_VALUE} a change holds")));
        }
        let value = rules::check(setting, value).map_err(|why| Error::invalid(format!("{path}: {why}")))?;
        for follower in self.hub.followers() {
            let Some(other) = follower.declaration().setting(path) else { continue };
            let refused =
                |why: String| Error::invalid(format!("{path}: {why}, as another handle of the config declares it"));
            if other.secret || !other.live {
                return Err(refused(if other.secret { "a secret".to_owned() } else { "not live".to_owned() }));
            }
            rules::check(other, &value).map_err(refused)?;
        }
        Ok(value)
    }

    /// Whether `leaves` over `tree` make a value of the program's type that
    /// its `validate` takes.
    fn candidate(&self, tree: &Value, leaves: &[(String, Value)]) -> Result<()> {
        let mut next = tree.clone();
        for (path, value) in leaves {
            set(&mut next, path, value.clone());
        }
        let built = self.making().built(&next).map_err(Error::invalid)?;
        match &self.validate {
            Some(validate) => validate(&built).map_err(Error::invalid),
            None => Ok(()),
        }
    }

    fn making(&self) -> Making<'_, T> {
        Making { declaration: &self.declaration, validate: self.validate.as_ref(), view: self.view }
    }

    /// The settings a partial value names, each with its value checked.
    #[cfg(any(test, feature = "wire"))]
    fn flatten(&self, group: &Group, changes: &Value, path: &str, leaves: &mut Vec<(String, Value)>) -> Result<()> {
        let Value::Object(map) = changes else {
            let at = if path.is_empty() { "a change" } else { path };
            return Err(Error::invalid(format!("{at}: {} is not a group of settings", shown(changes))));
        };
        for (key, value) in map {
            let at = joined(path, key);
            match group.get(key) {
                None => return Err(Error::invalid(format!("{at}: no such setting"))),
                Some(Node::Group(inner)) => self.flatten(inner, value, &at, leaves)?,
                Some(Node::Setting(setting)) => {
                    let checked = self.checked(&at, setting, value)?;
                    leaves.push((at, checked));
                }
            }
        }
        Ok(())
    }

    fn explain(&self) -> Explained {
        let current = lock(&self.current);
        let resolved = &current.resolved;
        let mut settings = Vec::new();
        for (path, setting) in self.declaration.settings() {
            let mut stack = self.base.stacks.get(&path).cloned().unwrap_or_default();
            if let Some(change) = resolved.applied.get(&path) {
                let from = Origin::Change { version: change.version, at: from_unix_millis(change.at) };
                stack.push(Stacked { value: change.value.clone(), from });
            }
            if let Some(line) = line(path, setting, stack) {
                settings.push(line);
            }
        }
        let mut notes: Vec<String> = self.base.absent.iter().map(|file| format!("{file}: absent")).collect();
        notes.extend(self.base.unused.iter().map(ToString::to_string));
        notes.extend(resolved.ignored.iter().map(|(path, change, why)| ignored(path, change, why)));
        notes.extend(
            resolved.foreign.iter().map(|path| format!("{path}: a change of a setting this program does not declare")),
        );
        Explained { settings, notes }
    }

    fn deliver(&self) {
        self.watchers.deliver(|| self.snapshot());
    }

    /// Says each ignored change in the log, once.
    fn warn_ignored(&self) {
        let current = lock(&self.current);
        let mut warned = lock(&self.warned);
        for (path, change, why) in &current.resolved.ignored {
            if warned.insert((path.clone(), change.version)) {
                let line = ignored(path, change, why);
                tracing::warn!(target: "tinystore", "config {}: {line}", self.name);
            }
        }
    }

    /// Says in the log each setting another handle of the config declares
    /// another kind of, which no change can then fit.
    fn warn_differences(&self) {
        for follower in self.hub.followers() {
            for (path, setting) in self.declaration.settings() {
                match follower.declaration().setting(&path) {
                    Some(other) if other.kind != setting.kind => tracing::warn!(
                        target: "tinystore",
                        "config {}: {path} is {} here and {} to another handle of the config",
                        self.name,
                        described(&setting.kind),
                        described(&other.kind),
                    ),
                    _ => {}
                }
            }
        }
    }
}

impl<T> Follower for Inner<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// Makes the value of a later version, outside the lock, since the
    /// program's `validate` runs, then takes it unless a later one came.
    fn follow(&self, kept: &Arc<Kept>) {
        if kept.version <= lock(&self.current).version {
            return;
        }
        let resolved = self.making().resolve((&self.layered.0, &self.layered.1), kept);
        {
            let mut current = lock(&self.current);
            if kept.version <= current.version {
                return;
            }
            *current = Current { version: kept.version, resolved };
        }
        self.warn_ignored();
        self.deliver();
    }
}

/// A setting's line of `explain`, from its values oldest first: each shown
/// as `explain` shows a value, and its `from` and `over` that data as text.
fn line(path: String, setting: &Setting, stack: Vec<Stacked>) -> Option<Line> {
    let shown = |value: &Value| {
        if setting.secret { "***".to_owned() } else { shown_value(value, setting.kind == Kind::Duration) }
    };
    let given: Vec<Given> =
        stack.into_iter().rev().map(|stacked| Given { value: shown(&stacked.value), from: stacked.from }).collect();
    let (own, over) = given.split_first()?;
    let over = over.iter().map(|given| format!("{} from {}", given.value, given.from)).collect();
    Some(Line { path, value: own.value.clone(), from: own.from.to_string(), over, given })
}

/// A value as the config keeps it: marks taken off, spans in milliseconds.
fn plain<T: Serialize>(value: &T) -> Result<Value> {
    marked(value).map(|value| unmarked(&value)).map_err(|error| Error::invalid(error.to_string()))
}

/// The value at a path of keys joined by dots.
fn at<'a>(tree: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(tree, |node, key| node.get(key))
}

/// A config's watchers, and the one delivery that calls them in order.
struct Watchers<T> {
    list: Mutex<Vec<Watcher<T>>>,
    delivering: Mutex<()>,
    /// A delivery was asked for that the one running has not made.
    pending: AtomicBool,
    next: AtomicU64,
}

struct Watcher<T> {
    id: u64,
    call: Arc<WatchFn<T>>,
    /// The value it was last called with; none before its first call.
    seen: Option<Snapshot<T>>,
}

impl<T> Default for Watchers<T> {
    fn default() -> Self {
        Watchers {
            list: Mutex::default(),
            delivering: Mutex::default(),
            pending: AtomicBool::new(false),
            next: AtomicU64::new(0),
        }
    }
}

impl<T> Watchers<T> {
    fn add(&self, call: Arc<WatchFn<T>>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.list).push(Watcher { id, call, seen: None });
        id
    }

    fn remove(&self, id: u64) {
        lock(&self.list).retain(|watcher| watcher.id != id);
    }

    /// Calls each watcher with the value as it is now, which it has not seen,
    /// one delivery at a time so that each sees the versions in order. One
    /// asked for while another runs, from a watcher's own change too, is
    /// made by the one running rather than waited for.
    fn deliver(&self, now: impl Fn() -> Snapshot<T>) {
        self.pending.store(true, Ordering::SeqCst);
        while self.pending.load(Ordering::SeqCst) {
            let _delivering = match self.delivering.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return,
            };
            while self.pending.swap(false, Ordering::SeqCst) {
                self.round(&now());
            }
        }
    }

    fn round(&self, now: &Snapshot<T>) {
        let due: Vec<Due<T>> = lock(&self.list)
            .iter()
            .filter(|watcher| watcher.seen.as_ref().is_none_or(|seen| seen.version < now.version))
            .map(|watcher| (watcher.id, Arc::clone(&watcher.call), watcher.seen.clone()))
            .collect();
        for (id, call, seen) in due {
            call(now, seen.as_ref());
            if let Some(watcher) = lock(&self.list).iter_mut().find(|watcher| watcher.id == id) {
                watcher.seen = Some(now.clone());
            }
        }
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

impl<T> Clone for Config<T> {
    fn clone(&self) -> Self {
        Config { inner: Arc::clone(&self.inner) }
    }
}

impl<T> fmt::Debug for Config<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let version = lock(&self.inner.current).version;
        f.debug_struct("Config").field("name", &self.inner.name).field("version", &version).finish()
    }
}

impl<T> fmt::Debug for IfVersion<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IfVersion").field("config", &self.config.inner.name).field("version", &self.version).finish()
    }
}

impl fmt::Debug for Watching {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watching").field("stopped", &self.stop.is_none()).finish()
    }
}
