use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::maintain;
use super::policy::Policy;
use super::rows;
use super::state::{Kind, QueueState};
use super::work::Running;
use crate::engine::{Claim, Engine, Host};
use crate::sqlite::{Config, File, Migration};
use crate::{Clock, Error, Result, Store, unix_millis};

/// jobs.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// How often the store removes what the queues keep no longer.
const MAINTENANCE: Duration = Duration::from_secs(60);

/// Job ids reserved in one write of the meta row.
const ID_BLOCK: i64 = 1000;

/// The jobs engine of a store: jobs.db, the queues this process opened, and
/// the workers running their handlers. Every handle of the store shares it.
pub(crate) struct Jobs {
    file: File,
    clock: Arc<dyn Clock>,
    /// The next id to give and the end of the block reserved.
    ids: Mutex<(i64, i64)>,
    queues: Mutex<HashMap<String, Arc<QueueState>>>,
    /// Kept here, so that a worker its program let go of still stops at close.
    workers: Mutex<Vec<Arc<Running>>>,
    closed: AtomicBool,
    /// One maintenance at a time.
    maintaining: Mutex<()>,
    _claim: Claim,
}

impl Jobs {
    /// The store's jobs engine, opened by the first queue.
    pub(crate) fn of(store: &Store) -> Result<Arc<Jobs>> {
        store.engine(|store| {
            let jobs = Arc::new(Jobs::open(store)?);
            let weak = Arc::downgrade(&jobs);
            store.every("jobs: maintenance", MAINTENANCE, move || {
                weak.upgrade().map_or(Ok(()), |jobs| maintain::run(&jobs).map(drop))
            })?;
            Ok(jobs)
        })
    }

    /// Opens jobs.db and gives back the leases a process that died held,
    /// counting their attempts.
    fn open(store: &Store) -> Result<Jobs> {
        let claim = store.claim("jobs.db")?;
        let file = File::open(claim.path(), Config::default())?;
        file.migrate("jobs", MIGRATIONS)?;
        file.transaction(|tx| rows::end_dead_leases(tx))?;
        Ok(Jobs {
            file,
            clock: store.clock(),
            ids: Mutex::new((0, 0)),
            queues: Mutex::new(HashMap::new()),
            workers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            maintaining: Mutex::new(()),
            _claim: claim,
        })
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    pub(crate) fn now(&self) -> i64 {
        unix_millis(self.clock.now())
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The next job id, from a block reserved in a transaction of its own when
    /// the last is spent, so that an id is never given twice, not after a crash
    /// either.
    pub(crate) fn next_id(&self) -> Result<i64> {
        let mut ids = lock(&self.ids);
        if ids.0 == ids.1 {
            let end = self.file.write(0, |tx| rows::reserve_ids(tx, ID_BLOCK))?;
            *ids = (end - ID_BLOCK, end);
        }
        ids.0 += 1;
        Ok(ids.0)
    }

    /// The queue `name` as this process knows it, registered on its first open.
    /// A second open in this process takes the same kind and options, or is
    /// `invalid`; the first gives back the jobs another group bound parked.
    pub(crate) fn queue(&self, name: &str, kind: Kind, policy: Policy) -> Result<Arc<QueueState>> {
        let describe = || format!("jobs {} {name}", kind.as_str());
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
        let state = Arc::new(QueueState::new(id, name, kind, policy));
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
    /// under way, then closes jobs.db.
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let workers: Vec<Arc<Running>> = lock(&self.workers).drain(..).collect();
        for worker in workers {
            worker.stop();
        }
        let closed = self.file.close();
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
