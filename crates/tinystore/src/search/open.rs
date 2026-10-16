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
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use super::kept::Kept;
use super::purge::Purge;
use super::settings::Settings;
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
    /// Sets since its head was last packed whole.
    pub(crate) sets: AtomicU64,
    /// Whether a change that needs the index read again waits for `reindex`.
    pub(crate) manual: AtomicBool,
    dropped: AtomicBool,
    /// When this process began the rebuild under way, to say how long it took.
    pub(crate) began: Mutex<Option<Instant>>,
    /// Woken when a rebuild switches or fails, for `ready`.
    settled: (Mutex<()>, Condvar),
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
            sets: AtomicU64::new(0),
            manual: AtomicBool::new(false),
            dropped: AtomicBool::new(false),
            began: Mutex::new(None),
            settled: (Mutex::new(()), Condvar::new()),
        }
    }

    pub(crate) fn current(&self) -> Arc<Current> {
        Arc::clone(&self.hold())
    }

    /// The current rows, held still until the guard drops.
    pub(crate) fn hold(&self) -> RwLockReadGuard<'_, Arc<Current>> {
        self.current.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The current rows, to be changed: no write runs meanwhile.
    pub(crate) fn change(&self) -> RwLockWriteGuard<'_, Arc<Current>> {
        self.current.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// The serving rows' id.
    pub(crate) fn id(&self) -> i64 {
        self.hold().serving.id
    }

    /// The serving rows' settings.
    #[cfg(test)]
    pub(crate) fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.hold().serving.settings)
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
        let _held = self.settled.0.lock().unwrap_or_else(PoisonError::into_inner);
        self.settled.1.notify_all();
    }

    /// Waits up to `span` for a rebuild to switch or fail.
    pub(crate) fn wait_settled(&self, span: Duration) {
        let held = self.settled.0.lock().unwrap_or_else(PoisonError::into_inner);
        drop(self.settled.1.wait_timeout(held, span).unwrap_or_else(PoisonError::into_inner));
    }
}
