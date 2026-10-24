//! An index as the engine holds it while the store is open: the rows it
//! serves searches from, and the rows a rebuild makes beside them, which a
//! delete reaches too and a set through the rebuild's copy, until the
//! rebuild switches them in.
//!
//! ```text
//! set ──► serving ── a rebuild's copy, in the order of numbers ──► beside
//! delete ──► serving and beside, one commit      switch: beside serves, the old rows dropped
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};
use std::time::{Duration, Instant};

use super::kept::Kept;
use super::purge::Purge;
use super::settings::Settings;
use crate::sqlite::File;
#[cfg(feature = "async")]
use crate::sync::lock;
use crate::{Error, Result};

/// An index's rows in search.db as the engine works on them: their id, the
/// settings they are read by, what searches keep of them, and a purge under
/// way. A change of settings that applies at once keeps the rest.
#[derive(Clone)]
pub(crate) struct Data {
    pub(crate) id: i64,
    pub(crate) settings: Arc<Settings>,
    pub(crate) kept: Arc<Kept>,
    pub(crate) purge: Arc<Mutex<Option<Purge>>>,
}

impl std::fmt::Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Data").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Data {
    pub(crate) fn new(id: i64, settings: Settings) -> Data {
        Data { id, settings: Arc::new(settings), kept: Arc::default(), purge: Arc::default() }
    }

    /// The same rows read by other settings.
    pub(crate) fn with(&self, settings: Settings) -> Data {
        Data { settings: Arc::new(settings), ..self.clone() }
    }
}

/// What an index serves now, and the rows a rebuild makes beside them; a
/// rebuild that failed writes beside them no longer.
#[derive(Clone, Debug)]
pub(crate) struct Current {
    pub(crate) serving: Data,
    pub(crate) beside: Option<Beside>,
}

#[derive(Clone, Debug)]
pub(crate) struct Beside {
    pub(crate) data: Data,
    pub(crate) failed: bool,
}

impl Current {
    /// The rows maintenance keeps and a delete reaches: the serving ones,
    /// and those beside them while their rebuild goes on.
    pub(crate) fn written(&self) -> impl Iterator<Item = &Data> {
        let beside = self.beside.as_ref().filter(|beside| !beside.failed).map(|beside| &beside.data);
        std::iter::once(&self.serving).chain(beside)
    }
}

/// Where an index stands: serving the settings it was opened with, read
/// again beside them, or a rebuild that stopped.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Ready,
    /// The share of the documents the rebuild has read.
    Rebuilding {
        done: f64,
    },
    Failed {
        error: String,
    },
}

/// An index open in this process.
pub(crate) struct OpenIndex {
    pub(crate) name: String,
    /// Read by a write from its analysis to its commit, so that neither a
    /// change of settings nor a switch lands between them.
    current: RwLock<Arc<Current>>,
    /// The awaited writes submitted under the rows, which a change waits for
    /// as for a blocking write's read of `current`: such a write commits on
    /// another thread than the one that submitted it, where no guard of the
    /// lock can go.
    #[cfg(feature = "async")]
    submitted: (Mutex<Submissions>, Condvar),
    /// Sets since its head was last packed whole.
    pub(crate) sets: AtomicU64,
    /// Whether a change that needs the index read again waits for `reindex`.
    pub(crate) manual: AtomicBool,
    dropped: AtomicBool,
    /// When this process began the rebuild under way, to say how long it took.
    pub(crate) began: Mutex<Option<Instant>>,
    /// Woken when a rebuild switches or fails, for `ready`.
    settled: (Mutex<()>, Condvar),
    /// The awaited readies, each answered at the next settle.
    #[cfg(feature = "async")]
    settles: Mutex<Vec<Settled>>,
}

impl std::fmt::Debug for OpenIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenIndex").field("name", &self.name).finish_non_exhaustive()
    }
}

impl OpenIndex {
    pub(crate) fn new(name: &str, current: Current) -> OpenIndex {
        OpenIndex {
            name: name.to_owned(),
            current: RwLock::new(Arc::new(current)),
            #[cfg(feature = "async")]
            submitted: (Mutex::default(), Condvar::new()),
            sets: AtomicU64::new(0),
            manual: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            began: Mutex::new(None),
            settled: (Mutex::new(()), Condvar::new()),
            #[cfg(feature = "async")]
            settles: Mutex::default(),
        }
    }

    pub(crate) fn current(&self) -> Arc<Current> {
        Arc::clone(&self.hold())
    }

    /// The current rows, held still until the guard drops.
    pub(crate) fn hold(&self) -> RwLockReadGuard<'_, Arc<Current>> {
        self.current.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current rows, to be changed: no write runs meanwhile. The awaited
    /// writes submitted under them commit first, in `file`, and none is
    /// submitted while the change waits for them.
    ///
    /// It waits for them before it takes the rows, since their commits run
    /// on the store's pool, whose threads would otherwise wait for the rows
    /// it took: sixteen searches did, and the commit it waited for never ran.
    /// A commit whose lead no thread of the pool took yet it leads itself:
    /// every thread may be busy with what waits for this change, as passes
    /// of maintenance wait for the one whose switch it is.
    pub(crate) fn change(&self, file: &File) -> RwLockWriteGuard<'_, Arc<Current>> {
        #[cfg(feature = "async")]
        let _waited = self.wait_for_the_submitted(file);
        #[cfg(not(feature = "async"))]
        let _ = file; // without the async API no write is submitted under the rows
        self.current.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until no write submitted under the rows is left to commit, a
    /// commit that no thread leads yet led here, and keeps new ones from
    /// being submitted until what it gives back is dropped.
    #[cfg(feature = "async")]
    fn wait_for_the_submitted(&self, file: &File) -> Changing<'_> {
        lock(&self.submitted.0).changes += 1;
        let changing = Changing(self);
        let mut submitted = lock(&self.submitted.0);
        while submitted.writes > 0 {
            let queued = submitted.queued;
            // Not held while it leads, since each commit it answers counts its write out.
            drop(submitted);
            file.lead_handed();
            submitted = lock(&self.submitted.0);
            if submitted.writes > 0 && submitted.queued == queued {
                submitted = self.submitted.1.wait(submitted).unwrap_or_else(PoisonError::into_inner);
            }
        }
        changing
    }

    /// The current rows held still for a write submitted under them, as
    /// [`hold`](Self::hold) holds them, until the hold is dropped where the
    /// write commits; none while a change holds them or waits to, since the
    /// thread that polls an awaited write must not wait for it.
    #[cfg(feature = "async")]
    pub(crate) fn try_hold_submitted(self: &Arc<Self>) -> Option<Submitted> {
        let mut submitted = lock(&self.submitted.0);
        if submitted.changes > 0 {
            return None;
        }
        let held = match self.current.try_read() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        submitted.writes += 1;
        Some(Submitted { current: Arc::clone(&held), open: Arc::clone(self) })
    }

    /// Says that a write submitted under the rows is queued for its commit,
    /// its lead handed to the pool if it took one, so that a change waiting
    /// for it looks again for a lead to take.
    #[cfg(feature = "async")]
    pub(crate) fn queued(&self) {
        let mut submitted = lock(&self.submitted.0);
        submitted.queued = submitted.queued.wrapping_add(1);
        if submitted.changes > 0 {
            self.submitted.1.notify_all();
        }
    }

    /// What searches keep of the serving rows `id`, for a search on the
    /// writer of a transaction to start from; nothing while a change holds
    /// the rows, since the change may be waiting for that very writer.
    #[cfg(feature = "sql")]
    pub(crate) fn kept_for_a_transaction(&self, id: i64) -> Kept {
        let current = match self.current.try_read() {
            Ok(held) => Arc::clone(&held),
            Err(TryLockError::Poisoned(poisoned)) => Arc::clone(&poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => return Kept::default(),
        };
        if current.serving.id != id {
            return Kept::default();
        }
        current.serving.kept.committed()
    }

    /// The serving rows' id.
    pub(crate) fn id(&self) -> i64 {
        self.hold().serving.id
    }

    /// The serving rows' id for a read, which waits for a change holding the
    /// rows only where its thread may wait: a point read tried in an awaited
    /// call's poll is refused, and runs again on the store's pool.
    pub(crate) fn serving_id(&self) -> Result<i64> {
        match self.current.try_read() {
            Ok(held) => return Ok(held.serving.id),
            Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner().serving.id),
            Err(TryLockError::WouldBlock) => {}
        }
        crate::polled::may_wait(|| format!("index {}: its rows, which a change holds", self.name))?;
        Ok(self.id())
    }

    /// The serving rows' settings.
    #[cfg(test)]
    pub(crate) fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.hold().serving.settings)
    }

    /// Whether a change holds the rows, or waits to.
    #[cfg(all(test, feature = "sql"))]
    pub(crate) fn changing(&self) -> bool {
        matches!(self.current.try_read(), Err(TryLockError::WouldBlock))
    }

    pub(crate) fn refuse_dropped(&self) -> Result<()> {
        if self.dropped.load(Ordering::Acquire) {
            return Err(Error::invalid("the index was dropped"));
        }
        Ok(())
    }

    pub(crate) fn mark_dropped(&self) {
        self.dropped.store(true, Ordering::Release);
        self.settle();
    }

    /// Wakes whoever waits for the index to be ready.
    pub(crate) fn settle(&self) {
        let held = self.settled.0.lock().unwrap_or_else(PoisonError::into_inner);
        self.settled.1.notify_all();
        drop(held);
        #[cfg(feature = "async")]
        {
            // Not answered under the lock: a ready polled at once as it is woken waits again here.
            let waiting = std::mem::take(&mut *lock(&self.settles));
            for settled in waiting {
                settled.answer(Ok(()));
            }
        }
    }

    /// Answers `settled` at the next settle. The waits answered otherwise
    /// meanwhile, by the store's timer, are let go here.
    #[cfg(feature = "async")]
    pub(crate) fn on_settle(&self, settled: Settled) {
        let mut settles = lock(&self.settles);
        settles.retain(|waiting| !waiting.answered());
        settles.push(settled);
    }

    /// Waits up to `span` for a rebuild to switch or fail.
    pub(crate) fn wait_settled(&self, span: Duration) {
        let held = self.settled.0.lock().unwrap_or_else(PoisonError::into_inner);
        drop(self.settled.1.wait_timeout(held, span).unwrap_or_else(PoisonError::into_inner));
    }
}

/// The awaited writes under an index's rows, and the changes that wait for
/// them.
#[cfg(feature = "async")]
#[derive(Default)]
struct Submissions {
    /// Submitted and not committed yet.
    writes: usize,
    /// Waiting for those writes to commit, or for the rows once they did: no
    /// write is submitted meanwhile.
    changes: usize,
    /// How many writes were queued for their commits, for a waiting change
    /// to see that one came since it last looked.
    queued: u64,
}

/// A change waiting to take the rows, which no write is submitted under
/// until it is dropped, once it holds them.
#[cfg(feature = "async")]
struct Changing<'o>(&'o OpenIndex);

#[cfg(feature = "async")]
impl Drop for Changing<'_> {
    fn drop(&mut self) {
        lock(&self.0.submitted.0).changes -= 1;
    }
}

/// The rows a submitted write was made by, held still until it commits.
#[cfg(feature = "async")]
pub(crate) struct Submitted {
    open: Arc<OpenIndex>,
    current: Arc<Current>,
}

#[cfg(feature = "async")]
impl Submitted {
    pub(crate) fn current(&self) -> &Current {
        &self.current
    }

    pub(crate) fn open(&self) -> &Arc<OpenIndex> {
        &self.open
    }
}

#[cfg(feature = "async")]
impl Drop for Submitted {
    fn drop(&mut self) {
        let mut submitted = lock(&self.open.submitted.0);
        submitted.writes -= 1;
        if submitted.writes == 0 {
            self.open.submitted.1.notify_all();
        }
    }
}

/// An awaited `ready`'s wait for the next settle, which a settle and the
/// store's timer may both answer: the first of them alone is heard.
#[cfg(feature = "async")]
#[derive(Clone)]
pub(crate) struct Settled(Arc<Mutex<Option<Waiting>>>);

#[cfg(feature = "async")]
type Waiting = Box<dyn FnOnce(Result<()>) + Send>;

#[cfg(feature = "async")]
impl Settled {
    pub(crate) fn new(done: impl FnOnce(Result<()>) + Send + 'static) -> Settled {
        Settled(Arc::new(Mutex::new(Some(Box::new(done)))))
    }

    pub(crate) fn answer(&self, answer: Result<()>) {
        let waiting = lock(&self.0).take();
        if let Some(done) = waiting {
            done(answer);
        }
    }

    fn answered(&self) -> bool {
        lock(&self.0).is_none()
    }
}

#[cfg(all(test, feature = "async"))]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::search::Index;
    use crate::search::fixture::fixture;
    use crate::search::settings::Wanted;

    // contract: SRCH-06
    #[test]
    fn a_change_waits_for_the_writes_submitted_under_the_rows_to_commit() {
        let fixture = fixture();
        let index: Index<serde_json::Value> = fixture.articles("articles");
        let file = Arc::clone(&index.search.file);
        let serving = Data::new(1, Settings::new(&Wanted::default()));
        let open = Arc::new(OpenIndex::new("articles", Current { serving, beside: None }));
        let submitted = open.try_hold_submitted().expect("no change holds the rows");
        let (changed, change) = mpsc::channel();
        let changing = {
            let open = Arc::clone(&open);
            thread::spawn(move || {
                let held = open.change(&file);
                changed.send(()).unwrap();
                drop(held);
            })
        };
        let early = change.recv_timeout(Duration::from_millis(200));
        assert!(early.is_err(), "the change went on while a write submitted under the rows had not committed");
        assert!(open.try_hold_submitted().is_none(), "a change waits: no write is submitted under the rows meanwhile");
        assert!(open.current.try_read().is_ok(), "a change that waits for the writes holds no rows: reads go on");
        drop(submitted); // as the write commits
        change.recv_timeout(Duration::from_secs(10)).expect("the change never went on");
        changing.join().unwrap();

        let held = open.change(&index.search.file);
        assert!(open.try_hold_submitted().is_none(), "a change holds the rows: no write is submitted under them");
        drop(held);
        assert!(open.try_hold_submitted().is_some());
    }

    #[test]
    fn a_ready_woken_by_a_settle_waits_again_on_the_thread_that_woke_it() {
        let serving = Data::new(1, Settings::new(&Wanted::default()));
        let open = Arc::new(OpenIndex::new("articles", Current { serving, beside: None }));
        let (waited, again) = mpsc::channel();
        let woken = Arc::clone(&open);
        // as an executor does that polls the woken ready at once, inside its wake
        open.on_settle(Settled::new(move |_| {
            woken.on_settle(Settled::new(drop));
            waited.send(()).unwrap();
        }));
        let settling = {
            let open = Arc::clone(&open);
            thread::spawn(move || open.settle())
        };
        again.recv_timeout(Duration::from_secs(10)).expect("the settle answered its waiters under their lock");
        settling.join().unwrap();
    }
}
