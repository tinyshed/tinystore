//! What the store measures of itself, plan/self-metrics.md: each file tells
//! the store's probe of its commits, and its periodic work of its failures.
//! Nobody listens unless the store was opened with `self_metrics`, and then
//! it is the metrics engine, which no file names: the probe is the root's,
//! so that the SQLite under every engine imports no engine.
//!
//! A commit's report costs a store without a listener one load of an empty
//! cell; with one, the listener's instruments change numbers in memory.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
#[cfg(feature = "metrics")]
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::sqlite::Readers;

/// What hears the store's measurements.
pub(crate) trait Listen: Send + Sync {
    /// A commit of `file`'s writer: the writes it carried, how long its
    /// `commit` took, its sync included, and how long each write waited for
    /// the writer.
    fn committed(&self, file: &str, writes: usize, took: Duration, waits: &[Duration]);
    /// The store's periodic work named `task` failed.
    fn failed(&self, task: &str);
}

/// The store's one probe, shared by its files and its scheduler.
#[derive(Default)]
pub(crate) struct Probe {
    listener: OnceLock<Arc<dyn Listen>>,
    #[cfg(feature = "metrics")]
    files: Mutex<Vec<Opened>>,
}

/// A file the probe was told of, whose size and readers the listener reads
/// while its readers are held, which is while it is open.
#[cfg(feature = "metrics")]
struct Opened {
    file: String,
    path: PathBuf,
    readers: Weak<Readers>,
}

/// A file open now, as the listener reads it.
#[cfg(feature = "metrics")]
pub(crate) struct OpenFile {
    pub(crate) file: String,
    pub(crate) path: PathBuf,
    pub(crate) readers: usize,
}

impl Probe {
    /// Makes `listener` hear every measurement from now on; a second is
    /// ignored, as the store has one metrics engine.
    #[cfg(feature = "metrics")]
    pub(crate) fn listen(&self, listener: Arc<dyn Listen>) {
        let _ = self.listener.set(listener);
    }

    pub(crate) fn listening(&self) -> bool {
        self.listener.get().is_some()
    }

    /// What a file of the store reports through: `file` is its name as a
    /// label, `kv` or `sql/app`.
    pub(crate) fn for_file(self: &Arc<Self>, file: String) -> FileProbe {
        FileProbe { probe: Arc::clone(self), file }
    }

    pub(crate) fn failed(&self, task: &str) {
        if let Some(listener) = self.listener.get() {
            listener.failed(task);
        }
    }

    /// The files open now, those closed since the last call left out.
    #[cfg(feature = "metrics")]
    pub(crate) fn files(&self) -> Vec<OpenFile> {
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        files.retain(|opened| opened.readers.strong_count() > 0);
        let open = |opened: &Opened| {
            let readers = opened.readers.upgrade()?.open();
            Some(OpenFile { file: opened.file.clone(), path: opened.path.clone(), readers })
        };
        files.iter().filter_map(open).collect()
    }
}

/// A file's way to the store's probe.
#[derive(Clone)]
pub(crate) struct FileProbe {
    probe: Arc<Probe>,
    file: String,
}

impl std::fmt::Debug for FileProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FileProbe({})", self.file)
    }
}

impl FileProbe {
    /// When a write that comes now is measured, its queueing's time: none
    /// without a listener, or inside [`unmeasured`].
    pub(crate) fn queued(&self) -> Option<Instant> {
        (self.probe.listening() && !UNMEASURED.get()).then(Instant::now)
    }

    /// Tells the listener of a commit whose measured writes waited `waits`.
    pub(crate) fn committed(&self, took: Duration, waits: &[Duration]) {
        if let (Some(listener), false) = (self.probe.listener.get(), waits.is_empty()) {
            listener.committed(&self.file, waits.len(), took, waits);
        }
    }

    /// Tells the probe the file is open at `path` with `readers`; only the
    /// metrics engine reads them.
    #[cfg_attr(not(feature = "metrics"), expect(unused_variables, reason = "nothing reads a file's sizes"))]
    pub(crate) fn opened(&self, path: PathBuf, readers: Weak<Readers>) {
        #[cfg(feature = "metrics")]
        let mut files = self.probe.files.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(feature = "metrics")]
        files.push(Opened { file: self.file.clone(), path, readers });
    }
}

thread_local! {
    static UNMEASURED: Cell<bool> = const { Cell::new(false) };
}

/// Runs `work` unmeasured: the metrics engine's writes of what it measured,
/// which would otherwise measure themselves for ever.
#[cfg(feature = "metrics")]
pub(crate) fn unmeasured<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNMEASURED.set(self.0);
        }
    }
    let _restore = Restore(UNMEASURED.replace(true));
    work()
}

/// A file's name in the store's directory as its label: `kv.db` is `kv`,
/// `sql/app.db` is `sql/app`.
pub(crate) fn label(name: &str) -> String {
    name.strip_suffix(".db").unwrap_or(name).replace('\\', "/")
}
