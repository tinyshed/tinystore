use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::bucket::{Bucket, Expiry};
#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
use super::bucket::{Put, Version, WriteOptions};
use super::cells::Cell;
use super::fresh::FreshBuilder;
use super::path::Key;
use super::scope::{Kind, Scope};
use super::value::{Raw, Value};
use crate::clock::millis;
use crate::engine::Home;
use crate::{Error, Result, Store};

/// How long an answer is kept unless `keep` says.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);

/// Answers being opened: their name and type, and how long one is kept.
#[must_use = "once's answers open with open()"]
pub struct OnceBuilder<V> {
    store: Store,
    name: String,
    keep: Duration,
    _value: std::marker::PhantomData<fn() -> V>,
}

impl Store {
    /// Functions run once a key, their answers kept in the store's kv.db: a
    /// request sent twice gets the first one's answer instead of doing its work
    /// again, as an idempotency key promises.
    pub fn once<V: Value>(&self, name: &str) -> OnceBuilder<V> {
        OnceBuilder { store: self.clone(), name: name.to_owned(), keep: KEEP, _value: std::marker::PhantomData }
    }
}

impl<V: Value> OnceBuilder<V> {
    /// How long an answer is kept from when it was made; a day unless said.
    /// After that, a run of its key runs again.
    pub fn keep(mut self, keep: Duration) -> Self {
        self.keep = keep;
        self
    }

    /// How long an answer is answered without making a new one, shorter than
    /// `keep`: past it, the kept answer is answered at once and one new one
    /// is made in the background, as [`crate::kv::Fresh`] says.
    pub fn fresh(self, fresh: Duration) -> FreshBuilder<V> {
        FreshBuilder::new(self, fresh)
    }

    pub fn open(self) -> Result<Once<V>> {
        self.open_with(None)
    }

    /// Opens the answers, stale past `fresh` when it is given.
    pub(super) fn open_with(self, fresh: Option<Duration>) -> Result<Once<V>> {
        Ok(Once { answers: answers(&self.store, &self.name, (self.keep, fresh))?, fresh })
    }
}

/// Once's answers as rows, for the wire: a client runs the function, and the
/// store keeps what it answered under the key for `keep`, stale past `fresh`.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) fn rows(store: &Store, name: &str, keep: Option<Duration>, fresh: Option<Duration>) -> Result<Bucket<()>> {
    answers(store, name, (keep.unwrap_or(KEEP), fresh))
}

/// The bucket that keeps once's answers of a name, each for `keep`. A `fresh`
/// of zero would answer every kept answer stale, and one as long as `keep`
/// never would.
fn answers<T>(store: &Store, name: &str, (keep, fresh): (Duration, Option<Duration>)) -> Result<Bucket<T>> {
    let refused = |why: &str| Err(Error::invalid(why.to_owned()).within(format!("kv once {name}")));
    if keep.is_zero() {
        return refused("a keep of zero");
    }
    match fresh {
        Some(fresh) if fresh.is_zero() => return refused("a fresh of zero"),
        Some(fresh) if fresh >= keep => return refused("a fresh as long as its keep or longer"),
        _ => {}
    }
    let scope = Scope::open(&Home::Store(store.clone()), name, Kind::Once)?;
    Ok(Bucket::new(scope, Expiry::Ttl(keep)))
}

/// An answer with how old it is: `stale` once it is older than the `fresh`
/// its answers opened with, and never for answers opened without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer<V> {
    pub value: V,
    pub age: Duration,
    pub stale: bool,
}

/// How old a kept answer is at `now`. The store keeps when an answer ends
/// rather than when it was made, so its age is read back through `keep`: an
/// answer a handle of another `keep` wrote reads shifted by the difference,
/// within `[0, keep]`.
pub(crate) fn age_of(expires: Option<i64>, now: i64, keep: Duration) -> Duration {
    let keep = millis(keep);
    let made = expires.map_or(now, |expires| expires.saturating_sub(keep));
    Duration::from_millis(now.saturating_sub(made).clamp(0, keep).unsigned_abs())
}

/// How long no build of a key starts in the background after one failed, so
/// that a source that is down is not asked by every call: a tenth of `fresh`,
/// a second at least and a minute at most.
pub(crate) fn pause_after(fresh: Duration) -> Duration {
    (fresh / 10).clamp(Duration::from_secs(1), Duration::from_secs(60))
}

/// Counts one of once's own metrics under its answers' name: a branch and
/// nothing made while the store measures nothing.
pub(crate) fn counted(scope: &Scope, metric: &'static str) {
    scope.kv.probe().count(metric, &[("name", scope.name())], 1.0);
}

pub(crate) const BUILDS: &str = "tinystore_once_builds_total";
pub(crate) const FAILURES: &str = "tinystore_once_build_failures_total";
pub(crate) const STALE: &str = "tinystore_once_stale_total";

/// How old a kept cell's answer is now, and whether it is older than `fresh`.
pub(crate) fn aged<T>(answers: &Bucket<T>, cell: &Cell, fresh: Option<Duration>) -> (Duration, bool) {
    let age = age_of(cell.expires, answers.scope.now(), answers.kept_for().unwrap_or(KEEP));
    (age, fresh.is_some_and(|fresh| age >= fresh))
}

/// The cell kept under `key` while its answer is still stale, read again once
/// a build in the background claimed the key: a run that let the key go
/// between a caller's first read and its claim made a fresh one, or a delete
/// took it, and then no build is made.
pub(crate) fn still_stale<T>(answers: &Bucket<T>, key: &str, fresh: Option<Duration>) -> Result<Option<Cell>> {
    let cell = answers.read_cell(key)?;
    Ok(cell.filter(|cell| aged(answers, cell, fresh).1))
}

/// An answer kept under a key, as a client is given it.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) struct Kept {
    pub(crate) raw: Raw,
    #[cfg_attr(not(feature = "wire"), expect(dead_code, reason = "a node's claim reads only the answer"))]
    pub(crate) age: Duration,
    #[cfg_attr(not(feature = "wire"), expect(dead_code, reason = "a node's claim reads only the answer"))]
    pub(crate) stale: bool,
}

/// What a client's run of a key starts with.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) enum Hand {
    /// The answer kept under the key.
    Kept(Kept),
    /// The answer kept, stale, and a new one for the client to make in the
    /// background: its answer replaces the kept one, its failure keeps it.
    Refresh(
        Kept,
        #[cfg_attr(
            not(feature = "wire"),
            expect(dead_code, reason = "a node's claim asks for no fresh, so never refreshes")
        )]
        Handed,
    ),
    /// The run is the client's: it runs its function and keeps what it answers.
    Yours(Handed),
    /// Another run holds the key; the waiter is called once it ends.
    Wait,
}

/// Starts a client's run of `key` on once's rows: the answer kept, or the run
/// handed over, or, while another run holds the key, a wait that calls `waiter`
/// once it ends, when the client asks again. Past `fresh`, a kept answer goes
/// with a build of a new one when no run holds the key and none failed
/// within its pause, and a stale answer never waits.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) fn hand(answers: &Bucket<()>, fresh: Option<Duration>, key: &str, waiter: Waiter) -> Result<Hand> {
    if let Some(cell) = answers.read_cell(key)? {
        let (age, stale) = aged(answers, &cell, fresh);
        let kept = Kept { raw: cell.raw, age, stale };
        if !stale {
            return Ok(Hand::Kept(kept));
        }
        counted(&answers.scope, STALE);
        let running = (answers.scope.id, answers.scope.path(key)?);
        if !answers.scope.kv.runs().claim_refresh(&running, answers.scope.now()) {
            return Ok(Hand::Kept(kept));
        }
        let handed = Handed::new(answers.clone(), key, running, fresh.map(pause_after));
        // read again under the claim: a run that let the key go meanwhile made a fresh one
        let Some(cell) = answers.read_cell(key)? else { return Ok(Hand::Kept(kept)) };
        let (age, stale) = aged(answers, &cell, fresh);
        let (version, kept) = (cell.version, Kept { raw: cell.raw, age, stale });
        return Ok(if stale { Hand::Refresh(kept, handed.bound(version)) } else { Hand::Kept(kept) });
    }
    let running = (answers.scope.id, answers.scope.path(key)?);
    if !answers.scope.kv.runs().claim_or_wait(running.clone(), None, waiter)? {
        return Ok(Hand::Wait);
    }
    let handed = Handed::new(answers.clone(), key, running, None);
    match answers.read_cell(key)? {
        Some(cell) => {
            let (age, stale) = aged(answers, &cell, fresh);
            Ok(Hand::Kept(Kept { raw: cell.raw, age, stale }))
        }
        None => Ok(Hand::Yours(handed)),
    }
}

/// A run of a key a client was handed; dropping it lets the runs waiting for
/// the key go on, keeping nothing.
#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
pub(crate) struct Handed {
    answers: Bucket<()>,
    key: String,
    running: (i64, Vec<u8>),
    /// The client's answer when it could not be kept, for the runs waiting.
    unkept: Option<Box<Raw>>,
    /// For a build in the background, how long no other starts once it failed.
    pause: Option<Duration>,
    /// For a build in the background, the version of the stale answer it
    /// replaces: one deleted or cleared meanwhile keeps the build's answer out.
    replaces: Option<i64>,
}

#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
impl Handed {
    pub(crate) fn new(answers: Bucket<()>, key: &str, running: (i64, Vec<u8>), pause: Option<Duration>) -> Handed {
        Handed { answers, key: key.to_owned(), running, unkept: None, pause, replaces: None }
    }

    /// The build in the background replaces the stale answer at `version`,
    /// and only while that answer is there.
    pub(crate) fn bound(mut self, version: i64) -> Handed {
        self.replaces = Some(version);
        self
    }

    /// How the run's answer is written: a build in the background replaces a
    /// live answer, whose expiry a write would keep, and starts its `keep`
    /// again, while that answer is the one it was handed.
    fn options(&self) -> WriteOptions {
        let options = match (self.pause, self.answers.kept_for()) {
            (Some(_), Some(keep)) => WriteOptions::default().ttl(keep),
            _ => WriteOptions::default(),
        };
        match self.replaces {
            Some(version) => options.if_version(Version::new(version)),
            None => options,
        }
    }

    /// Says that the run's answer could not be kept, unless a build in the
    /// background found its stale answer deleted, cleared or ended: then
    /// nobody is owed it, and it is no failure.
    fn could_not_keep(&mut self, error: &Error, raw: Raw) {
        if self.replaces.is_some() && error.kind() == crate::ErrorKind::Conflict {
            return;
        }
        not_kept(error);
        self.unkept = Some(Box::new(raw));
    }

    /// Says that the run's function failed: nothing is kept, and a build in
    /// the background holds off the next for its pause.
    pub(crate) fn failed(&self) {
        counted(&self.answers.scope, BUILDS);
        counted(&self.answers.scope, FAILURES);
        if let Some(pause) = self.pause {
            let now = self.answers.scope.now();
            self.answers.scope.kv.runs().pause(&self.running, now.saturating_add(millis(pause)), now);
        }
    }
}

#[cfg(any(feature = "wire", feature = "replication"))]
impl Handed {
    /// Keeps the client's answer, or nothing when its function failed, and
    /// lets the key go. An answer that could not be kept goes to the runs that
    /// waited for this one, as [`Once::run`]'s does, and the failure is logged.
    pub(crate) fn keep(mut self, answer: Option<Raw>) {
        let Some(raw) = answer else { return self.failed() };
        counted(&self.answers.scope, BUILDS);
        if let Err(error) = self.answers.put_raw(&self.key, raw.clone(), self.options(), Put::Always) {
            self.could_not_keep(&error, raw);
        }
    }
}

#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
impl Drop for Handed {
    fn drop(&mut self) {
        self.answers.scope.kv.runs().release(&self.running, self.unkept.take().map(|raw| *raw));
    }
}

#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
impl fmt::Debug for Handed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handed({})", self.answers.scope.shown_key(&self.key))
    }
}

/// The run of an awaited call, whose function is a future the call polls:
/// the async API's `Once::run` claims, keeps and lets go through these.
#[cfg(feature = "async")]
impl Handed {
    /// Keeps `raw` under the run's key without waiting, and lets the key go
    /// once its commit ended, so that no run of the key starts before the
    /// answer is there to read: `done` hears then. An answer that could not
    /// be kept goes to the runs that waited for this one, and the failure is
    /// logged, as [`Once::run`]'s does.
    pub(crate) fn keep_then(mut self, raw: Raw, done: impl FnOnce() + Send + 'static) {
        counted(&self.answers.scope, BUILDS);
        let (answers, key) = (self.answers.clone(), self.key.clone());
        answers.put_raw_then(&key, (raw.clone(), self.options(), Put::Always), move |kept| {
            if let Err(error) = kept {
                self.could_not_keep(&error, raw);
            }
            drop(self);
            done();
        });
    }

    /// Lets the key go, nothing kept, for an answer that does not encode.
    pub(crate) fn not_kept(self, error: &Error) {
        counted(&self.answers.scope, BUILDS);
        not_kept(error);
    }

    /// Runs `poll`, a poll of the run's function, with its key marked as run
    /// on this thread now: a run of the key inside it, awaited or blocking,
    /// is `Invalid`, as one inside a blocking run is, rather than wait for
    /// itself. A task has no thread of its own, so the mark lasts one poll.
    pub(crate) fn polling<T>(&self, poll: impl FnOnce() -> T) -> T {
        let runs = std::ptr::from_ref(self.answers.scope.kv.runs()).addr();
        POLLING.with(|polling| polling.borrow_mut().push((runs, self.running.clone())));
        let _unmark = Unmark;
        poll()
    }
}

#[cfg(feature = "async")]
thread_local! {
    /// The keys whose awaited run's function this thread polls now, each by
    /// the address of the runs that hold it: two stores may share a key.
    static POLLING: std::cell::RefCell<Polled> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Keys polled now, each by the address of its runs.
#[cfg(feature = "async")]
type Polled = Vec<(usize, (i64, Vec<u8>))>;

/// Takes a poll's mark off as the poll ends, a panic's unwinding included.
#[cfg(feature = "async")]
struct Unmark;

#[cfg(feature = "async")]
impl Drop for Unmark {
    fn drop(&mut self) {
        POLLING.with(|polling| polling.borrow_mut().pop());
    }
}

/// What a run on a copy found at its home: the answer kept there, or the run
/// handed to this node by its token, which its keep names.
#[cfg(feature = "replication")]
pub(crate) enum AtHome<V> {
    Kept(V),
    Yours(u64),
}

/// Functions run at most once a key, their answers kept: a charge, an import,
/// a webhook's effect, done once however often the request arrives.
pub struct Once<V> {
    pub(super) answers: Bucket<V>,
    /// Set for the answers a [`crate::kv::Fresh`] holds.
    pub(super) fresh: Option<Duration>,
}

impl<V: Value> Once<V> {
    /// The same answers seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Once<V> {
        Once { answers: self.answers.under(owner), fresh: self.fresh }
    }

    /// [`Once::run`]'s answer with how old it is: no older than `keep`, and
    /// never stale, since these answers were opened without `fresh`.
    ///
    /// ```no_run
    /// # fn main() -> tinystore::Result<()> {
    /// # let store = tinystore::Store::open("data", Default::default())?;
    /// let charges = store.once::<String>("charges").open()?;
    /// let receipt = charges.answer("key-7f", || Ok::<_, tinystore::Error>("receipt 1".to_owned()))?;
    /// println!("{} made {:?} ago", receipt.value, receipt.age);
    /// # Ok(())
    /// # }
    /// ```
    pub fn answer<E: From<Error>>(&self, key: impl Key, work: impl FnOnce() -> Result<V, E>) -> Result<Answer<V>, E> {
        let key = key.text();
        let found = self.answers.read_cell(&key)?;
        self.build(&key, found, work)
    }

    /// The answer kept under `key`, or `work`'s, which is then kept.
    ///
    /// While another run of the key is under way, in this process or through
    /// its server, this one waits for it and returns its answer. An error keeps
    /// nothing, so the next run runs again; an answer meant to be kept, a card
    /// declined, is a value rather than an error. An answer that could not be
    /// kept is still returned, to this caller and to those that waited for this
    /// run, and the failure logged.
    pub fn run<E: From<Error>>(&self, key: impl Key, work: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        self.answer(key, work).map(|answer| answer.value)
    }

    /// The answer `found` kept under `key`, or once no run holds the key, the
    /// answer of `work`, which is then kept: [`Once::run`]'s steps after its
    /// first read, which a [`crate::kv::Fresh`] takes when nothing is kept.
    pub(super) fn build<E: From<Error>>(
        &self,
        key: &str,
        mut found: Option<Cell>,
        work: impl FnOnce() -> Result<V, E>,
    ) -> Result<Answer<V>, E> {
        #[cfg(feature = "replication")]
        if found.is_none() && self.answers.is_copy() {
            return self.run_at_home(key, work).map(made);
        }
        let mut claimed = loop {
            if let Some(cell) = found {
                return Ok(self.answered(key, &cell)?);
            }
            match self.claim(key)? {
                Claim::Yours(claimed) => break claimed,
                Claim::Unkept(raw) => return Ok(made(super::bucket::decode(&raw)?)),
                Claim::Ended => found = self.answers.read_cell(key)?,
            }
        };
        if let Some(cell) = self.answers.read_cell(key)? {
            return Ok(self.answered(key, &cell)?);
        }
        counted(&self.answers.scope, BUILDS);
        let answer = work().inspect_err(|_| counted(&self.answers.scope, FAILURES))?;
        if let Err(error) = self.answers.set(key, &answer) {
            not_kept(&error);
            claimed.unkept = self.answers.encode(key, &answer).ok();
        }
        drop(claimed);
        Ok(made(answer))
    }

    /// A kept cell's answer, how old it is and whether it is stale.
    pub(super) fn answered(&self, key: &str, cell: &Cell) -> Result<Answer<V>> {
        let value =
            super::bucket::decode(&cell.raw).map_err(|error| error.within(self.answers.scope.shown_key(key)))?;
        let (age, stale) = aged(&self.answers, cell, self.fresh);
        Ok(Answer { value, age, stale })
    }

    /// Counts a stale answer answered.
    pub(crate) fn stale_answered(&self) {
        counted(&self.answers.scope, STALE);
    }

    /// The answer kept under `key`, with its age, for the async API's runs.
    #[cfg(feature = "async")]
    pub(crate) fn kept(&self, key: &str) -> Result<Option<Answer<V>>> {
        let found = self.answers.read_cell(key)?;
        found.map(|cell| self.answered(key, &cell)).transpose()
    }

    /// Removes every answer of this branch and of the branches under it, in
    /// one commit, so that the next run of each key runs again. A run under
    /// way meanwhile keeps its answer once it ends.
    pub fn clear(&self) -> Result<()> {
        self.answers.clear()
    }

    /// A run on a copy: its home holds the key, so that one run of it goes
    /// on across every node, and keeps what this one answered.
    #[cfg(feature = "replication")]
    fn run_at_home<E: From<Error>>(&self, key: &str, work: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        let token = match self.claim_at_home(key)? {
            AtHome::Kept(answer) => return Ok(answer),
            AtHome::Yours(token) => token,
        };
        let answer = work();
        let raw = match &answer {
            Ok(value) => Some(self.answers.encode(key, value)?),
            Err(_) => None,
        };
        self.keep_at_home(key, token, raw);
        answer
    }

    /// Claims `key` at the home of the copy these answers are kept in, and
    /// waits for its answer.
    #[cfg(feature = "replication")]
    pub(crate) fn claim_at_home(&self, key: &str) -> Result<AtHome<V>> {
        match self.answers.once_claim(key)? {
            super::forward::Claimed::Kept(raw) => Ok(AtHome::Kept(super::bucket::decode(&raw)?)),
            super::forward::Claimed::Yours(token) => Ok(AtHome::Yours(token)),
        }
    }

    /// Keeps at the home what the run it handed over by `token` answered,
    /// none when its function failed, and lets the key go there; a keep that
    /// failed is logged, and the next run of the key runs again.
    #[cfg(feature = "replication")]
    pub(crate) fn keep_at_home(&self, key: &str, token: u64, raw: Option<Raw>) {
        if let Err(error) = self.answers.once_keep(key, token, raw) {
            tracing::warn!(
                target: "tinystore",
                %error,
                "a once answer was not kept at its home; the next run of its key runs again",
            );
        }
    }

    /// The bucket that keeps the answers, for the async API's calls.
    #[cfg(feature = "async")]
    pub(crate) fn answers(&self) -> &Bucket<V> {
        &self.answers
    }

    /// Claims `key` for an awaited run, which no thread holds: the run handed
    /// over, or none while another run holds the key, `waiter` called once
    /// that run ends. A run of a key whose function this thread runs or polls
    /// now would wait for itself: that is `Invalid`.
    #[cfg(feature = "async")]
    pub(crate) fn claim_then(&self, key: &str, waiter: Waiter) -> Result<Option<Handed>> {
        let running = (self.answers.scope.id, self.answers.scope.path(key)?);
        let claimed = self.answers.scope.kv.runs().claim_or_wait(running.clone(), None, waiter);
        if !claimed.map_err(|error| error.within(self.answers.scope.shown_key(key)))? {
            return Ok(None);
        }
        Ok(Some(Handed::new(self.answers.untyped(), key, running, None)))
    }

    /// Claims `key` for an awaited build in the background, of a kept answer
    /// gone stale: none while a run holds the key or a build of it that
    /// failed holds off the next, and no waiter, since a stale answer waits
    /// for nothing.
    #[cfg(feature = "async")]
    pub(crate) fn claim_refresh(&self, key: &str) -> Result<Option<Handed>> {
        let Some(fresh) = self.fresh else { return Ok(None) };
        let running = (self.answers.scope.id, self.answers.scope.path(key)?);
        if !self.answers.scope.kv.runs().claim_refresh(&running, self.answers.scope.now()) {
            return Ok(None);
        }
        Ok(Some(Handed::new(self.answers.untyped(), key, running, Some(pause_after(fresh)))))
    }

    /// The version of the answer under `key` while it is still stale, read
    /// again once a build in the background claimed the key, as
    /// [`still_stale`] says.
    pub(crate) fn still_stale(&self, key: &str) -> Result<Option<i64>> {
        Ok(still_stale(&self.answers, key, self.fresh)?.map(|cell| cell.version))
    }

    /// The answer a run could not keep, as the runs that waited for it get it.
    #[cfg(feature = "async")]
    pub(crate) fn unkept(&self, raw: &Raw) -> Result<V> {
        super::bucket::decode(raw)
    }

    /// The answer kept under `key`.
    pub fn get(&self, key: impl Key) -> Result<Option<V>> {
        self.answers.get(key)
    }

    /// Forgets the answer kept under `key`, so that the next run runs again;
    /// says whether there was one.
    pub fn delete(&self, key: impl Key) -> Result<bool> {
        self.answers.delete(key)
    }

    /// Claims `key` for this caller, or waits for the run under way to end.
    fn claim(&self, key: &str) -> Result<Claim<'_>> {
        crate::polled::may_wait(|| format!("{}: a run", self.answers.scope.shown_key(key)))?;
        let running = (self.answers.scope.id, self.answers.scope.path(key)?);
        let (ended, waiting) = mpsc::channel();
        let runs = self.answers.scope.kv.runs();
        let wake = Box::new(move |unkept| {
            let _ = ended.send(unkept);
        });
        let claimed = runs.claim_or_wait(running.clone(), Some(thread::current().id()), wake);
        if claimed.map_err(|error| error.within(self.answers.scope.shown_key(key)))? {
            return Ok(Claim::Yours(Claimed { runs, running, unkept: None }));
        }
        // A sender dropped unsent is a run let go as well.
        Ok(match waiting.recv() {
            Ok(Some(raw)) => Claim::Unkept(raw),
            _ => Claim::Ended,
        })
    }
}

impl<V> Clone for Once<V> {
    fn clone(&self) -> Self {
        Once { answers: self.answers.clone(), fresh: self.fresh }
    }
}

/// An answer just made, or one a run made could not keep.
pub(crate) fn made<V>(value: V) -> Answer<V> {
    Answer { value, age: Duration::ZERO, stale: false }
}

impl<V> fmt::Debug for Once<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Once({:?})", self.answers.scope)
    }
}

impl<V> fmt::Debug for OnceBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OnceBuilder({})", self.name)
    }
}

/// What the end of a run calls: one for every caller that waited for it,
/// given the run's answer when it could not be kept, since they waited for
/// that run rather than for the next.
pub(crate) type Waiter = Box<dyn FnOnce(Option<Arc<Raw>>) + Send>;

/// The keys whose function runs now in this process, each with the callers
/// waiting for its end. A waiter is a callback rather than a blocked thread, so
/// a client on an event loop waits without holding one of the store's threads.
#[derive(Default)]
pub(crate) struct Runs {
    running: Mutex<Running>,
    /// The keys whose build in the background failed, each with the time,
    /// in unix milliseconds of the store's clock, until which no other
    /// starts. Kept in memory, and forgotten at a restart.
    paused: Mutex<Pauses>,
}

/// The keys paused, each with when its pause ends, and how many are kept
/// before those ended are swept out at the next: twice what the sweep before
/// left, so that a sweep's cost is paid by as many pauses as it looks at.
#[derive(Default)]
struct Pauses {
    until: HashMap<(i64, Vec<u8>), i64>,
    swept_at: usize,
    #[cfg(test)]
    sweeps: usize,
}

/// The fewest pauses kept before those ended are swept out.
const PAUSES_SWEPT: usize = 1024;

/// The keys running, by their name's id and path.
type Running = HashMap<(i64, Vec<u8>), Run>;

/// A run under way: the thread running it, when a thread does, and who waits
/// for its end.
struct Run {
    thread: Option<ThreadId>,
    waiters: Vec<Waiter>,
}

impl Runs {
    /// Claims `key` for `thread` and says true, or keeps `waiter` for the end
    /// of the run that holds it and says false. A thread asking for a key its
    /// own run holds would wait for itself: that is `Invalid`, whether this
    /// thread runs that run's function or polls it, for an awaited run.
    pub(crate) fn claim_or_wait(&self, key: (i64, Vec<u8>), thread: Option<ThreadId>, waiter: Waiter) -> Result<bool> {
        let itself = || Error::invalid("a run of a key inside its own run would wait for itself");
        if self.polled_here(&key) {
            return Err(itself());
        }
        let mut running = self.lock();
        match running.get_mut(&key) {
            Some(run) if run.thread == Some(thread::current().id()) => Err(itself()),
            Some(run) => {
                run.waiters.push(waiter);
                Ok(false)
            }
            None => {
                running.insert(key, Run { thread, waiters: Vec::new() });
                Ok(true)
            }
        }
    }

    /// Claims `key` for a build in the background and says true, unless a run
    /// holds it or a build of it failed less than its pause before `now`. It
    /// keeps no waiter: whoever asks for a stale answer waits for nothing.
    pub(crate) fn claim_refresh(&self, key: &(i64, Vec<u8>), now: i64) -> bool {
        let mut running = self.lock();
        if running.contains_key(key) {
            return false;
        }
        let mut paused = self.paused();
        if paused.until.get(key).is_some_and(|until| *until > now) {
            return false;
        }
        paused.until.remove(key);
        running.insert(key.clone(), Run { thread: None, waiters: Vec::new() });
        true
    }

    /// Marks `key`'s run as `thread`'s, once a build in the background claimed
    /// it starts there: a run of the key inside it would wait for itself.
    pub(crate) fn running_on(&self, key: &(i64, Vec<u8>), thread: ThreadId) {
        if let Some(run) = self.lock().get_mut(key) {
            run.thread = Some(thread);
        }
    }

    /// Holds off the next build of `key` in the background until `until`.
    pub(crate) fn pause(&self, key: &(i64, Vec<u8>), until: i64, now: i64) {
        let mut paused = self.paused();
        if paused.until.len() >= paused.swept_at.max(PAUSES_SWEPT) {
            paused.until.retain(|_, kept| *kept > now);
            paused.swept_at = 2 * paused.until.len();
            #[cfg(test)]
            {
                paused.sweeps += 1;
            }
        }
        paused.until.insert(key.clone(), until);
    }

    fn paused(&self) -> MutexGuard<'_, Pauses> {
        self.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many pauses are kept, and how many sweeps looked at them.
    #[cfg(test)]
    pub(crate) fn pauses(&self) -> (usize, usize) {
        let paused = self.paused();
        (paused.until.len(), paused.sweeps)
    }

    /// Lets go of `key` and calls everyone who waited for it, outside the lock,
    /// with the answer the run could not keep.
    pub(crate) fn release(&self, key: &(i64, Vec<u8>), unkept: Option<Raw>) {
        let run = self.lock().remove(key);
        let unkept = unkept.map(Arc::new);
        for waiter in run.map(|run| run.waiters).unwrap_or_default() {
            waiter(unkept.clone());
        }
    }

    /// How many keys run now, and how many callers wait for them.
    #[cfg(test)]
    pub(crate) fn under_way(&self) -> (usize, usize) {
        let running = self.lock();
        (running.len(), running.values().map(|run| run.waiters.len()).sum())
    }

    /// Whether this thread polls the function of `key`'s awaited run now.
    #[cfg(feature = "async")]
    fn polled_here(&self, key: &(i64, Vec<u8>)) -> bool {
        let runs = std::ptr::from_ref(self).addr();
        POLLING.with(|polling| polling.borrow().iter().any(|(held, polled)| *held == runs && polled == key))
    }

    /// Whether this thread polls an awaited run's function: never, in a
    /// library built without the async API.
    #[cfg(not(feature = "async"))]
    fn polled_here(&self, _key: &(i64, Vec<u8>)) -> bool {
        false
    }

    fn lock(&self) -> MutexGuard<'_, Running> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for Runs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Runs({} running)", self.lock().len())
    }
}

/// How a caller's claim of a key ended.
enum Claim<'a> {
    Yours(Claimed<'a>),
    /// The run waited for ended with an answer it could not keep.
    Unkept(Arc<Raw>),
    /// The run waited for ended; what it kept, if anything, is in the bucket.
    Ended,
}

/// A key this caller runs; dropping it, a panic's unwinding included, lets the
/// callers waiting for it go on.
struct Claimed<'a> {
    runs: &'a Runs,
    running: (i64, Vec<u8>),
    /// The answer when it could not be kept, for the callers waiting.
    unkept: Option<Raw>,
}

impl Drop for Claimed<'_> {
    fn drop(&mut self) {
        self.runs.release(&self.running, self.unkept.take());
    }
}

/// Says that a run's answer was not kept: the callers that waited for it have
/// it, and the next run of its key runs again.
pub(super) fn not_kept(error: &Error) {
    tracing::warn!(
        target: "tinystore",
        %error,
        "a once answer was not kept; the callers that waited have it, and the next run of its key runs again",
    );
}

#[cfg(test)]
#[path = "once_tests.rs"]
mod tests;
