//! Research-only adapter seams, not a supported replication API.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::Connection;

use crate::{Error, Result};

/// Calls run under the file writer. No network waits belong here.
pub trait Observer: Debug + Send + Sync + 'static {
    fn opened(&self, path: &Path, connection: &Connection) -> Result<()>;
    fn committed(&self, path: &Path, connection: &Connection, frames: u32) -> Result<bool>;
    fn closing(&self, path: &Path, connection: &Connection) -> Result<()>;
    fn checkpointed(&self, _path: &Path, _connection: &Connection) -> Result<()> {
        Ok(())
    }
}

type Observers = BTreeMap<PathBuf, Arc<dyn Observer>>;

fn registry() -> &'static Mutex<Observers> {
    static REGISTRY: OnceLock<Mutex<Observers>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Register before opening engines in this directory. Dropping this handle
/// affects future opens; already open files retain their observer.
#[derive(Debug)]
pub struct Registration(PathBuf);

pub fn register(root: &Path, observer: Arc<dyn Observer>) -> Result<Registration> {
    std::fs::create_dir_all(root).map_err(|e| Error::io("capture directory", e))?;
    let root = root.canonicalize().map_err(|e| Error::io("capture directory", e))?;
    let mut entries =
        registry().lock().map_err(|_| Error::new(crate::ErrorKind::Unavailable, "capture registry poisoned"))?;
    if entries.keys().any(|other| root.starts_with(other) || other.starts_with(&root)) {
        return Err(Error::invalid("capture directories overlap"));
    }
    entries.insert(root.clone(), observer);
    Ok(Registration(root))
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut entries) = registry().lock() {
            entries.remove(&self.0);
        }
    }
}

pub(crate) fn observer(path: &Path) -> Result<Option<Arc<dyn Observer>>> {
    let parent = path.parent().ok_or_else(|| Error::invalid("capture file has no parent"))?;
    let path = parent
        .canonicalize()
        .map_err(|e| Error::io("capture parent", e))?
        .join(path.file_name().ok_or_else(|| Error::invalid("capture file has no name"))?);
    let entries =
        registry().lock().map_err(|_| Error::new(crate::ErrorKind::Unavailable, "capture registry poisoned"))?;
    Ok(entries.iter().find(|(root, _)| path.starts_with(root)).map(|(_, value)| Arc::clone(value)))
}

#[cfg(all(test, feature = "sql"))]
mod tests {
    use super::*;
    use crate::{ErrorKind, Options, Store};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct Counter {
        commits: AtomicUsize,
        fail: AtomicBool,
    }
    impl Observer for Counter {
        fn opened(&self, _: &Path, _: &Connection) -> Result<()> {
            Ok(())
        }
        fn committed(&self, _: &Path, _: &Connection, frames: u32) -> Result<bool> {
            assert!(frames > 0);
            self.commits.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(Error::new(ErrorKind::Unavailable, "injected capture failure"));
            }
            Ok(false)
        }
        fn closing(&self, _: &Path, _: &Connection) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn capture_runs_before_ack_and_failure_stops_further_writes() -> Result<()> {
        let root = tempfile::tempdir().unwrap();
        let counter = Arc::new(Counter::default());
        let _registration = register(root.path(), counter.clone())?;
        let store = Store::open(root.path(), Options { background: false, ..Default::default() })?;
        let db = store.database("app").open()?;
        db.exec("create table items(value integer)")?;
        let before = counter.commits.load(Ordering::SeqCst);
        db.exec("insert into items values(1)")?;
        assert_eq!(counter.commits.load(Ordering::SeqCst), before + 1);
        db.tx::<_, Error>(|tx| {
            tx.exec("insert into items values(2)")?;
            tx.exec("insert into items values(3)")?;
            Ok(())
        })?;
        assert_eq!(counter.commits.load(Ordering::SeqCst), before + 2);
        counter.fail.store(true, Ordering::SeqCst);
        assert_eq!(db.exec("insert into items values(4)").unwrap_err().kind(), ErrorKind::OutcomeUnknown);
        assert_eq!(db.exec("insert into items values(5)").unwrap_err().kind(), ErrorKind::Unavailable);
        assert_eq!(db.scalar::<u32>("select count(*) from items")?, 4);
        assert!(store.close().is_err());
        Ok(())
    }
}
