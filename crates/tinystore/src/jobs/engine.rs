use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::maintain;
use super::policy::Policy;
use super::rows;
use super::state::{Kind, QueueState};
use super::work::Running;
use crate::engine::{Claim, Engine, Home, Host};
use crate::sqlite::{Config, File, Migration};
use crate::{Clock, Error, Result, Store, unix_millis};

/// jobs.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often the store removes what the queues keep no longer.
const MAINTENANCE: Duration = Duration::from_secs(60);

/// jobs in one file: jobs.db, or a database's file; the queues this process
/// opened there, and the workers running their handlers. Every handle opened
/// from the store, or from the database, shares it.
pub(crate) struct Jobs {
    file: Arc<File>,
    /// The engine whose file jobs are kept in, when it is not jobs.db: `sql app`.
    lent_by: Option<String>,
    clock: Arc<dyn Clock>,
    queues: Mutex<HashMap<String, Arc<QueueState>>>,
    /// Kept here, so that a worker its program let go of still stops at close.
    workers: Mutex<Vec<Arc<Running>>>,
    closed: AtomicBool,
    /// One maintenance at a time.
    maintaining: Mutex<()>,
    _claim: Option<Claim>,
}

impl Jobs {
    /// The store's jobs engine, in jobs.db, opened by the first queue.
    pub(crate) fn of(store: &Store) -> Result<Arc<Jobs>> {
        store.engine(|store| {
            let claim = store.claim("jobs.db")?;
            let file = Arc::new(File::open(claim.path(), Config::committing(store.durability()))?);
            store.keep_file(&file);
            Jobs::start(store, file, None, Some(claim))
        })
    }

    /// The jobs engine a queue opens in: the store's, or the one kept in the
    /// file of the database it was opened from.
    pub(crate) fn at(home: &Home) -> Result<Arc<Jobs>> {
        match home {
            Home::Store(store) => Jobs::of(store),
            Home::Shared(shared) => shared.engine(|store| {
                let lent_by = Some(shared.owner().to_owned());
                Jobs::start(store, Arc::clone(shared.file()), lent_by, None)
            }),
        }
    }

    /// Makes the tables of jobs in `file` when it has none, gives back the
    /// leases a process that died held, counting their attempts, and starts
    /// maintenance on the store's background thread.
    fn start(store: &Store, file: Arc<File>, lent_by: Option<String>, claim: Option<Claim>) -> Result<Arc<Jobs>> {
        file.migrate("jobs", MIGRATIONS)?;
        file.transaction(|tx| rows::end_dead_leases(tx))?;
        let task = lent_by.as_ref().map_or_else(|| "jobs".to_owned(), |owner| format!("jobs in {owner}"));
        let jobs = Arc::new(Jobs {
            file,
            lent_by,
            clock: store.clock(),
            queues: Mutex::new(HashMap::new()),
            workers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            maintaining: Mutex::new(()),
            _claim: claim,
        });
        let weak = Arc::downgrade(&jobs);
        store.every(&format!("{task}: maintenance"), MAINTENANCE, move || {
            weak.upgrade().map_or(Ok(()), |jobs| maintain::run(&jobs).map(drop))
        })?;
        Ok(jobs)
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// The file as an error names it: `jobs.db`, or `sql app`.
    pub(crate) fn place(&self) -> &str {
        self.lent_by.as_deref().unwrap_or("jobs.db")
    }

    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The queue `name` as this process knows it, registered on its first open.
    /// A second open in this process takes the same kind and options, or is
    /// `invalid`; the first gives back the jobs another group bound parked.
    pub(crate) fn queue(&self, name: &str, kind: Kind, policy: Policy) -> Result<Arc<QueueState>> {
        let describe = || QueueState::described(self.lent_by.as_deref(), kind, name);
        check_name(name).map_err(|error| error.within(describe()))?;
        policy.check().map_err(|error| error.within(describe()))?;
        let mut queues = lock(&self.queues);
        if let Some(state) = queues.get(name) {
            if state.kind != kind || state.policy != policy {
                let why = format!("open in this process as a {} with other options", state.kind.as_str());
                return Err(Error::invalid(why).within(describe()));
            }
            return Ok(Arc::clone(state));
        }
        let bound = i64::from(policy.concurrency.group.unwrap_or(0));
        let (id, stored) = self
            .file
            .transaction(|tx| {
                let (id, stored, in_group) = rows::queue(tx, name, kind.as_str())?;
                if stored == kind.as_str() {
                    rows::keep_group_bound(tx, id, in_group, bound)?;
                }
                Ok::<_, Error>((id, stored))
            })
            .map_err(|error| error.within(describe()))?;
        if stored != kind.as_str() {
            return Err(Error::invalid(format!("the name is a {stored}")).within(describe()));
        }
        let state = Arc::new(QueueState::new(id, name, kind, policy, self.lent_by.clone()));
        queues.insert(name.to_owned(), Arc::clone(&state));
        Ok(state)
    }

    pub(crate) fn open_queues(&self) -> Vec<Arc<QueueState>> {
        lock(&self.queues).values().cloned().collect()
    }

    /// Keeps a worker to stop at close.
    pub(crate) fn keep_worker(&self, running: &Arc<Running>) {
        let mut workers = lock(&self.workers);
        workers.retain(|worker| !worker.stopped());
        workers.push(Arc::clone(running));
    }

    pub(crate) fn hold_maintenance(&self) -> MutexGuard<'_, ()> {
        lock(&self.maintaining)
    }
}

/// A queue's name, as a file's is: short and plain.
fn check_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if first && rest && name.len() <= 64 {
        return Ok(());
    }
    Err(Error::invalid(format!("a name is [a-z0-9][a-z0-9_-]{{0,63}}, not {name:?}")))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Engine for Jobs {
    /// Stops every worker, which takes no job more and waits for its handlers
    /// under way, then closes jobs.db. A file another engine lends is that
    /// engine's to close.
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let workers: Vec<Arc<Running>> = lock(&self.workers).drain(..).collect();
        for worker in workers {
            worker.stop();
        }
        let closed = if self.lent_by.is_none() { self.file.close() } else { Ok(()) };
        // a watcher waiting for a change reads again, and hears the store closed
        for queue in self.open_queues() {
            queue.watchers.changed();
        }
        closed
    }

    /// Wakes every worker and watcher to read the clock again, so that a job
    /// the test made due runs now rather than when a worker's sleep ends.
    fn clock_moved(&self) {
        for queue in self.open_queues() {
            queue.alarm.wake();
            queue.watchers.changed();
        }
    }
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jobs({})", self.file.path().display())
    }
}
