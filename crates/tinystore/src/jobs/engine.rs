use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::maintain;
use super::policy::Policy;
use super::rows;
use super::state::{Kind, QueueState};
use super::work::Running;
use crate::engine::{Claim, Copied, Engine, Home, Host, copy_file};
use crate::sqlite::{Config, File, Migration};
use crate::sync::lock;
use crate::{Clock, Error, Result, Store, unix_millis};

/// jobs.db's schema, a step a file; until `v0.1.0` the first step is edited
/// rather than a second added.
const MIGRATIONS: &[Migration] = &[Migration::of(1, "0001_schema.sql", include_str!("migrations/0001_schema.sql"))];

/// jobs' own file in the store's directory.
pub(crate) const FILE: &str = "jobs.db";

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
    /// The store, for the writes a copy passes to its home, and the pool an
    /// awaited handle made from a blocking one runs on.
    #[cfg(any(feature = "replication", feature = "async"))]
    store: crate::store::WeakStore,
    queues: Mutex<HashMap<String, Arc<QueueState>>>,
    /// Kept here, so that a worker its program let go of still stops at close.
    workers: Mutex<Vec<Arc<Running>>>,
    closed: AtomicBool,
    /// One maintenance at a time.
    maintaining: Mutex<()>,
    /// What jobs tells the store's own metrics.
    probe: Arc<crate::probe::Probe>,
    _claim: Option<Claim>,
}

impl Jobs {
    /// The store's jobs engine, in jobs.db, opened by the first queue.
    pub(crate) fn of(store: &Store) -> Result<Arc<Jobs>> {
        store.engine(|store| {
            let claim = store.claim(FILE)?;
            let file = Arc::new(store.open_file(claim.name(), Config::committing(store.durability()))?);
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
        crate::engine::migrate_in(store, &file, "jobs", MIGRATIONS, lent_by.as_deref())?;
        // A copy's leases are its home's workers', and its home ends them.
        if !file.is_copy() {
            file.transaction(|tx| rows::end_dead_leases(tx))?;
        }
        let task = lent_by.as_ref().map_or_else(|| "jobs".to_owned(), |owner| format!("jobs in {owner}"));
        let jobs = Arc::new(Jobs {
            file,
            lent_by,
            clock: store.clock(),
            #[cfg(any(feature = "replication", feature = "async"))]
            store: store.downgrade(),
            queues: Mutex::new(HashMap::new()),
            workers: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            maintaining: Mutex::new(()),
            probe: Arc::clone(store.probe()),
            _claim: claim,
        });
        let sampler: std::sync::Weak<dyn crate::probe::Sample> = Arc::downgrade(&jobs) as std::sync::Weak<Jobs>;
        store.probe().sampled(sampler);
        let weak = Arc::downgrade(&jobs);
        // A copy's maintenance is its home's, and reaches it as pages: each turn
        // asks, since a file's home may move while it is open.
        store.every(&format!("{task}: maintenance"), MAINTENANCE, move || {
            weak.upgrade().filter(|jobs| !jobs.file.is_copy()).map_or(Ok(()), |jobs| maintain::run(&jobs).map(drop))
        })?;
        Ok(jobs)
    }

    /// The store jobs opened in, while it is open.
    #[cfg(any(feature = "replication", feature = "async"))]
    pub(crate) fn store(&self) -> Result<Store> {
        self.store.upgrade().ok_or_else(|| Error::closed("the store"))
    }

    /// The queue `name` as this process holds it open, or as a queue opens
    /// by default: what a write another node passed on runs on.
    #[cfg(feature = "replication")]
    pub(crate) fn queue_as_open(&self, name: &str, kind: Kind) -> Result<Arc<QueueState>> {
        if let Some(state) = lock(&self.queues).get(name) {
            return Ok(Arc::clone(state));
        }
        self.queue(name, kind, Policy::default())
    }

    /// The engine whose file jobs are kept in, when it is not jobs.db.
    #[cfg(feature = "replication")]
    pub(crate) fn lent_by(&self) -> Option<&str> {
        self.lent_by.as_deref()
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// The file itself, which a batch of the engine that lends it holds a
    /// queue's write to.
    #[cfg(feature = "sql")]
    pub(crate) fn shared_file(&self) -> &Arc<File> {
        &self.file
    }

    /// The file as an error names it: `jobs.db`, or `sql app`.
    pub(crate) fn place(&self) -> &str {
        self.lent_by.as_deref().unwrap_or(FILE)
    }

    pub(crate) fn probe(&self) -> &crate::probe::Probe {
        &self.probe
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
        #[cfg(feature = "replication")]
        if self.file.is_copy() {
            // let go while the home answers: applying its commits reads the queues again
            drop(queues);
            let (id, stored) = self.queue_from_home(name, kind).map_err(|error| error.within(describe()))?;
            if stored != kind.as_str() {
                return Err(Error::invalid(format!("the name is a {stored}")).within(describe()));
            }
            let state = Arc::new(QueueState::new(id, name, kind, policy, self.lent_by.clone()));
            let mut queues = lock(&self.queues);
            let kept = queues.entry(name.to_owned()).or_insert(state);
            // Another open may have kept one while the home answered.
            if kept.kind != kind || kept.policy != policy {
                let why = format!("open in this process as a {} with other options", kept.kind.as_str());
                return Err(Error::invalid(why).within(describe()));
            }
            return Ok(Arc::clone(kept));
        }
        let made = crate::sqlite::own_work(|| {
            self.file.transaction(|tx| {
                let (id, stored, in_group) = rows::queue(tx, name, kind.as_str())?;
                if stored == kind.as_str() {
                    rows::keep_group_bound(tx, id, in_group, bound)?;
                }
                Ok::<_, Error>((id, stored))
            })
        });
        let (id, stored) = made.map_err(|error| error.within(describe()))?;
        if stored != kind.as_str() {
            return Err(Error::invalid(format!("the name is a {stored}")).within(describe()));
        }
        let state = Arc::new(QueueState::new(id, name, kind, policy, self.lent_by.clone()));
        queues.insert(name.to_owned(), Arc::clone(&state));
        Ok(state)
    }

    /// The id and kind of the queue `name` in a copy of jobs' file: the
    /// copy's, or the one its home registers when the name is new.
    #[cfg(feature = "replication")]
    fn queue_from_home(&self, name: &str, kind: Kind) -> Result<(i64, String)> {
        let found = |jobs: &Jobs| jobs.file.read(|c| rows::find_queue(c, name));
        if let Some(found) = found(self)? {
            return Ok(found);
        }
        if kind != Kind::Queue {
            return Err(Error::new(crate::ErrorKind::Unavailable, "a copy passes no schedule to its home yet"));
        }
        let database = self.lent_by.as_deref().and_then(crate::replication::database_of);
        super::forward::pass_on_open(&self.store()?, (name, database))?;
        found(self)?.ok_or_else(|| {
            Error::new(
                crate::ErrorKind::Unavailable,
                "its home registered the queue, and this copy does not hold it yet",
            )
        })
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
    crate::text::check_name("a queue's", name)
}

impl Engine for Jobs {
    /// Stops every worker, which takes no job more and waits for its handlers
    /// under way.
    fn stop(&self) {
        let workers: Vec<Arc<Running>> = lock(&self.workers).drain(..).collect();
        for worker in workers {
            worker.stop();
        }
    }

    /// Stops every worker, then closes jobs.db. A file another engine lends
    /// is that engine's to close.
    fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.stop();
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

    /// Copies jobs.db; jobs in a database's file are the database's to copy.
    fn snapshot(&self, into: &std::path::Path) -> Result<Vec<Copied>> {
        if self.lent_by.is_some() {
            return Ok(Vec::new());
        }
        Ok(vec![copy_file(&self.file, FILE, "jobs", into)?])
    }

    /// A queue's registration is its home's, and a copy taken again may hold
    /// another: once a commit of jobs' own file arrives, a queue it names
    /// otherwise now is forgotten, and read again at its next open.
    #[cfg(feature = "replication")]
    fn applied(&self, file: &str) {
        if file == self.file_name() {
            self.forget_queues_registered_otherwise();
        }
    }
}

#[cfg(feature = "replication")]
impl Jobs {
    /// The file's name in the store's directory, as a copy's commits name it:
    /// `jobs.db`, or a database's `sql/app.db`.
    fn file_name(&self) -> String {
        self.lent_by()
            .and_then(crate::replication::database_of)
            .map_or_else(|| FILE.to_owned(), crate::engine::database_file)
    }

    /// Forgets each queue open here whose registration the file no longer
    /// holds as it was. A queue whose registration stands keeps its state:
    /// its handles and workers share it, and a second state of one queue
    /// would wake none of the first's workers nor stop their handlers.
    fn forget_queues_registered_otherwise(&self) {
        let open: Vec<(String, Arc<QueueState>)> =
            lock(&self.queues).iter().map(|(name, state)| (name.clone(), Arc::clone(state))).collect();
        if open.is_empty() {
            return;
        }
        let registered =
            self.file.read(|c| open.iter().map(|(name, _)| rows::find_queue(c, name)).collect::<Result<Vec<_>>>());
        // a read that failed learned nothing, and the states stay
        let Ok(registered) = registered else {
            return;
        };
        let mut queues = lock(&self.queues);
        for ((name, state), found) in open.iter().zip(registered) {
            let stands = found.is_some_and(|(id, kind)| id == state.id && kind == state.kind.as_str());
            if !stands && queues.get(name).is_some_and(|kept| Arc::ptr_eq(kept, state)) {
                queues.remove(name);
            }
        }
    }
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jobs({})", self.file.path().display())
    }
}

/// The queues jobs.db holds, by name. None while the store has no jobs.db,
/// which a read beside would otherwise try to make.
#[cfg(feature = "client")]
pub(crate) fn queues(store: &Store) -> Result<Vec<String>> {
    const SELECT_QUEUES: &str = "select name from _tinystore_jobs_queues order by name";
    if !store.dir().join("jobs.db").exists() {
        return Ok(Vec::new());
    }
    Jobs::of(store)?.file().read(|c| {
        let mut select =
            c.prepare_cached(SELECT_QUEUES).map_err(|error| crate::sqlite::sql_error("jobs: its queues", error))?;
        let rows = select.query_map([], |row| row.get(0));
        rows.and_then(Iterator::collect).map_err(|error| crate::sqlite::sql_error("jobs: its queues", error))
    })
}
