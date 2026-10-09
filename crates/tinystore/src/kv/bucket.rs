use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::cells::{self, Cell, MAX_VALUE};
use super::engine::{Kv, next_version};
use super::path::{Branch, Key};
use super::value::{self, Raw, Value};
use crate::{Error, ErrorKind, Result, Store, unix_millis};

const ROLE: &str = "values";

/// Keys a clear deletes in one transaction; a larger branch is marked cleared
/// and maintenance deletes it a batch at a time.
const CLEAR_BOUND: usize = 10_000;

/// Keys a page holds at most.
pub const PAGE_KEYS: usize = 1000;

/// A bucket being opened: its name, its type, how its keys expire.
#[must_use = "a bucket opens with open()"]
pub struct BucketBuilder<V> {
    store: Store,
    name: String,
    expiry: Expiry,
    _value: PhantomData<fn() -> V>,
}

/// How a bucket's keys expire when a write does not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expiry {
    Never,
    /// A span after the key was written.
    Ttl(Duration),
    /// A span after the key was last read or written.
    Idle(Duration),
}

impl Store {
    /// A bucket of values of type `V` by key, in the store's kv.db; the engine
    /// opens with the first bucket.
    pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V> {
        BucketBuilder { store: self.clone(), name: name.to_owned(), expiry: Expiry::Never, _value: PhantomData }
    }
}

impl<V: Value> BucketBuilder<V> {
    /// Keys expire this long after they are written, unless a write says
    /// otherwise.
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.expiry = Expiry::Ttl(ttl);
        self
    }

    /// Keys expire this long after they were last read or written.
    pub fn idle(mut self, idle: Duration) -> Self {
        self.expiry = Expiry::Idle(idle);
        self
    }

    pub fn open(self) -> Result<Bucket<V>> {
        let what = || format!("kv bucket {}", self.name);
        check_name(&self.name).map_err(|error| error.within(what()))?;
        check_expiry(self.expiry).map_err(|error| error.within(what()))?;
        let kv = Kv::of(&self.store).map_err(|error| error.within(what()))?;
        let id = kv.bucket(&self.name, ROLE).map_err(|error| error.within(what()))?;
        Ok(Bucket {
            kv,
            id,
            name: self.name.into(),
            branch: Ok(Branch::default()),
            expiry: self.expiry,
            _value: PhantomData,
        })
    }
}

/// Values of one type by key. A handle is cheap to clone and to keep; `of`
/// gives the same bucket seen from a branch.
pub struct Bucket<V> {
    kv: Arc<Kv>,
    id: i64,
    name: Arc<str>,
    /// An invalid owner given to `of` fails the branch's calls, not `of`.
    branch: std::result::Result<Branch, String>,
    expiry: Expiry,
    _value: PhantomData<fn() -> V>,
}

/// A key's value with what a conditional write needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry<V> {
    pub key: String,
    pub value: V,
    pub version: Version,
    pub expires_at: Option<SystemTime>,
}

/// The revision of kv.db that wrote a value. It never repeats, across deletes,
/// expiry and restarts, and it compares only for equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Version(i64);

impl Version {
    pub(crate) fn new(revision: i64) -> Version {
        Version(revision)
    }

    pub(crate) fn revision(self) -> i64 {
        self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for Version {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        text.parse().map(Version).map_err(|_| Error::invalid(format!("{text:?} is not a version")))
    }
}

/// One page of a branch's keys, in the byte order of their text.
#[derive(Clone, Debug, PartialEq)]
pub struct Page<V> {
    pub entries: Vec<Entry<V>>,
    /// The key the next page starts after, when there is more.
    pub next: Option<String>,
}

/// What a write may ask beside its key and value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Write {
    expires: Option<Expires>,
    if_version: Option<Version>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expires {
    In(Duration),
    At(SystemTime),
}

/// A write whose key expires `ttl` from now.
pub fn ttl(ttl: Duration) -> Write {
    Write::default().ttl(ttl)
}

/// A write whose key expires at `time`.
pub fn expires_at(time: SystemTime) -> Write {
    Write::default().expires_at(time)
}

/// A write that applies only to a key still at `version`.
pub fn if_version(version: Version) -> Write {
    Write::default().if_version(version)
}

impl Write {
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.expires = Some(Expires::In(ttl));
        self
    }

    pub fn expires_at(mut self, time: SystemTime) -> Self {
        self.expires = Some(Expires::At(time));
        self
    }

    pub fn if_version(mut self, version: Version) -> Self {
        self.if_version = Some(version);
        self
    }

    fn expires(self, now: i64) -> Option<i64> {
        self.expires.map(|expires| match expires {
            Expires::In(span) => now.saturating_add(millis(span)),
            Expires::At(time) => unix_millis(time),
        })
    }
}

/// How a write treats the key it finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Put {
    Always,
    OnlyNew,
}

/// What a write left: whether it wrote, and the version and expiry the key
/// has now, its own when a create found a live key there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) written: bool,
    pub(crate) version: Version,
    pub(crate) expires: Option<i64>,
}

impl<V: Value> Bucket<V> {
    /// Writes `value` under `key`. A key that is there keeps its expiry; a new
    /// one takes the bucket's.
    pub fn set(&self, key: impl Key, value: &V) -> Result<()> {
        self.put(&key.text(), value, Write::default(), Put::Always).map(|_| ())
    }

    /// `set` with an expiry or a version the key must still be at.
    pub fn set_with(&self, key: impl Key, value: &V, write: Write) -> Result<()> {
        self.put(&key.text(), value, write, Put::Always).map(|_| ())
    }

    /// Writes `value` only when `key` is not there, and says whether it did; an
    /// expired key is not there. A key that is there is `false`, never an error.
    pub fn create(&self, key: impl Key, value: &V) -> Result<bool> {
        self.put(&key.text(), value, Write::default(), Put::OnlyNew)
    }

    pub fn create_with(&self, key: impl Key, value: &V, write: Write) -> Result<bool> {
        self.put(&key.text(), value, write, Put::OnlyNew)
    }

    pub fn get(&self, key: impl Key) -> Result<Option<V>> {
        Ok(self.entry(key)?.map(|entry| entry.value))
    }

    /// The value with its version and expiry.
    pub fn entry(&self, key: impl Key) -> Result<Option<Entry<V>>> {
        let key = key.text();
        let found = self.read_cell(&key)?;
        found.map(|cell| self.entry_of(&key, cell)).transpose()
    }

    /// Reads `key` and removes it in one commit: of two callers taking the same
    /// key, one gets the value.
    pub fn take(&self, key: impl Key) -> Result<Option<V>> {
        self.take_with(key, Write::default())
    }

    pub fn take_with(&self, key: impl Key, write: Write) -> Result<Option<V>> {
        let key = key.text();
        let taken = self.remove(&key, write)?;
        taken.map(|cell| self.decode(&key, &cell.raw)).transpose()
    }

    /// One page of this branch's own keys, after `after` when it continues
    /// another; at most `limit` keys, and never more than [`PAGE_KEYS`].
    pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>> {
        let branch = self.branch()?;
        let limit = limit.clamp(1, PAGE_KEYS);
        let (from, past) = branch.own_keys_after(after);
        let now = self.kv.now();
        let id = self.id;
        let rows = self
            .kv
            .file()
            .read(|connection| cells::page(connection, id, (&from, &past), now, limit))
            .map_err(|error| error.within(self.shown_branch()))?;
        let full = rows.len() == limit;
        let mut entries = Vec::with_capacity(rows.len());
        for (path, cell) in rows {
            let key = branch.key_of(&path)?;
            entries.push(self.entry_of(&key, cell)?);
        }
        let next = if full { entries.last().map(|entry| entry.key.clone()) } else { None };
        Ok(Page { entries, next })
    }

    /// Every key of this branch, a page at a time. No snapshot is held between
    /// pages, so a slow loop keeps no reader, and a key written meanwhile may
    /// or may not be met.
    pub fn all(&self) -> All<V> {
        All { bucket: self.clone(), page: Vec::new().into_iter(), next: None, done: false }
    }

    fn put(&self, key: &str, value: &V, write: Write, put: Put) -> Result<bool> {
        let raw = value::encode(value).map_err(|mismatch| {
            Error::invalid(format!("a value it cannot keep: {mismatch}")).within(self.shown_key(key))
        })?;
        Ok(self.put_raw(key, raw, write, put)?.written)
    }

    fn entry_of(&self, key: &str, cell: Cell) -> Result<Entry<V>> {
        Ok(Entry {
            key: key.to_owned(),
            value: self.decode(key, &cell.raw)?,
            version: Version(cell.version),
            expires_at: cell.expires.map(time_of),
        })
    }

    fn decode(&self, key: &str, raw: &Raw) -> Result<V> {
        value::decode(raw).map_err(|mismatch| {
            Error::corrupt(format!("its value does not read as the bucket's type: {mismatch}"))
                .within(self.shown_key(key))
        })
    }
}

/// What a bucket does whatever its type: the calls that make no value, and
/// the row-level ones the wire's handles use.
impl<V> Bucket<V> {
    /// The same bucket seen from the branch `owner` names under this one:
    /// `sessions.of(user_id).clear()` signs a user out everywhere.
    pub fn of(&self, owner: impl Key) -> Bucket<V> {
        let branch = match &self.branch {
            Ok(branch) => branch.of(&owner.text()).map_err(|error| error.to_string()),
            Err(invalid) => Err(invalid.clone()),
        };
        Bucket { branch, ..self.clone() }
    }

    pub fn has(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        if matches!(self.expiry, Expiry::Idle(_)) {
            return Ok(self.read_cell(&key)?.is_some());
        }
        let (id, path, now) = (self.id, self.path(&key)?, self.kv.now());
        self.kv
            .file()
            .read(|connection| cells::has(connection, id, &path, now))
            .map_err(|error| error.within(self.shown_key(&key)))
    }

    /// Removes `key` and says whether a live key was there.
    pub fn delete(&self, key: impl Key) -> Result<bool> {
        self.delete_with(key, Write::default())
    }

    pub fn delete_with(&self, key: impl Key, write: Write) -> Result<bool> {
        Ok(self.remove(&key.text(), write)?.is_some())
    }

    /// Gives a live key a new expiry, `ttl` from now, as Redis's EXPIRE does,
    /// keeping its value and version; says whether the key was there.
    pub fn expire(&self, key: impl Key, ttl: Duration) -> Result<bool> {
        self.expire_with(key, Write::default().ttl(ttl))
    }

    pub fn expire_with(&self, key: impl Key, write: Write) -> Result<bool> {
        let key = key.text();
        let now = self.kv.now();
        let expires = write
            .expires(now)
            .ok_or_else(|| Error::invalid("an expiry needs a ttl or a time").within(self.shown_key(&key)))?;
        let (id, path, if_version) = (self.id, self.path(&key)?, write.if_version);
        self.write(&key, 0, move |tx, _| {
            if live_at(tx, id, &path, now, if_version)?.is_none() {
                return Ok(false);
            }
            cells::set_expires(tx, id, &path, Some(expires))?;
            Ok(true)
        })
    }

    /// Removes every key of this branch and of the branches under it, at once
    /// for every reader, however many there are.
    pub fn clear(&self) -> Result<()> {
        let branch = self.branch()?;
        let (id, prefix, everything) = (self.id, branch.prefix().to_vec(), branch.everything());
        let revision = self.kv.revision();
        let shown = self.shown_branch();
        self.kv
            .file()
            .write(0, move |tx| {
                if cells::delete_under(tx, id, (&everything.0, &everything.1), CLEAR_BOUND)? {
                    return Ok(());
                }
                let cleared = next_version(&revision, tx)?;
                cells::mark_cleared(tx, id, &prefix, cleared)
            })
            .map_err(|error| error.within(shown))
    }

    /// Writes a row's value under `key`, as `put` says, and stamps it.
    pub(crate) fn put_raw(&self, key: &str, raw: Raw, write: Write, put: Put) -> Result<Stamp> {
        if raw.len() > MAX_VALUE {
            let too_large = Error::limit(format!("a value of {} bytes, over {MAX_VALUE}", raw.len()));
            return Err(too_large.within(self.shown_key(key)));
        }
        let now = self.kv.now();
        let (id, path, given, default) = (self.id, self.path(key)?, write.expires(now), self.default_expires(now));
        let if_version = write.if_version;
        self.write(key, raw.len(), move |tx, revision| {
            let current = cells::current(tx, id, &path, now)?;
            let live = current.filter(|current| current.live);
            if let (Put::OnlyNew, Some(live)) = (put, live) {
                return Ok(Stamp { written: false, version: Version(live.version), expires: live.expires });
            }
            check_version(live.map(|live| live.version), if_version)?;
            let expires = given.or(live.map_or(default, |live| live.expires));
            let version = next_version(revision, tx)?;
            cells::put(tx, id, &path, (version, expires), &raw, current.and_then(|current| current.spill))?;
            Ok(Stamp { written: true, version: Version(version), expires })
        })
    }

    /// Removes a live key, at the version asked when one is, and hands back its
    /// cell.
    pub(crate) fn remove(&self, key: &str, write: Write) -> Result<Option<Cell>> {
        let now = self.kv.now();
        let (id, path, if_version) = (self.id, self.path(key)?, write.if_version);
        self.write(key, 0, move |tx, _| {
            let Some(found) = live_at(tx, id, &path, now, if_version)? else {
                return Ok(None);
            };
            let cell = cells::live(tx, id, &path, now)?;
            cells::remove(tx, id, &path, found.spill)?;
            Ok(cell)
        })
    }

    /// Reads a live cell, renewing an idle bucket's key once a thirtieth of its
    /// term has passed since it last was.
    pub(crate) fn read_cell(&self, key: &str) -> Result<Option<Cell>> {
        let (id, path, now) = (self.id, self.path(key)?, self.kv.now());
        let found = self
            .kv
            .file()
            .read(|connection| cells::live(connection, id, &path, now))
            .map_err(|error| error.within(self.shown_key(key)))?;
        match (found, self.expiry) {
            (Some(cell), Expiry::Idle(term)) => self.renew(key, path, cell, now, term).map(Some),
            (found, _) => Ok(found),
        }
    }

    fn renew(&self, key: &str, path: Vec<u8>, mut cell: Cell, now: i64, term: Duration) -> Result<Cell> {
        let (term, Some(expires)) = (millis(term), cell.expires) else {
            return Ok(cell);
        };
        if expires - now > term - term / 30 {
            return Ok(cell);
        }
        let (id, version, renewed) = (self.id, cell.version, now.saturating_add(term));
        self.write(key, 0, move |tx, _| cells::renew(tx, id, &path, (version, expires), renewed))?;
        cell.expires = Some(renewed);
        Ok(cell)
    }

    /// Runs a write on kv.db's writer, naming the key in its errors.
    fn write<T: Send + 'static>(
        &self,
        key: &str,
        bytes: usize,
        work: impl FnOnce(&crate::sqlite::Tx<'_>, &std::sync::atomic::AtomicI64) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let revision = self.kv.revision();
        self.kv.file().write(bytes, move |tx| work(tx, &revision)).map_err(|error| error.within(self.shown_key(key)))
    }

    fn path(&self, key: &str) -> Result<Vec<u8>> {
        self.branch()?.path(key).map_err(|error| error.within(format!("kv bucket {}", self.name)))
    }

    fn branch(&self) -> Result<&Branch> {
        self.branch
            .as_ref()
            .map_err(|invalid| Error::invalid(invalid.clone()).within(format!("kv bucket {}", self.name)))
    }

    fn default_expires(&self, now: i64) -> Option<i64> {
        match self.expiry {
            Expiry::Never => None,
            Expiry::Ttl(span) | Expiry::Idle(span) => Some(now.saturating_add(millis(span))),
        }
    }

    fn shown_key(&self, key: &str) -> String {
        match &self.branch {
            Ok(branch) => format!("kv bucket {}: {}", self.name, branch.shown_key(key)),
            Err(_) => format!("kv bucket {}", self.name),
        }
    }

    fn shown_branch(&self) -> String {
        match &self.branch {
            Ok(branch) => format!("kv bucket {}: {}", self.name, branch.shown()),
            Err(_) => format!("kv bucket {}", self.name),
        }
    }
}

impl Bucket<()> {
    /// Adds `key` to a bucket of keys alone, a set, and says whether it was new:
    /// `seen.add(event_id)` is false for an event already handled.
    pub fn add(&self, key: impl Key) -> Result<bool> {
        self.create(key, &())
    }
}

impl<V> Clone for Bucket<V> {
    fn clone(&self) -> Self {
        Bucket {
            kv: Arc::clone(&self.kv),
            id: self.id,
            name: Arc::clone(&self.name),
            branch: self.branch.clone(),
            expiry: self.expiry,
            _value: PhantomData,
        }
    }
}

impl<V> fmt::Debug for Bucket<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bucket({})", self.name)
    }
}

impl<V> fmt::Debug for BucketBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BucketBuilder({})", self.name)
    }
}

/// Every key of a branch, a page at a time.
pub struct All<V> {
    bucket: Bucket<V>,
    page: std::vec::IntoIter<Entry<V>>,
    next: Option<String>,
    done: bool,
}

impl<V: Value> Iterator for All<V> {
    type Item = Result<Entry<V>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.page.next() {
                return Some(Ok(entry));
            }
            if self.done {
                return None;
            }
            match self.bucket.list(PAGE_KEYS, self.next.as_deref()) {
                Ok(page) => {
                    self.done = page.next.is_none();
                    self.next = page.next;
                    self.page = page.entries.into_iter();
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

impl<V> fmt::Debug for All<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "All({})", self.bucket.name)
    }
}

/// The key's current cell when it is live and, if a version is asked, at that
/// version; a key that is not is a conflict when a version was asked.
fn live_at(
    tx: &crate::sqlite::Tx<'_>,
    id: i64,
    path: &[u8],
    now: i64,
    if_version: Option<Version>,
) -> Result<Option<cells::Current>> {
    let live = cells::current(tx, id, path, now)?.filter(|current| current.live);
    check_version(live.map(|live| live.version), if_version)?;
    Ok(live)
}

fn check_version(found: Option<i64>, wanted: Option<Version>) -> Result<()> {
    match wanted {
        Some(Version(wanted)) if found != Some(wanted) => Err(Error::new(
            ErrorKind::Conflict,
            match found {
                Some(found) => format!("at version {found}, not {wanted}"),
                None => format!("not there at version {wanted}"),
            },
        )),
        _ => Ok(()),
    }
}

fn check_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    let first_fits = bytes.first().is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let rest_fits =
        bytes.iter().all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-');
    if first_fits && rest_fits && bytes.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid("a name is a lowercase letter or digit, then up to 63 of them, '_' and '-'"))
}

fn check_expiry(expiry: Expiry) -> Result<()> {
    match expiry {
        Expiry::Ttl(span) | Expiry::Idle(span) if span.is_zero() => Err(Error::invalid("an expiry of zero")),
        _ => Ok(()),
    }
}

fn millis(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).unwrap_or(i64::MAX)
}

fn time_of(millis: i64) -> SystemTime {
    match u64::try_from(millis) {
        Ok(after) => UNIX_EPOCH + Duration::from_millis(after),
        Err(_) => UNIX_EPOCH - Duration::from_millis(millis.unsigned_abs()),
    }
}

#[cfg(test)]
#[path = "bucket_tests.rs"]
mod tests;
