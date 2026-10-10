use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use super::cells::{self, Cell, MAX_VALUE};
use super::encrypted::Encrypted;
use super::engine::next_version;
use super::key_call::KeyCall;
use super::path::Key;
use super::renewals::Renewal;
use super::scope::{Kind, Scope};
use super::value::{self, Raw, Value};
use crate::clock::{from_unix_millis, millis};
use crate::engine::{Home, SharedFile};
use crate::sqlite::Tx;
use crate::{Error, ErrorKind, Result, Store, unix_millis};

/// Keys a clear deletes in one transaction; a larger branch is marked cleared
/// and maintenance deletes it a batch at a time.
const CLEAR_BOUND: usize = 10_000;

/// Keys a page holds at most.
pub const PAGE_KEYS: usize = 1000;

/// An idle key is renewed once a thirtieth of its term has passed since it
/// last was, and before its read returns when it has less than a minute left.
const REFRESHES: i64 = 30;
const RENEW_AT_ONCE: i64 = 60_000;

/// A bucket being opened: its name, its type, how its keys expire, whether
/// its values are encrypted.
#[must_use = "a bucket opens with open()"]
pub struct BucketBuilder<V> {
    home: Home,
    name: String,
    expiry: Expiry,
    encrypted: bool,
    _value: PhantomData<fn() -> V>,
}

/// How a bucket's keys expire when a write does not say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expiry {
    Never,
    /// A span after the key was written.
    Ttl(Duration),
    /// A span after the key was last read or written.
    Idle(Duration),
}

impl Store {
    /// A bucket of values of type `V` by key, in the store's kv.db; the engine
    /// opens with the first handle.
    pub fn bucket<V: Value>(&self, name: &str) -> BucketBuilder<V> {
        BucketBuilder::new(Home::Store(self.clone()), name)
    }
}

impl<V: Value> BucketBuilder<V> {
    fn new(home: Home, name: &str) -> Self {
        BucketBuilder { home, name: name.to_owned(), expiry: Expiry::Never, encrypted: false, _value: PhantomData }
    }

    /// A bucket kept in a file another engine lends, a database's.
    pub(crate) fn in_file(shared: Arc<SharedFile>, name: &str) -> Self {
        BucketBuilder::new(Home::Shared(shared), name)
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

    /// Keeps every value sealed with the store's encryption key, as a password
    /// or a token is kept: the file holds what nobody reads without the key,
    /// and a value copied under another key does not open. A key is not
    /// sealed, so a secret is a value and never a key. A name keeps whether
    /// it is encrypted: it opens the same way every time.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let passwords = store.bucket::<String>("source-passwords").encrypted().open()?;
    /// passwords.set("source-42", &"hunter2".to_owned())?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn encrypted(mut self) -> Self {
        self.encrypted = true;
        self
    }

    pub fn open(self) -> Result<Bucket<V>> {
        check_expiry(self.expiry).map_err(|error| error.within(format!("kv bucket {}", self.name)))?;
        let kind = if self.encrypted { Kind::Encrypted } else { Kind::Values };
        let scope = Scope::open(&self.home, &self.name, kind)?;
        let encrypted = self.encrypted.then(|| Encrypted::of(&scope)).transpose()?;
        Ok(Bucket { encrypted, ..Bucket::new(scope, self.expiry) })
    }
}

/// Values of one type by key. A handle is cheap to clone and to keep; `of`
/// gives the same bucket seen from a branch.
pub struct Bucket<V> {
    pub(crate) scope: Scope,
    expiry: Expiry,
    /// What seals its rows, when the bucket is encrypted.
    encrypted: Option<Encrypted>,
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

/// What a call on one key asks beside its key and value: a new expiry, and a
/// version the key must still be at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteOptions {
    pub(crate) expires: Option<Expires>,
    pub(crate) if_version: Option<Version>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expires {
    In(Duration),
    At(SystemTime),
}

impl WriteOptions {
    pub(crate) fn ttl(mut self, ttl: Duration) -> Self {
        self.expires = Some(Expires::In(ttl));
        self
    }

    pub(crate) fn expires_at(mut self, time: SystemTime) -> Self {
        self.expires = Some(Expires::At(time));
        self
    }

    pub(crate) fn if_version(mut self, version: Version) -> Self {
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

/// A write as a closure the group commit or a transaction runs.
pub(crate) trait Work<T>: FnOnce(&Tx<'_>, &AtomicI64) -> Result<T> + Send + 'static {}

impl<T, F: FnOnce(&Tx<'_>, &AtomicI64) -> Result<T> + Send + 'static> Work<T> for F {}

impl<V: Value> Bucket<V> {
    /// Writes `value` under `key`. A key that is there keeps its ttl; a new one
    /// takes the bucket's, and an idle key lives its span from the write.
    pub fn set(&self, key: impl Key, value: &V) -> Result<()> {
        self.put(&key.text(), value, WriteOptions::default(), Put::Always).map(|_| ())
    }

    /// Writes `value` only when `key` is not there, and says whether it did; an
    /// expired key is not there. A key that is there is `false`, never an error.
    pub fn create(&self, key: impl Key, value: &V) -> Result<bool> {
        self.put(&key.text(), value, WriteOptions::default(), Put::OnlyNew)
    }

    /// A call on `key` with what it asks beside its value, a new expiry or a
    /// version the key must still be at, run by its last step:
    /// `sessions.key(&token).if_version(seen).set(&next)`.
    pub fn key(&self, key: impl Key) -> KeyCall<'_, V> {
        KeyCall::new(self, None, key.text().into_owned())
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
    /// key, one gets the value. A value that no longer reads as the bucket's
    /// type is `Corrupt`, and stays.
    pub fn take(&self, key: impl Key) -> Result<Option<V>> {
        let key = key.text();
        let work = self.take_work(&key, WriteOptions::default())?;
        self.scope.write(&key, 0, work)
    }

    /// One page of this branch's own keys, after `after` when it continues
    /// another; at most `limit` keys, and never more than [`PAGE_KEYS`].
    pub fn list(&self, limit: usize, after: Option<&str>) -> Result<Page<V>> {
        let limit = limit.clamp(1, PAGE_KEYS);
        let rows = self
            .scope
            .kv
            .file()
            .read(|connection| self.page_rows(connection, limit, after))
            .map_err(|error| error.within(self.scope.shown_branch()))?;
        self.page_of(rows, limit)
    }

    /// Every key of this branch, a page at a time. No snapshot is held between
    /// pages, so a slow loop keeps no reader, and a key written meanwhile may
    /// or may not be met.
    pub fn all(&self) -> All<V> {
        All { bucket: self.clone(), page: Vec::new().into_iter(), next: None, done: false }
    }

    fn put(&self, key: &str, value: &V, options: WriteOptions, put: Put) -> Result<bool> {
        let (bytes, work) = self.put_work(key, self.encode(key, value)?, options, put)?;
        Ok(self.scope.write(key, bytes, work)?.written)
    }

    pub(crate) fn encode(&self, key: &str, value: &V) -> Result<Raw> {
        value::encode(value).map_err(|mismatch| {
            Error::invalid(format!("a value it cannot keep: {mismatch}")).within(self.scope.shown_key(key))
        })
    }

    pub(crate) fn take_work(&self, key: &str, options: WriteOptions) -> Result<impl Work<Option<V>> + use<V>> {
        let take = self.take_cell_work(key, options)?;
        Ok(move |tx: &Tx<'_>, revision: &AtomicI64| {
            let cell = take(tx, revision)?;
            cell.map(|cell| decode::<V>(&cell.raw)).transpose()
        })
    }

    pub(crate) fn entry_of(&self, key: &str, cell: Cell) -> Result<Entry<V>> {
        Ok(Entry {
            key: key.to_owned(),
            value: decode(&cell.raw).map_err(|error| error.within(self.scope.shown_key(key)))?,
            version: Version(cell.version),
            expires_at: cell.expires.map(from_unix_millis),
        })
    }

    /// Entries of the rows a page read, refused when one is not this branch's.
    pub(crate) fn page_of(&self, rows: Vec<(Vec<u8>, Cell)>, limit: usize) -> Result<Page<V>> {
        let branch = self.scope.branch()?;
        let full = rows.len() == limit;
        let mut entries = Vec::with_capacity(rows.len());
        for (path, cell) in rows {
            let key = branch.key_of(&path)?;
            entries.push(self.entry_of(&key, cell)?);
        }
        let next = if full { entries.last().map(|entry| entry.key.clone()) } else { None };
        Ok(Page { entries, next })
    }
}

/// What a bucket does whatever its type: the calls that make no value, and
/// the row-level ones the wire's handles use.
impl<V> Bucket<V> {
    pub(crate) fn new(scope: Scope, expiry: Expiry) -> Bucket<V> {
        Bucket { scope, expiry, encrypted: None, _value: PhantomData }
    }

    /// The same bucket seen from the branch `owner` names under this one:
    /// `sessions.under(user_id).clear()` signs a user out everywhere.
    pub fn under(&self, owner: impl Key) -> Bucket<V> {
        Bucket { encrypted: self.encrypted.clone(), ..Bucket::new(self.scope.under(owner), self.expiry) }
    }

    pub fn has(&self, key: impl Key) -> Result<bool> {
        let key = key.text();
        if matches!(self.expiry, Expiry::Idle(_)) {
            return Ok(self.read_cell(&key)?.is_some());
        }
        let (id, path, now) = (self.scope.id, self.scope.path(&key)?, self.scope.now());
        self.scope.read(&key, |connection| cells::has(connection, id, &path, now))
    }

    /// Removes `key` and says whether a live key was there.
    pub fn delete(&self, key: impl Key) -> Result<bool> {
        self.remove(&key.text(), WriteOptions::default())
    }

    /// Gives a live key a new expiry, `ttl` from now, as Redis's EXPIRE does,
    /// keeping its value and version; says whether the key was there.
    pub fn expire(&self, key: impl Key, ttl: Duration) -> Result<bool> {
        self.expire_as(&key.text(), WriteOptions::default().ttl(ttl))
    }

    /// Gives a live key the expiry `options` names, at the version they name.
    pub(crate) fn expire_as(&self, key: &str, options: WriteOptions) -> Result<bool> {
        let work = self.expire_work(key, options)?;
        self.scope.write(key, 0, work)
    }

    /// Removes every key of this branch and of the branches under it, at once
    /// for every reader, however many there are.
    pub fn clear(&self) -> Result<()> {
        let work = clear_work(&self.scope)?;
        let revision = self.scope.kv.revision();
        let shown = self.scope.shown_branch();
        self.scope.kv.file().write(0, move |tx| work(tx, &revision)).map_err(|error| error.within(shown))
    }

    /// `clear` without waiting for its commit.
    pub(crate) fn clear_then(&self, done: impl FnOnce(Result<()>) + Send + 'static) {
        let work = match clear_work(&self.scope) {
            Ok(work) => work,
            Err(error) => return done(Err(error)),
        };
        let (revision, shown) = (self.scope.kv.revision(), self.scope.shown_branch());
        let answered = move |cleared: Result<()>| done(cleared.map_err(|error| error.within(shown)));
        self.scope.kv.file().submit(0, move |tx| work(tx, &revision), answered);
    }

    /// Writes a row's value under `key`, as `put` says, and stamps it.
    pub(crate) fn put_raw(&self, key: &str, raw: Raw, options: WriteOptions, put: Put) -> Result<Stamp> {
        let (bytes, work) = self.put_work(key, raw, options, put)?;
        self.scope.write(key, bytes, work)
    }

    /// `put_raw` without waiting: `done` gets the stamp once the commit ends.
    pub(crate) fn put_raw_then(
        &self,
        key: &str,
        (raw, options, put): (Raw, WriteOptions, Put),
        done: impl FnOnce(Result<Stamp>) + Send + 'static,
    ) {
        match self.put_work(key, raw, options, put) {
            Ok((bytes, work)) => self.scope.submit(key, bytes, work, done),
            Err(error) => done(Err(error)),
        }
    }

    /// The write a put commits, and the bytes it adds to its commit.
    pub(crate) fn put_work(
        &self,
        key: &str,
        raw: Raw,
        options: WriteOptions,
        put: Put,
    ) -> Result<(usize, impl Work<Stamp> + use<V>)> {
        if raw.len() > MAX_VALUE {
            let too_large = Error::limit(format!("a value of {} bytes, over {MAX_VALUE}", raw.len()));
            return Err(too_large.within(self.scope.shown_key(key)));
        }
        let now = self.scope.now();
        let (id, path, given, default) =
            (self.scope.id, self.scope.path(key)?, options.expires(now), self.default_expires(now));
        let raw = self.kept(&path, raw).map_err(|error| error.within(self.scope.shown_key(key)))?;
        let (if_version, bytes) = (options.if_version, raw.len());
        // a write is a use of an idle key, which lives on from it; a key with a ttl keeps its own
        let keeps_its_expiry = !matches!(self.expiry, Expiry::Idle(_));
        let encrypted = self.encrypted.clone();
        let work = move |tx: &Tx<'_>, revision: &AtomicI64| {
            if let Some(encrypted) = &encrypted {
                encrypted.claim(tx)?;
            }
            let current = cells::current(tx, id, &path, now)?;
            let live = current.filter(|current| current.live);
            if let (Put::OnlyNew, Some(live)) = (put, live) {
                return Ok(Stamp { written: false, version: Version(live.version), expires: live.expires });
            }
            check_version(live.map(|live| live.version), if_version)?;
            let expires = given.or(match live {
                Some(live) if keeps_its_expiry => live.expires,
                _ => default,
            });
            let version = next_version(revision, tx)?;
            cells::put(tx, id, &path, (version, expires), &raw, current.and_then(|current| current.spill))?;
            Ok(Stamp { written: true, version: Version(version), expires })
        };
        Ok((bytes, work))
    }

    /// Removes a live key, at the version asked when one is, and says whether
    /// one was there.
    pub(crate) fn remove(&self, key: &str, options: WriteOptions) -> Result<bool> {
        let work = self.remove_work(key, options)?;
        self.scope.write(key, 0, work)
    }

    /// `remove` without waiting: `done` hears once the commit ends.
    pub(crate) fn remove_then(
        &self,
        key: &str,
        options: WriteOptions,
        done: impl FnOnce(Result<bool>) + Send + 'static,
    ) {
        match self.remove_work(key, options) {
            Ok(work) => self.scope.submit(key, 0, work, done),
            Err(error) => done(Err(error)),
        }
    }

    /// The delete of a key. It reads no value, so a key whose value no longer
    /// opens, sealed with a key the store has lost, can still be deleted.
    pub(crate) fn remove_work(&self, key: &str, options: WriteOptions) -> Result<impl Work<bool> + use<V>> {
        let now = self.scope.now();
        let (id, path, if_version) = (self.scope.id, self.scope.path(key)?, options.if_version);
        Ok(move |tx: &Tx<'_>, _: &AtomicI64| {
            let Some(found) = live_at(tx, id, &path, now, if_version)? else {
                return Ok(false);
            };
            cells::remove(tx, id, &path, found.spill)?;
            Ok(true)
        })
    }

    /// `take_cell_work` without waiting: `done` gets the cell once the commit
    /// ends.
    pub(crate) fn take_cell_then(
        &self,
        key: &str,
        options: WriteOptions,
        done: impl FnOnce(Result<Option<Cell>>) + Send + 'static,
    ) {
        match self.take_cell_work(key, options) {
            Ok(work) => self.scope.submit(key, 0, work, done),
            Err(error) => done(Err(error)),
        }
    }

    /// The take of a key: its delete, which hands back the cell it removed. A
    /// cell that does not open fails the take, and stays.
    pub(crate) fn take_cell_work(&self, key: &str, options: WriteOptions) -> Result<impl Work<Option<Cell>> + use<V>> {
        let now = self.scope.now();
        let (id, path, if_version) = (self.scope.id, self.scope.path(key)?, options.if_version);
        let encrypted = self.encrypted.clone();
        Ok(move |tx: &Tx<'_>, _: &AtomicI64| {
            let Some(found) = live_at(tx, id, &path, now, if_version)? else {
                return Ok(None);
            };
            let cell = cells::live(tx, id, &path, now)?;
            cells::remove(tx, id, &path, found.spill)?;
            cell.map(|cell| opened(encrypted.as_ref(), &path, cell)).transpose()
        })
    }

    pub(crate) fn expire_work(&self, key: &str, options: WriteOptions) -> Result<impl Work<bool> + use<V>> {
        let now = self.scope.now();
        let expires = options
            .expires(now)
            .ok_or_else(|| Error::invalid("an expiry needs a ttl or a time").within(self.scope.shown_key(key)))?;
        let (id, path, if_version) = (self.scope.id, self.scope.path(key)?, options.if_version);
        Ok(move |tx: &Tx<'_>, _: &AtomicI64| {
            if live_at(tx, id, &path, now, if_version)?.is_none() {
                return Ok(false);
            }
            cells::set_expires(tx, id, &path, Some(expires))?;
            Ok(true)
        })
    }

    /// Reads a live cell on a reader, renewing an idle key when it is due.
    pub(crate) fn read_cell(&self, key: &str) -> Result<Option<Cell>> {
        let (id, path, now) = (self.scope.id, self.scope.path(key)?, self.scope.now());
        let read = |connection: &Connection| {
            let found = cells::live(connection, id, &path, now)?;
            found.map(|cell| opened(self.encrypted.as_ref(), &path, cell)).transpose()
        };
        let found = self.scope.read(key, read)?;
        Ok(found.map(|cell| self.renew(key, path, cell, now)))
    }

    /// Reads a live cell inside a transaction, renewing an idle key in it.
    pub(crate) fn read_cell_in(&self, tx: &Tx<'_>, key: &str) -> Result<Option<Cell>> {
        let (id, path, now) = (self.scope.id, self.scope.path(key)?, self.scope.now());
        let Some(cell) = cells::live(tx, id, &path, now)? else {
            return Ok(None);
        };
        let mut cell = opened(self.encrypted.as_ref(), &path, cell)?;
        if let Some(renewal) = self.renewal_due(&cell, now)
            && cells::renew(tx, id, &path, (renewal.version, renewal.seen), renewal.until)?
        {
            cell.expires = Some(renewal.until);
        }
        Ok(Some(cell))
    }

    /// One page of this branch's own rows, each with its key, read in one
    /// snapshot; the wire's pages, whose values stay rows.
    pub(crate) fn list_rows(&self, limit: usize, after: Option<&str>) -> Result<Vec<(String, Cell)>> {
        let limit = limit.clamp(1, PAGE_KEYS);
        let rows = self
            .scope
            .kv
            .file()
            .read(|connection| self.page_rows(connection, limit, after))
            .map_err(|error| error.within(self.scope.shown_branch()))?;
        self.keyed(rows)
    }

    /// Rows of this branch by path, each with its key instead.
    pub(crate) fn keyed(&self, rows: Vec<(Vec<u8>, Cell)>) -> Result<Vec<(String, Cell)>> {
        let branch = self.scope.branch()?;
        rows.into_iter().map(|(path, cell)| Ok((branch.key_of(&path)?, cell))).collect()
    }

    /// A page of this branch's own rows on `connection`, by path.
    pub(crate) fn page_rows(
        &self,
        connection: &Connection,
        limit: usize,
        after: Option<&str>,
    ) -> Result<Vec<(Vec<u8>, Cell)>> {
        let (from, past) = self.scope.branch()?.own_keys_after(after);
        let rows = cells::page(connection, self.scope.id, (&from, &past), self.scope.now(), limit)?;
        let read = |(path, cell): (Vec<u8>, Cell)| {
            let cell = opened(self.encrypted.as_ref(), &path, cell)?;
            Ok((path, cell))
        };
        rows.into_iter().map(read).collect()
    }

    /// The row as the file keeps it: sealed when the bucket is encrypted.
    fn kept(&self, path: &[u8], raw: Raw) -> Result<Raw> {
        match &self.encrypted {
            Some(encrypted) => encrypted.seal(path, &raw),
            None => Ok(raw),
        }
    }

    /// Renews a due idle key: with the next flush of renewals, or before the
    /// read returns when the key has less than a minute left. A renewal that
    /// cannot be written now waits for the flush.
    fn renew(&self, key: &str, path: Vec<u8>, mut cell: Cell, now: i64) -> Cell {
        let Some(renewal) = self.renewal_due(&cell, now) else {
            return cell;
        };
        let renewals = self.scope.kv.renewals();
        if renewal.seen - now >= RENEW_AT_ONCE {
            renewals.ask(self.scope.id, path, renewal);
            return cell;
        }
        let (id, written_path) = (self.scope.id, path.clone());
        let at_once = move |tx: &Tx<'_>, _: &AtomicI64| {
            cells::renew(tx, id, &written_path, (renewal.version, renewal.seen), renewal.until)
        };
        match self.scope.write(key, 0, at_once) {
            Ok(true) => cell.expires = Some(renewal.until),
            Ok(false) => {}
            Err(_) => renewals.ask(self.scope.id, path, renewal),
        }
        cell
    }

    fn renewal_due(&self, cell: &Cell, now: i64) -> Option<Renewal> {
        let Expiry::Idle(term) = self.expiry else {
            return None;
        };
        let (term, expires) = (millis(term), cell.expires?);
        if expires - now > term - term / REFRESHES {
            return None;
        }
        Some(Renewal { version: cell.version, seen: expires, until: now.saturating_add(term) })
    }

    fn default_expires(&self, now: i64) -> Option<i64> {
        match self.expiry {
            Expiry::Never => None,
            Expiry::Ttl(span) | Expiry::Idle(span) => Some(now.saturating_add(millis(span))),
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
        Bucket { encrypted: self.encrypted.clone(), ..Bucket::new(self.scope.clone(), self.expiry) }
    }
}

impl<V> fmt::Debug for Bucket<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bucket({:?})", self.scope)
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
        write!(f, "All({:?})", self.bucket.scope)
    }
}

/// The clear of a scope's branch: every key under it deleted when they are few,
/// else the branch marked cleared at a new version, which hides them at once.
pub(crate) fn clear_work(scope: &Scope) -> Result<impl Work<()> + use<>> {
    let branch = scope.branch()?;
    let (id, prefix, everything) = (scope.id, branch.prefix().to_vec(), branch.everything());
    Ok(move |tx: &Tx<'_>, revision: &AtomicI64| {
        // a bucket cleared whole holds no value of any key, and takes the key of its next write
        if prefix.is_empty() {
            cells::keep_bucket_key(tx, id, None)?;
        }
        if cells::delete_under(tx, id, (&everything.0, &everything.1), CLEAR_BOUND)? {
            return Ok(());
        }
        let cleared = next_version(revision, tx)?;
        cells::mark_cleared(tx, id, &prefix, cleared)
    })
}

/// A cell of the file as a caller reads it: opened when its bucket is
/// encrypted.
fn opened(encrypted: Option<&Encrypted>, path: &[u8], mut cell: Cell) -> Result<Cell> {
    if let Some(encrypted) = encrypted {
        cell.raw = encrypted.open(path, &cell.raw)?;
    }
    Ok(cell)
}

/// A row read back as `V`; a row that is not one is corrupt.
pub(crate) fn decode<V: Value>(raw: &Raw) -> Result<V> {
    value::decode(raw)
        .map_err(|mismatch| Error::corrupt(format!("its value does not read as the bucket's type: {mismatch}")))
}

/// The key's current cell when it is live and, if a version is asked, at that
/// version; a key that is not is a conflict when a version was asked.
fn live_at(tx: &Tx<'_>, id: i64, path: &[u8], now: i64, if_version: Option<Version>) -> Result<Option<cells::Current>> {
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

pub(crate) fn check_expiry(expiry: Expiry) -> Result<()> {
    match expiry {
        Expiry::Ttl(span) | Expiry::Idle(span) if span.is_zero() => Err(Error::invalid("an expiry of zero")),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "bucket_tests.rs"]
mod tests;
