use std::collections::HashMap;
use std::fmt;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use super::bucket::{Bucket, Expiry, Put, WriteOptions};
use super::path::Key;
use super::scope::{Kind, Scope};
use super::value::{Raw, Value};
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
        if self.keep.is_zero() {
            return Err(Error::invalid("a keep of zero").within(format!("kv once {}", self.name)));
        }
        let scope = Scope::open(&self.store, &self.name, Kind::Once)?;
        Ok(Once { answers: Bucket::new(scope, Expiry::Ttl(self.keep)) })
    }
}

/// Once's answers as rows, for the wire: a client runs the function, and the
/// store keeps what it answered under the key for `keep`.
pub(crate) fn rows(store: &Store, name: &str, keep: Option<Duration>) -> Result<Bucket<()>> {
    let keep = keep.unwrap_or(KEEP);
    if keep.is_zero() {
        return Err(Error::invalid("a keep of zero").within(format!("kv once {name}")));
    }
    let scope = Scope::open(store, name, Kind::Once)?;
    Ok(Bucket::new(scope, Expiry::Ttl(keep)))
}

/// What a client's run of a key starts with.
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
pub(crate) fn hand(answers: &Bucket<()>, key: &str, waiter: Waiter) -> Result<Hand> {
    if let Some(cell) = answers.read_cell(key)? {
        return Ok(Hand::Kept(cell.raw));
    }
    let running = (answers.scope.id, answers.scope.path(key)?);
    if !answers.scope.kv.runs().claim_or_wait(running.clone(), None, waiter)? {
        return Ok(Hand::Wait);
    }
    let handed = Handed { answers: answers.clone(), key: key.to_owned(), running };
    match answers.read_cell(key)? {
        Some(cell) => Ok(Hand::Kept(cell.raw)),
        None => Ok(Hand::Yours(handed)),
    }
}

/// A run of a key a client was handed; dropping it lets the runs waiting for
/// the key go on, keeping nothing.
pub(crate) struct Handed {
    answers: Bucket<()>,
    key: String,
    running: (i64, Vec<u8>),
}

impl Handed {
    /// Keeps the client's answer, or nothing when its function failed, and
    /// lets the key go.
    pub(crate) fn keep(self, answer: Option<Raw>) -> Result<()> {
        match answer {
            Some(raw) => self.answers.put_raw(&self.key, raw, WriteOptions::default(), Put::Always).map(|_| ()),
            None => Ok(()),
        }
    }
}

impl Drop for Handed {
    fn drop(&mut self) {
        self.answers.scope.kv.runs().release(&self.running);
    }
}

impl fmt::Debug for Handed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handed({})", self.answers.scope.shown_key(&self.key))
    }
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
    /// kept is still returned, and the failure logged.
    pub fn run<E: From<Error>>(&self, key: impl Key, work: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        let key = key.text();
        let claimed = loop {
            if let Some(answer) = self.answers.get(&*key)? {
                return Ok(answer);
            }
            match self.claim(&key)? {
                Some(claim) => break claim,
                None => continue,
            }
        };
        if let Some(answer) = self.answers.get(&*key)? {
            return Ok(answer);
        }
        let answer = work()?;
        if let Err(error) = self.answers.set(&*key, &answer) {
            tracing::warn!(target: "tinystore", %error, "a once answer was not kept; the next run of its key runs again");
        }
        drop(claimed);
        Ok(answer)
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

    /// Claims `key` for this caller, or waits for the run under way to end and
    /// says so with `None`.
    fn claim(&self, key: &str) -> Result<Option<Claimed<'_>>> {
        let running = (self.answers.scope.id, self.answers.scope.path(key)?);
        let (ended, waiting) = mpsc::channel();
        let runs = self.answers.scope.kv.runs();
        let wake = Box::new(move || {
            let _ = ended.send(());
        });
        let claimed = runs.claim_or_wait(running.clone(), Some(thread::current().id()), wake);
        if claimed.map_err(|error| error.within(self.answers.scope.shown_key(key)))? {
            return Ok(Some(Claimed { runs, running }));
        }
        // A sender dropped unsent is a run let go as well.
        let _ = waiting.recv();
        Ok(None)
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

/// What the end of a run calls: one for every caller that waited for it.
pub(crate) type Waiter = Box<dyn FnOnce() + Send>;

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
    /// own run holds would wait for itself: that is `Invalid`.
    pub(crate) fn claim_or_wait(&self, key: (i64, Vec<u8>), thread: Option<ThreadId>, waiter: Waiter) -> Result<bool> {
        let mut running = self.lock();
        match running.get_mut(&key) {
            Some(run) if thread.is_some() && run.thread == thread => {
                Err(Error::invalid("a run of a key inside its own run would wait for itself"))
            }
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

    /// Lets go of `key` and calls everyone who waited for it, outside the lock.
    pub(crate) fn release(&self, key: &(i64, Vec<u8>)) {
        let run = self.lock().remove(key);
        for waiter in run.map(|run| run.waiters).unwrap_or_default() {
            waiter();
        }
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

/// A key this caller runs; dropping it, a panic's unwinding included, lets the
/// callers waiting for it go on.
struct Claimed<'a> {
    runs: &'a Runs,
    running: (i64, Vec<u8>),
}

impl Drop for Claimed<'_> {
    fn drop(&mut self) {
        self.runs.release(&self.running);
    }
}

#[cfg(test)]
#[path = "once_tests.rs"]
mod tests;
