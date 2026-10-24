use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::bucket::{Bucket, Expiry};
#[cfg(any(feature = "wire", feature = "replication", feature = "async"))]
use super::bucket::{Put, WriteOptions};
use super::path::Key;
use super::scope::{Kind, Scope};
use super::value::{Raw, Value};
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

    pub fn open(self) -> Result<Once<V>> {
        Ok(Once { answers: answers(&self.store, &self.name, self.keep)? })
    }
}

/// Once's answers as rows, for the wire: a client runs the function, and the
/// store keeps what it answered under the key for `keep`.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) fn rows(store: &Store, name: &str, keep: Option<Duration>) -> Result<Bucket<()>> {
    answers(store, name, keep.unwrap_or(KEEP))
}

/// The bucket that keeps once's answers of a name, each for `keep`.
fn answers<T>(store: &Store, name: &str, keep: Duration) -> Result<Bucket<T>> {
    if keep.is_zero() {
        return Err(Error::invalid("a keep of zero").within(format!("kv once {name}")));
    }
    let scope = Scope::open(&Home::Store(store.clone()), name, Kind::Once)?;
    Ok(Bucket::new(scope, Expiry::Ttl(keep)))
}

/// What a client's run of a key starts with.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) enum Hand {
    /// The answer kept under the key.
    Kept(Raw),
    /// The run is the client's: it runs its function and keeps what it answers.
    Yours(Handed),
    /// Another run holds the key; the waiter is called once it ends.
    Wait,
}

/// Starts a client's run of `key` on once's rows: the answer kept, or the run
/// handed over, or, while another run holds the key, a wait that calls `waiter`
/// once it ends, when the client asks again.
#[cfg(any(feature = "wire", feature = "replication"))]
pub(crate) fn hand(answers: &Bucket<()>, key: &str, waiter: Waiter) -> Result<Hand> {
    if let Some(cell) = answers.read_cell(key)? {
        return Ok(Hand::Kept(cell.raw));
    }
    let running = (answers.scope.id, answers.scope.path(key)?);
    if !answers.scope.kv.runs().claim_or_wait(running.clone(), None, waiter)? {
        return Ok(Hand::Wait);
    }
    let handed = Handed { answers: answers.clone(), key: key.to_owned(), running, unkept: None };
    match answers.read_cell(key)? {
        Some(cell) => Ok(Hand::Kept(cell.raw)),
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
}

#[cfg(any(feature = "wire", feature = "replication"))]
impl Handed {
    /// Keeps the client's answer, or nothing when its function failed, and
    /// lets the key go. An answer that could not be kept goes to the runs that
    /// waited for this one, as [`Once::run`]'s does, and the failure is logged.
    pub(crate) fn keep(mut self, answer: Option<Raw>) {
        let Some(raw) = answer else { return };
        if let Err(error) = self.answers.put_raw(&self.key, raw.clone(), WriteOptions::default(), Put::Always) {
            not_kept(&error);
            self.unkept = Some(Box::new(raw));
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
        let (answers, key) = (self.answers.clone(), self.key.clone());
        answers.put_raw_then(&key, (raw.clone(), WriteOptions::default(), Put::Always), move |kept| {
            if let Err(error) = kept {
                not_kept(&error);
                self.unkept = Some(Box::new(raw));
            }
            drop(self);
            done();
        });
    }

    /// Lets the key go, nothing kept, for an answer that does not encode.
    pub(crate) fn not_kept(self, error: &Error) {
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
    answers: Bucket<V>,
}

impl<V: Value> Once<V> {
    /// The same answers seen from the branch `owner` names under these.
    pub fn under(&self, owner: impl Key) -> Once<V> {
        Once { answers: self.answers.under(owner) }
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
        let key = key.text();
        #[cfg(feature = "replication")]
        if self.answers.is_copy() {
            return self.run_at_home(&key, work);
        }
        let mut claimed = loop {
            if let Some(answer) = self.answers.get(&*key)? {
                return Ok(answer);
            }
            match self.claim(&key)? {
                Claim::Yours(claimed) => break claimed,
                Claim::Unkept(raw) => return Ok(super::bucket::decode(&raw)?),
                Claim::Ended => continue,
            }
        };
        if let Some(answer) = self.answers.get(&*key)? {
            return Ok(answer);
        }
        let answer = work()?;
        if let Err(error) = self.answers.set(&*key, &answer) {
            not_kept(&error);
            claimed.unkept = self.answers.encode(&key, &answer).ok();
        }
        drop(claimed);
        Ok(answer)
    }

    /// A run on a copy: its home holds the key, so that one run of it goes
    /// on across every node, and keeps what this one answered.
    #[cfg(feature = "replication")]
    fn run_at_home<E: From<Error>>(&self, key: &str, work: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        if let Some(answer) = self.answers.get(key)? {
            return Ok(answer);
        }
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
        Ok(Some(Handed { answers: self.answers.untyped(), key: key.to_owned(), running, unkept: None }))
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
        Once { answers: self.answers.clone() }
    }
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
}

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
    #[cfg(all(test, feature = "async"))]
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
fn not_kept(error: &Error) {
    tracing::warn!(
        target: "tinystore",
        %error,
        "a once answer was not kept; the callers that waited have it, and the next run of its key runs again",
    );
}

#[cfg(test)]
#[path = "once_tests.rs"]
mod tests;
