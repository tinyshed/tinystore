//! `get_or_create`: a file made once on a miss. A miss claims its path in a
//! row of `_tinystore_blobs_filling`, leased for 30 s and renewed while the
//! function runs, and every other `get_or_create` of the path meanwhile waits
//! for that run instead of running its own (BLOB-21). The run's result lands
//! only if, at its commit, the row still names the run and the path holds no
//! file (BLOB-22): a write or a delete of the path deletes the row, and a run
//! whose lease ended has lost it.
//!
//! | How the run ended                      | What the calls that waited for it do           |
//! |----------------------------------------|------------------------------------------------|
//! | its file landed                        | read it                                        |
//! | a put, a delete or a lease ended it    | look again: a file, another claim, or a run    |
//! | its function or its write failed       | fail with that error; the next call runs again |
//!
//! The store's directory is one process's, its `LOCK`'s, so a claim no run of
//! this process holds is one a process that died left: it is taken over, and
//! the open deletes every claim before anything runs. A run waits only for a
//! run of its own process, through a waiter called as that run ends, in the
//! process or for a client of its server, which runs the function there.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};

use super::change::Change;
use super::engine::Blobs;
use super::files::{Files, Options};
use super::info::FileInfo;
use super::read::{PIECE, StoredFile};
use super::upload::Upload;
use crate::sqlite::{Tx, own_work, sql_error};
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

/// How long a claim holds unless it is renewed.
pub(crate) const LEASE: Duration = Duration::from_secs(30);
/// How often the claims of the runs under way are renewed: a third of the
/// lease, so that a renewal late by one turn still holds it.
pub(crate) const RENEW_EVERY: Duration = Duration::from_secs(10);
/// How long a claim is renewed unless `timeout` says: a function still
/// running then loses its claim within a lease.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(10 * 60);

const SELECT_CLAIM: &str = "select owner, until from _tinystore_blobs_filling where set_id = ?1 and path = ?2";
const UPSERT_CLAIM: &str = "insert into _tinystore_blobs_filling (set_id, path, owner, until) values (?1, ?2, ?3, ?4)
    on conflict (set_id, path) do update set owner = excluded.owner, until = excluded.until";
const RENEW_CLAIM: &str =
    "update _tinystore_blobs_filling set until = ?4 where set_id = ?1 and path = ?2 and owner = ?3 and until > ?5";
const PURGE_CLAIMS: &str = "delete from _tinystore_blobs_filling where until <= ?1";
const FORGET_CLAIMS: &str = "delete from _tinystore_blobs_filling";
/// The fence: the run's own claim, still leased, taken in the transaction
/// that writes its file, so that a run that lost it writes nothing.
pub(super) const TAKE_CLAIM: &str =
    "delete from _tinystore_blobs_filling where set_id = ?1 and path = ?2 and owner = ?3 and until > ?4";
/// What every other write or delete of a path does to its claim.
pub(super) const END_CLAIM: &str =
    "delete from _tinystore_blobs_filling where set_id = ?1 and path = ?2 returning owner";
/// What a clear of a folder does to the claims under it.
pub(super) const END_CLAIMS_UNDER: &str =
    "delete from _tinystore_blobs_filling where set_id = ?1 and path >= ?2 and path < ?3 returning owner";

/// What a `get_or_create`'s function gives: the file's bytes, whole, as a
/// reader or in a file of this machine, and the type and the meta it is
/// written with when the call gave none.
///
/// ```no_run
/// # fn main() -> tinystore::Result<()> {
/// # let store = tinystore::Store::open("data", Default::default())?;
/// use tinystore::blobs::Fill;
///
/// let pages = store.files("pages").open()?;
/// let page = pages.key("7f3a.pdf").get_or_create(|| {
///     let pdf = std::fs::File::open("/tmp/7f3a.pdf").map_err(|error| tinystore::Error::io("the page", error))?;
///     Ok::<_, tinystore::Error>(Fill::reader(pdf).content_type("application/pdf")) // streamed, never whole in memory
/// })?;
/// # Ok(())
/// # }
/// ```
pub struct Fill {
    body: Body,
    content_type: Option<String>,
    meta: BTreeMap<String, String>,
}

enum Body {
    Bytes(Vec<u8>),
    Reader(Box<dyn Read + Send>),
    Path(PathBuf),
}

impl Fill {
    pub fn bytes(bytes: impl Into<Vec<u8>>) -> Fill {
        Fill::of(Body::Bytes(bytes.into()))
    }

    /// Bytes read to their end as they are written, a piece at a time.
    pub fn reader(reader: impl Read + Send + 'static) -> Fill {
        Fill::of(Body::Reader(Box::new(reader)))
    }

    /// The bytes of a file of this machine, read as they are written.
    pub fn file(path: impl Into<PathBuf>) -> Fill {
        Fill::of(Body::Path(path.into()))
    }

    /// The file's type, in place of the call's.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// A string of the program's the file carries; any given here take the
    /// place of the call's.
    pub fn meta(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.meta.insert(name.into(), value.into());
        self
    }

    fn of(body: Body) -> Fill {
        Fill { body, content_type: None, meta: BTreeMap::new() }
    }
}

impl fmt::Debug for Fill {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let body = match &self.body {
            Body::Bytes(bytes) => format!("{} bytes", bytes.len()),
            Body::Reader(_) => "a reader".to_owned(),
            Body::Path(path) => format!("{}", path.display()),
        };
        write!(f, "Fill({body})")
    }
}

impl From<Vec<u8>> for Fill {
    fn from(bytes: Vec<u8>) -> Fill {
        Fill::bytes(bytes)
    }
}

impl From<&[u8]> for Fill {
    fn from(bytes: &[u8]) -> Fill {
        Fill::bytes(bytes)
    }
}

impl From<String> for Fill {
    fn from(text: String) -> Fill {
        Fill::bytes(text)
    }
}

impl From<&str> for Fill {
    fn from(text: &str) -> Fill {
        Fill::bytes(text)
    }
}

/// What one `get_or_create` call came to, as the store's own metrics count it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    /// The file was there.
    Hit,
    /// It joined a run another call held, and read what that run wrote.
    Wait,
    /// It ran the function.
    Run,
    Failed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Hit => "hit",
            Outcome::Wait => "wait",
            Outcome::Run => "run",
            Outcome::Failed => "failed",
        }
    }

    /// Counts the call under its set.
    pub(crate) fn count(self, files: &Files) {
        let labels = [("set", files.name()), ("outcome", self.as_str())];
        files.blobs.probe.count("tinystore_files_get_or_create_total", &labels, 1.0);
    }
}

/// What a call that waits for a run is told as that run ends: the failure
/// it ended with, or none when the caller is to look again.
pub(crate) type Waiter = Box<dyn FnOnce(Option<Arc<Error>>) + Send>;

/// The runs of `get_or_create` under way in this process, by their owner,
/// each with the calls waiting for it. A waiter is a callback rather than a
/// blocked thread, so that a client of the server waits on no thread.
#[derive(Default)]
pub(crate) struct Fills {
    runs: Mutex<HashMap<i64, Run>>,
}

struct Run {
    set: i64,
    path: String,
    /// Until when, by the store's clock, its claim is renewed.
    renew_until: i64,
    /// The thread that runs its function, for a blocking call: a call of the
    /// same path inside it would wait for itself.
    thread: Option<ThreadId>,
    waiters: Vec<Waiter>,
}

/// How a call that would wait for `owner` was answered.
enum Joined {
    Waiting,
    /// No run of this process is `owner`.
    NotHere,
    /// The run is the caller's own.
    Itself,
}

impl Fills {
    fn start(&self, owner: i64, (set, path): (i64, &str), renew_until: i64, thread: Option<ThreadId>) {
        let run = Run { set, path: path.to_owned(), renew_until, thread, waiters: Vec::new() };
        lock(&self.runs).insert(owner, run);
    }

    fn join(&self, owner: i64, caller: Option<ThreadId>, waiter: Waiter) -> Joined {
        let mut runs = lock(&self.runs);
        match runs.get_mut(&owner) {
            Some(run) if caller.is_some() && run.thread == caller => Joined::Itself,
            Some(run) => {
                run.waiters.push(waiter);
                Joined::Waiting
            }
            None => Joined::NotHere,
        }
    }

    /// Ends the run `owner`, if it is under way here, and tells everyone who
    /// waited for it, outside the lock.
    pub(super) fn end(&self, owner: i64, failure: Option<Arc<Error>>) {
        let run = lock(&self.runs).remove(&owner);
        for waiter in run.map(|run| run.waiters).unwrap_or_default() {
            waiter(failure.clone());
        }
    }

    /// The claims to renew at `now`: the runs whose timeout has not passed.
    fn renewing(&self, now: i64) -> Vec<(i64, i64, String)> {
        let runs = lock(&self.runs);
        runs.iter()
            .filter(|(_, run)| run.renew_until > now)
            .map(|(&owner, run)| (owner, run.set, run.path.clone()))
            .collect()
    }

    /// Renews `owner`'s claim no more: another took it, or a write ended it.
    fn stop_renewing(&self, owner: i64) {
        if let Some(run) = lock(&self.runs).get_mut(&owner) {
            run.renew_until = i64::MIN;
        }
    }

    /// How many runs are under way, and how many calls wait for them.
    #[cfg(test)]
    pub(crate) fn under_way(&self) -> (usize, usize) {
        let runs = lock(&self.runs);
        (runs.len(), runs.values().map(|run| run.waiters.len()).sum())
    }
}

impl fmt::Debug for Fills {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fills({} under way)", lock(&self.runs).len())
    }
}

/// How a call's claim of a path came out.
pub(crate) enum Hand {
    /// A live file is there: the call reads it.
    There,
    /// A run of this process holds the path; the waiter is called as it ends.
    Waiting,
    /// The run is the caller's.
    Yours(Filling),
}

/// What the claim's transaction found.
enum Claimed {
    There,
    Waiting,
    Mine,
}

/// A run a call holds the claim of; dropping it ends the run, and the calls
/// that waited for it look again, or fail with what [`Filling::fail`] gave.
pub(crate) struct Filling {
    files: Files,
    whole: String,
    owner: i64,
    failure: Option<Arc<Error>>,
}

impl Files {
    /// Claims the path at `whole` for a run of the caller's, unless a live
    /// file is there or a run of this process holds it, `waiter` then called
    /// once that run ends. `timeout` bounds how long the claim is renewed.
    /// A blocking caller names its thread, so that a call of the path inside
    /// its own run is `Invalid` rather than a wait for itself.
    pub(crate) fn hand(
        &self,
        whole: &str,
        timeout: Duration,
        caller: Option<ThreadId>,
        waiter: Waiter,
    ) -> Result<Hand> {
        let owner = drawn();
        let (blobs, set, path) = (Arc::clone(&self.blobs), self.set, whole.to_owned());
        let renew_for = crate::clock::millis(timeout);
        let claimed = self.blobs.file.write(0, move |tx| {
            let asked = Asked { set, path: &path, owner, renew_for, caller };
            claim(tx, &blobs, &asked, waiter)
        });
        match claimed {
            Ok(Claimed::There) => Ok(Hand::There),
            Ok(Claimed::Waiting) => Ok(Hand::Waiting),
            Ok(Claimed::Mine) => {
                Ok(Hand::Yours(Filling { files: self.clone(), whole: whole.to_owned(), owner, failure: None }))
            }
            Err(error) => {
                self.blobs.fills.end(owner, None);
                Err(error)
            }
        }
    }
}

/// A claim as its transaction asks it.
struct Asked<'a> {
    set: i64,
    path: &'a str,
    owner: i64,
    /// How long the claim is renewed, in milliseconds.
    renew_for: i64,
    caller: Option<ThreadId>,
}

/// The claim's transaction: a live file is there, or the run that holds the
/// path is joined, or the path is claimed for `owner`, taking over a claim no
/// run of this process holds or whose lease ended. That run's waiters look
/// again, and find this claim.
fn claim(tx: &Tx<'_>, blobs: &Blobs, asked: &Asked<'_>, waiter: Waiter) -> Result<Claimed> {
    let change = Change::new(tx, blobs);
    if change.current(asked.set, asked.path)?.is_some_and(|current| current.live(change.now)) {
        return Ok(Claimed::There);
    }
    let held: Option<(i64, i64)> = tx
        .prepare_cached(SELECT_CLAIM)
        .and_then(|mut select| select.query_row(params![asked.set, asked.path], |row| Ok((row.get(0)?, row.get(1)?))))
        .optional()
        .map_err(|error| sql_error("blobs: a path's claim", error))?;
    if let Some((holder, until)) = held {
        if until > change.now {
            match blobs.fills.join(holder, asked.caller, waiter) {
                Joined::Waiting => return Ok(Claimed::Waiting),
                Joined::Itself => {
                    return Err(Error::invalid("a getOrCreate of a path inside its own run would wait for itself"));
                }
                Joined::NotHere => {}
            }
        }
        blobs.fills.end(holder, None);
    }
    let until = change.now.saturating_add(crate::clock::millis(LEASE));
    blobs.fills.start(asked.owner, (asked.set, asked.path), change.now.saturating_add(asked.renew_for), asked.caller);
    change.execute(UPSERT_CLAIM, params![asked.set, asked.path, asked.owner, until])?;
    Ok(Claimed::Mine)
}

impl Filling {
    #[cfg(feature = "wire")]
    pub(crate) fn owner(&self) -> i64 {
        self.owner
    }

    /// Whether the run is of these files' path at `whole`.
    #[cfg(feature = "wire")]
    pub(crate) fn is_of(&self, files: &Files, whole: &str) -> bool {
        Arc::ptr_eq(&self.files.blobs, &files.blobs) && self.files.set == files.set && self.whole == whole
    }

    /// Writes what the function gave, with the call's `options`, unless the
    /// run lost its claim or a file is there; says what it wrote.
    pub(crate) fn land(&self, fill: Fill, options: &Options) -> Result<Option<FileInfo>> {
        let mut options = Options { fill: Some(self.owner), ..options.clone() };
        if let Some(content_type) = fill.content_type {
            options.content_type = Some(content_type);
        }
        if !fill.meta.is_empty() {
            options.meta = fill.meta;
            options.meta_given = true;
        }
        options.check().map_err(|error| self.files.fail(&self.whole, error))?;
        let mut upload = Upload::start(self.files.clone(), self.whole.clone(), options)?;
        match fill.body {
            Body::Bytes(bytes) => upload.take(&bytes)?,
            Body::Reader(reader) => pour(&mut upload, reader)?,
            Body::Path(path) => {
                let file = std::fs::File::open(&path)
                    .map_err(|error| Error::io(format!("blobs: open {}", path.display()), error))
                    .map_err(|error| self.files.fail(&self.whole, error))?;
                pour(&mut upload, file)?;
            }
        }
        upload.publish(true)
    }

    /// Ends the run with `failure`, which every call that waited for it gets.
    pub(crate) fn fail(mut self, failure: Error) {
        self.failure = Some(Arc::new(failure));
    }
}

impl Drop for Filling {
    fn drop(&mut self) {
        self.files.blobs.fills.end(self.owner, self.failure.take());
    }
}

impl fmt::Debug for Filling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Filling({:?}, {})", self.files, self.whole)
    }
}

/// Takes every byte `reader` gives into the upload, a piece at a time.
fn pour(upload: &mut Upload, mut reader: impl Read) -> Result<()> {
    let mut piece = vec![0; PIECE];
    loop {
        match reader.read(&mut piece) {
            Ok(0) => return Ok(()),
            Ok(read) => upload.take(&piece[..read])?,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Error::io("blobs: read what a getOrCreate's function gave", error)),
        }
    }
}

impl Blobs {
    /// Renews the claims of the runs under way, and deletes those whose lease
    /// ended: the store's background work every [`RENEW_EVERY`]. A run whose
    /// claim is gone is renewed no more.
    pub(crate) fn renew_claims(self: &Arc<Self>) -> Result<()> {
        let now = self.now();
        let renewing = self.fills.renewing(now);
        if renewing.is_empty() && lock(&self.fills.runs).is_empty() {
            return Ok(());
        }
        let until = now.saturating_add(crate::clock::millis(LEASE));
        let blobs = Arc::clone(self);
        let lost = own_work(|| {
            self.file.write(0, move |tx| {
                let change = Change::new(tx, &blobs);
                let mut lost = Vec::new();
                for (owner, set, path) in renewing {
                    let renewed = tx
                        .prepare_cached(RENEW_CLAIM)
                        .and_then(|mut renew| renew.execute(params![set, path, owner, until, change.now]))
                        .map_err(|error| sql_error("blobs: renew a claim", error))?;
                    if renewed == 0 {
                        lost.push(owner);
                    }
                }
                change.execute(PURGE_CLAIMS, params![change.now])?;
                Ok(lost)
            })
        })?;
        for owner in lost {
            self.fills.stop_renewing(owner);
        }
        Ok(())
    }

    /// Deletes every claim as the engine opens: no run of this process holds
    /// one yet, so each is a process's that died.
    pub(super) fn forget_claims(&self) -> Result<()> {
        own_work(|| {
            self.file.write(0, |tx| {
                tx.execute(FORGET_CLAIMS, []).map(drop).map_err(|error| sql_error("blobs: forget the claims", error))
            })
        })
    }
}

/// The file at the path, or the one `work` makes, written once while the
/// calls of the path wait for it: the blocking `get_or_create`.
pub(super) fn get_or_create<T, E>(
    files: &Files,
    whole: &str,
    options: &Options,
    mut work: impl FnMut() -> std::result::Result<T, E>,
) -> std::result::Result<StoredFile, E>
where
    T: Into<Fill>,
    E: From<Error> + fmt::Display + 'static,
{
    let timeout = options.timeout.unwrap_or(TIMEOUT);
    let mut outcome = Outcome::Hit;
    let answered = loop {
        match files.get_whole(whole) {
            Ok(Some(file)) => break Ok(file),
            Ok(None) => {}
            Err(error) => break Err(E::from(error)),
        }
        let (told, ended) = mpsc::channel();
        let waiter: Waiter = Box::new(move |failure| {
            let _ = told.send(failure);
        });
        let handed = crate::polled::may_wait(|| format!("{}: {whole:?}: a getOrCreate", files.describe()))
            .and_then(|()| files.hand(whole, timeout, Some(thread::current().id()), waiter))
            .map_err(|error| files.fail(whole, error));
        let filling = match handed {
            Err(error) => break Err(E::from(error)),
            Ok(Hand::There) => continue,
            Ok(Hand::Waiting) => {
                outcome = outcome.max(Outcome::Wait);
                // a waiter dropped unsent, as the store closes, is a look again
                match ended.recv() {
                    Ok(Some(failure)) => break Err(E::from(failure.duplicate())),
                    _ => continue,
                }
            }
            Ok(Hand::Yours(filling)) => filling,
        };
        outcome = Outcome::Run;
        let fill = match work() {
            Ok(fill) => fill.into(),
            Err(error) => {
                filling.fail(failure_of(&error).within(format!("files {}: {whole:?}", files.name())));
                break Err(error);
            }
        };
        if let Err(error) = filling.land(fill, options) {
            filling.fail(error.duplicate());
            break Err(E::from(error));
        }
        // landed, or lost to a write or a lease: either way the path says now
    };
    if answered.is_err() {
        outcome = Outcome::Failed;
    }
    outcome.count(files);
    answered
}

/// What the calls that waited for a run whose function failed with `error`
/// get: the store's own error as it is, any other `conflict`, its text
/// kept: the call may run the function itself by calling again, and no
/// client's link resends a conflict as it resends an `unavailable`.
pub(crate) fn failure_of<E: fmt::Display + 'static>(error: &E) -> Error {
    match (error as &dyn std::any::Any).downcast_ref::<Error>() {
        Some(error) => error.duplicate(),
        None => Error::new(ErrorKind::Conflict, format!("the run waited for failed: {error}")),
    }
}

/// 63 bits for a run's owner that no earlier call gave, as records' event
/// ids draw theirs: SipHash of a count under keys std draws from the system.
fn drawn() -> i64 {
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    i64::try_from(hasher.finish() >> 1).unwrap_or_default()
}

#[cfg(test)]
#[path = "filling_tests.rs"]
mod tests;
