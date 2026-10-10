use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use super::tx::BOUND;
use crate::engine::{Claim, Engine, Host, SharedFile};
use crate::sqlite::{Config, File, Migration};
use crate::{Clock, Error, Memory, Result, Store};

/// The history a database's own migrations are kept under.
const HISTORY: &str = "migrations";

/// Statements known to write that a database remembers; past them it forgets
/// them all and asks SQLite again.
const WRITES_KEPT: usize = 1024;

/// The store's application databases, by name: one file and one writer each,
/// opened by the first handle and closed with the store.
pub(crate) struct Databases {
    open: Mutex<HashMap<String, Arc<Base>>>,
}

/// One application database: `sql/<name>.db`, its writer and its readers,
/// and the buckets and queues kept in its file.
pub(crate) struct Base {
    pub(crate) name: String,
    pub(crate) shared: Arc<SharedFile>,
    clock: Arc<dyn Clock>,
    /// The store's memory, which a call's rows hold while it holds them.
    pub(crate) memory: Arc<Memory>,
    /// Statements known to write, by their text. SQLite says whether one
    /// writes when it compiles it, and a read call sends those to the writer.
    writes: Mutex<HashSet<String>>,
    _claim: Claim,
}

impl Databases {
    /// The store's databases, opened with the first.
    pub(crate) fn of(store: &Store) -> Result<Arc<Databases>> {
        store.engine(|_| Ok(Arc::new(Databases { open: Mutex::new(HashMap::new()) })))
    }

    /// The database `name`, opened when it is not, after `migrations` are
    /// applied and checked; one already open applies what it lacks of them.
    pub(crate) fn open(&self, store: &Store, name: &str, migrations: Option<Vec<Migration>>) -> Result<Arc<Base>> {
        let mut open = lock(&self.open);
        if let Some(base) = open.get(name) {
            let base = Arc::clone(base);
            drop(open);
            if let Some(migrations) = migrations {
                base.file().migrate_checked(HISTORY, &migrations)?;
            }
            return Ok(base);
        }
        let claim = store.claim(&format!("sql/{name}.db"))?;
        let file = File::open(claim.path(), config())?;
        if let Some(migrations) = migrations {
            file.migrate_checked(HISTORY, &migrations)?;
        }
        let base = Base {
            name: name.to_owned(),
            shared: Arc::new(SharedFile::new(file, format!("sql {name}"), store)),
            clock: store.clock(),
            memory: Arc::clone(store.memory()),
            writes: Mutex::new(HashSet::new()),
            _claim: claim,
        };
        let base = Arc::new(base);
        open.insert(name.to_owned(), Arc::clone(&base));
        Ok(base)
    }
}

impl Engine for Databases {
    /// Closes every database; the first that fails is the error, and the
    /// others close either way.
    fn close(&self) -> Result<()> {
        let open: Vec<Arc<Base>> = lock(&self.open).drain().map(|(_, base)| base).collect();
        let mut first_failure = None;
        for base in open {
            if let Err(error) = base.file().close() {
                first_failure.get_or_insert(error.within(base.describe()));
            }
        }
        first_failure.map_or(Ok(()), Err)
    }
}

impl Base {
    pub(crate) fn file(&self) -> &File {
        self.shared.file()
    }

    pub(crate) fn now(&self) -> SystemTime {
        self.clock.now()
    }

    /// Fails once a transaction that started at `started` has held the writer
    /// past its bound, which then rolls back.
    pub(crate) fn within_bound(&self, started: SystemTime) -> Result<()> {
        let held = self.now().duration_since(started).unwrap_or_default();
        if held <= BOUND {
            return Ok(());
        }
        Err(Error::limit(format!(
            "{}: a transaction held the writer {held:?}, past its {BOUND:?}, and rolls back",
            self.describe()
        )))
    }

    pub(crate) fn writes(&self, text: &str) -> bool {
        lock(&self.writes).contains(text)
    }

    pub(crate) fn remember_write(&self, text: &str) {
        let mut writes = lock(&self.writes);
        if writes.len() >= WRITES_KEPT {
            writes.clear();
        }
        writes.insert(text.to_owned());
    }

    pub(crate) fn describe(&self) -> String {
        format!("sql {}", self.name)
    }
}

/// A database's connections: every file's, with room for an application's
/// statements.
fn config() -> Config {
    Config { statements: 128, ..Config::default() }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
