use std::collections::VecDeque;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::call::{Asked, JobCall};
use super::claim::{self, Claiming};
use super::engine::Jobs;
use super::policy::{Concurrency, Policy};
use super::read::{self, Filter, Job, Page};
use super::run::{Outcome, Run};
use super::state::{Kind, QueueState};
use super::work::{self, Worker};
use super::write::{self, Cancelled, Prepared};
use crate::clock::millis;
use crate::engine::{Home, SharedFile};
use crate::sqlite::Tx as SqlTx;
use crate::{Error, ErrorKind, Result, Store, Transaction};

/// A queue being opened: its name, its type, and how it treats its jobs.
#[must_use = "a queue opens with open()"]
pub struct QueueBuilder<V> {
    home: Home,
    name: String,
    policy: Policy,
    _value: PhantomData<fn() -> V>,
}

impl Store {
    /// A queue of jobs whose values are `V`, as JSON, in the store's jobs.db;
    /// the engine opens with the first queue.
    pub fn queue<V>(&self, name: &str) -> QueueBuilder<V>
    where
        V: Serialize + DeserializeOwned + Send + 'static,
    {
        QueueBuilder::new(Home::Store(self.clone()), name)
    }
}

impl<V> fmt::Debug for QueueBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueueBuilder").field("name", &self.name).field("policy", &self.policy).finish()
    }
}

impl<V> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    fn new(home: Home, name: &str) -> Self {
        QueueBuilder { home, name: name.to_owned(), policy: Policy::default(), _value: PhantomData }
    }

    /// A queue kept in a file another engine lends, a database's.
    pub(crate) fn in_file(shared: Arc<SharedFile>, name: &str) -> Self {
        QueueBuilder::new(Home::Shared(shared), name)
    }

    /// Runs a job gets before it fails for good, the first counted: 10.
    pub fn attempts(mut self, attempts: u32) -> Self {
        self.policy.attempts = attempts;
        self
    }

    /// The wait before a retry: `initial`, doubling each time up to `max`, a
    /// tenth longer or shorter by the job: 1 s to 1 h.
    pub fn backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.policy.backoff = (initial, max);
        self
    }

    /// How long one run may take before it is told to stop: a minute.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.policy.timeout = timeout;
        self
    }

    /// Jobs running at once across every worker of the store, `8`, or in all
    /// and in each group, `Concurrency::total(8).group(2)`. Without it each
    /// worker runs one at a time.
    pub fn concurrency(mut self, concurrency: impl Into<Concurrency>) -> Self {
        self.policy.concurrency = concurrency.into();
        self
    }

    /// Jobs started in any span of `per`: 30 a second, as an API's limit asks.
    /// It counts starts, retries among them, and lives in memory.
    pub fn rate(mut self, count: u32, per: Duration) -> Self {
        self.policy.rate = Some((count, per));
        self
    }

    /// How long a done job keeps its id taken, so that adding it again adds
    /// nothing: an id runs once.
    pub fn dedupe(mut self, span: Duration) -> Self {
        self.policy.dedupe = Some(span);
        self
    }

    /// How long a failed job stays, with its error, to be found and started
    /// again: a week.
    pub fn keep(mut self, span: Duration) -> Self {
        self.policy.keep = span;
        self
    }

    /// Jobs that may wait; an add past it is a `limit`: ten million.
    pub fn max_waiting(mut self, jobs: u64) -> Self {
        self.policy.max_waiting = jobs;
        self
    }

    pub fn open(self) -> Result<Queue<V>> {
        Queue::open(&self.home, &self.name, Kind::Queue, self.policy)
    }
}

/// Jobs of one type in the order of their time. A handle is cheap to clone
/// and to keep.
pub struct Queue<V> {
    pub(crate) jobs: Arc<Jobs>,
    pub(crate) state: Arc<QueueState>,
    _value: PhantomData<fn() -> V>,
}

impl<V> Clone for Queue<V> {
    fn clone(&self) -> Self {
        Queue { jobs: Arc::clone(&self.jobs), state: Arc::clone(&self.state), _value: PhantomData }
    }
}

impl<V> fmt::Debug for Queue<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.state.describe())
    }
}

impl<V> Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// The same queue, its values read as another type: the wire's raw JSON.
    pub(crate) fn retyped<W>(&self) -> Queue<W> {
        Queue { jobs: Arc::clone(&self.jobs), state: Arc::clone(&self.state), _value: PhantomData }
    }

    pub(crate) fn open(home: &Home, name: &str, kind: Kind, policy: Policy) -> Result<Queue<V>> {
        let jobs = Jobs::at(home)?;
        let state = jobs.queue(name, kind, policy)?;
        Ok(Queue { jobs, state, _value: PhantomData })
    }

    /// Adds a job that runs as soon as a worker is free, and returns once it
    /// is on disk. Adds from many callers share a commit.
    pub fn add(&self, value: &V) -> Result<bool> {
        self.add_asked(Asked::default(), value, None)
    }

    /// Makes the job under `id` this value, due now, whatever it was; see
    /// [`JobCall::set`].
    pub fn set(&self, id: impl AsRef<str>, value: &V) -> Result<()> {
        self.set_asked(Asked::with_id(id.as_ref()), value, None)
    }

    /// Gives the job under `id` this value when it has not started, and says
    /// whether it did; see [`JobCall::update`].
    pub fn update(&self, id: impl AsRef<str>, value: &V) -> Result<bool> {
        self.update_asked(Asked::with_id(id.as_ref()), value, None)
    }

    /// Takes the job under `id`, whatever its state, and says whether there
    /// was one. A running one's handler is told to stop: `run.stopped()` turns
    /// true, and what it returns settles nothing. A repeating job stops.
    pub fn cancel(&self, id: impl AsRef<str>) -> Result<bool> {
        self.cancel_in(id.as_ref(), None)
    }

    /// A cancel, in `tx` when there is one. Its watchers learn it was a cancel
    /// inside its write, before the commit, so that a watch that reads the job
    /// gone never takes it for done; a cancel that does not commit takes that
    /// back.
    pub(crate) fn cancel_in(&self, id: &str, tx: Option<&Transaction<'_>>) -> Result<bool> {
        let (queue, key) = (Arc::clone(&self.state), id.to_owned());
        let work = move |c: &SqlTx<'_>| {
            let cancelled = write::cancel(c, &queue, &key)?;
            queue.watchers.cancelled(&key, !matches!(cancelled, Cancelled::Nothing));
            Ok(cancelled)
        };
        let written = match tx {
            None => self.jobs.file().write(0, work),
            Some(tx) => self.run_in(tx, work),
        };
        let cancelled = written.map_err(|error| {
            self.state.watchers.cancelled(id, false);
            self.fail(Some(id), error)
        })?;
        let found = !matches!(cancelled, Cancelled::Nothing);
        let (jobs, state, key) = (Arc::clone(&self.jobs), Arc::clone(&self.state), id.to_owned());
        let settle = move |committed: bool| cancel_settled(&jobs, &state, &key, cancelled, committed);
        match tx {
            None => settle(true),
            Some(tx) => tx.after(settle),
        }
        Ok(found)
    }

    /// `cancel` without waiting: the write is queued, and `done` is called
    /// with whether there was a job on the thread that commits it.
    pub(crate) fn cancel_then(&self, id: &str, done: impl FnOnce(Result<bool>) + Send + 'static) {
        let (queue, key, named) = (Arc::clone(&self.state), id.to_owned(), self.named(Some(id)));
        let work = move |c: &SqlTx<'_>| {
            let cancelled = write::cancel(c, &queue, &key)?;
            queue.watchers.cancelled(&key, !matches!(cancelled, Cancelled::Nothing));
            Ok(cancelled)
        };
        let (jobs, state, key) = (Arc::clone(&self.jobs), Arc::clone(&self.state), id.to_owned());
        self.jobs.file().submit(0, work, move |written| match written {
            Ok(cancelled) => {
                cancel_settled(&jobs, &state, &key, cancelled, true);
                done(Ok(!matches!(cancelled, Cancelled::Nothing)));
            }
            Err(error) => {
                state.watchers.cancelled(&key, false);
                done(Err(error.within(named)));
            }
        });
    }

    /// Where the job under `id` is: scheduled, waiting and how many jobs are
    /// ahead of it, running and its progress, failed and why, or done while the
    /// queue's dedupe keeps its id; none for an id with no job.
    pub fn get(&self, id: impl AsRef<str>) -> Result<Option<Job<V>>> {
        let (id, now) = (id.as_ref(), self.jobs.now());
        self.jobs.file().read(|c| read::get(c, &self.state, id, now)).map_err(|error| self.fail(Some(id), error))
    }

    /// A page of the queue's jobs; see [`Filter`].
    pub fn list(&self, filter: &Filter) -> Result<Page<V>> {
        let now = self.jobs.now();
        self.jobs.file().read(|c| read::page(c, &self.state, filter, now)).map_err(|error| self.fail(None, error))
    }

    /// Every job `filter` names, a page at a time, holding nothing between
    /// pages; a job written during the walk may or may not be met.
    pub fn all(&self, filter: Filter) -> All<V> {
        All { queue: self.clone(), filter: Some(filter), page: VecDeque::new() }
    }

    /// A call on the job with this id; see [`JobCall`].
    pub fn id(&self, id: impl AsRef<str>) -> JobCall<'_, V> {
        JobCall::new(self, None).id(id)
    }

    pub fn at(&self, time: SystemTime) -> JobCall<'_, V> {
        JobCall::new(self, None).at(time)
    }

    pub fn delay(&self, span: Duration) -> JobCall<'_, V> {
        JobCall::new(self, None).delay(span)
    }

    pub fn group(&self, group: impl AsRef<str>) -> JobCall<'_, V> {
        JobCall::new(self, None).group(group)
    }

    /// Runs `handler` on each job as it falls due, on threads the store starts
    /// for it, as many at once as the queue's concurrency lets, one when it
    /// sets none. It returns at once; see [`Worker`].
    ///
    /// The handler gets the job's value and its run. `Ok(())` is done; an
    /// answer of the run's says otherwise; an error retries the job after its
    /// backoff, its text kept as the job's error, and so does a panic.
    pub fn work<F, R, E>(&self, handler: F) -> Result<Worker>
    where
        F: Fn(V, &Run) -> std::result::Result<R, E> + Send + Sync + 'static,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        self.workers(self.state.policy.local()).work(handler)
    }

    /// Runs what is due when it starts and what falls due meanwhile, then
    /// returns how many jobs ran; it never waits for a later job. Tests,
    /// scripts and commands use it.
    pub fn run_due<F, R, E>(&self, handler: F) -> Result<usize>
    where
        F: Fn(V, &Run) -> std::result::Result<R, E> + Sync,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        self.workers(self.state.policy.local()).run_due(handler)
    }

    /// A worker that runs at most `handlers` at once, under the queue's
    /// concurrency.
    pub fn concurrency(&self, handlers: u32) -> Workers<'_, V> {
        self.workers(handlers)
    }

    fn workers(&self, handlers: u32) -> Workers<'_, V> {
        Workers { queue: self, handlers: usize::try_from(handlers).unwrap_or(1) }
    }

    /// Takes the next due job for a loop of the program's own, which settles
    /// it with [`Claimed::settle`]; none when nothing is due. A job not settled
    /// within the queue's timeout goes back to the queue.
    pub fn claim(&self) -> Result<Option<Claimed<V>>> {
        loop {
            let now = self.jobs.now();
            let until = now.saturating_add(millis(self.state.policy.timeout));
            let queue = Arc::clone(&self.state);
            let claimed = self
                .jobs
                .file()
                .write(0, move |tx| claim::claim(tx, Claiming { queue: &queue, now, until, limit: 1 }))
                .map_err(|error| self.fail(None, error))?;
            for _ in &claimed.abandoned {
                self.state.failures.observe(&self.state.name, "its attempts ended without a settlement");
            }
            if let Some(lease) = claimed.leases.into_iter().next() {
                let lease = Arc::new(lease);
                self.state.hold(&lease);
                let run = Run::new(Arc::clone(&lease), Arc::clone(&self.state), Arc::clone(&self.jobs));
                let value = match work::read_value::<V>(&self.jobs, &lease) {
                    Ok(value) => value,
                    Err(error) if error.kind() == ErrorKind::Invalid => {
                        Claimed { value: (), run }.settle(Outcome::fail_with(error.to_string()))?;
                        continue;
                    }
                    Err(error) => return Err(self.fail(lease.key.as_deref(), error)),
                };
                if lease.begin(now) {
                    self.state.watchers.changed();
                    return Ok(Some(Claimed { value, run }));
                }
                continue; // a cancel took it as it was claimed
            }
            if claimed.abandoned.is_empty() && !claimed.more {
                return Ok(None);
            }
        }
    }

    pub(crate) fn add_asked(&self, asked: Asked, value: &V, tx: Option<&Transaction<'_>>) -> Result<bool> {
        let id = asked.id().map(str::to_owned);
        let fail = |error| self.fail(id.as_deref(), error);
        let call = asked.prepare(value, self.jobs.now()).map_err(fail)?;
        let (queue, at) = (Arc::clone(&self.state), call.at);
        let added = match tx {
            None => self.jobs.file().write(call.value.len(), move |c| write::add(c, &queue, &call)),
            Some(tx) => self.run_in(tx, |c| write::add(c, &queue, &call)),
        }
        .map_err(fail)?;
        if added {
            self.wake(at, tx);
        }
        Ok(added)
    }

    pub(crate) fn set_asked(&self, asked: Asked, value: &V, tx: Option<&Transaction<'_>>) -> Result<()> {
        let id = asked.id().map(str::to_owned);
        let Some(key) = id.as_deref() else {
            return Err(self.fail(None, Error::invalid("a set needs an id, which names the job it sets")));
        };
        let fail = |error| self.fail(Some(key), error);
        let call = asked.prepare(value, self.jobs.now()).map_err(fail)?;
        let queue = Arc::clone(&self.state);
        let set = match tx {
            None => self.jobs.file().write(call.value.len(), move |c| write::set(c, &queue, &call)),
            Some(tx) => self.run_in(tx, |c| write::set(c, &queue, &call)),
        }
        .map_err(fail)?;
        self.wake(set.due, tx);
        Ok(())
    }

    pub(crate) fn update_asked(&self, asked: Asked, value: &V, tx: Option<&Transaction<'_>>) -> Result<bool> {
        let id = asked.id().map(str::to_owned);
        let Some(key) = id.as_deref() else {
            return Err(self.fail(None, Error::invalid("an update needs an id, which names the job it changes")));
        };
        let fail = |error| self.fail(Some(key), error);
        let call = asked.prepare(value, self.jobs.now()).map_err(fail)?;
        let queue = Arc::clone(&self.state);
        let due = match tx {
            None => self.jobs.file().write(call.value.len(), move |c| write::update(c, &queue, &call)),
            Some(tx) => self.run_in(tx, |c| write::update(c, &queue, &call)),
        }
        .map_err(fail)?;
        if let Some(due) = due {
            self.wake(due, tx);
        }
        Ok(due.is_some())
    }

    /// An add, a set or an update without waiting: the write is queued, and
    /// `done` is called with whether it changed anything, on the thread that
    /// commits it. For a host whose threads must not each wait for a commit:
    /// the writes waiting together are the writes one commit carries.
    pub(crate) fn write_then(
        &self,
        change: Change,
        asked: Asked,
        value: &V,
        done: impl FnOnce(Result<bool>) + Send + 'static,
    ) {
        let named = self.named(asked.id());
        let call = match self.prepared(change, asked, value) {
            Ok(call) => call,
            Err(error) => return done(Err(error.within(named))),
        };
        let (queue, state, bytes) = (Arc::clone(&self.state), Arc::clone(&self.state), call.value.len());
        // the time the job is due at, when the write changed it
        let write = move |c: &SqlTx<'_>| match change {
            Change::Add => Ok(write::add(c, &queue, &call)?.then_some(call.at)),
            Change::Set => write::set(c, &queue, &call).map(|set| Some(set.due)),
            Change::Update => write::update(c, &queue, &call),
        };
        self.jobs.file().submit(bytes, write, move |due| {
            if let Ok(Some(at)) = due {
                state.alarm.lower(at);
                state.watchers.changed();
            }
            done(due.map(|due| due.is_some()).map_err(|error| error.within(named)));
        });
    }

    /// A call checked, its value encoded, before its write waits for the writer.
    fn prepared(&self, change: Change, asked: Asked, value: &V) -> Result<Prepared> {
        match (change, asked.id()) {
            (Change::Set, None) => return Err(Error::invalid("a set needs an id, which names the job it sets")),
            (Change::Update, None) => {
                return Err(Error::invalid("an update needs an id, which names the job it changes"));
            }
            _ => {}
        }
        asked.prepare(value, self.jobs.now())
    }

    /// Runs a write of the queue in a savepoint of `tx`, once the queue is
    /// kept in the transaction's file.
    pub(crate) fn run_in<T>(&self, tx: &Transaction<'_>, work: impl FnOnce(&SqlTx<'_>) -> Result<T>) -> Result<T> {
        tx.takes(self.jobs.file(), self.jobs.place())?;
        tx.call(work)
    }

    /// Wakes the queue's workers and watchers for a job due at `at`, at once,
    /// or once the transaction it was written in ends: a worker woken before
    /// the commit would not find the job, and sleep past it.
    fn wake(&self, at: i64, tx: Option<&Transaction<'_>>) {
        let state = Arc::clone(&self.state);
        let wake = move |_committed: bool| {
            state.alarm.lower(at);
            state.watchers.changed();
        };
        match tx {
            None => wake(true),
            Some(tx) => tx.after(wake),
        }
    }

    /// Names the queue and the id a call failed on.
    pub(crate) fn fail(&self, id: Option<&str>, error: Error) -> Error {
        error.within(self.named(id))
    }

    /// The queue and the id of a call, as its error says them first.
    fn named(&self, id: Option<&str>) -> String {
        match id {
            Some(id) => format!("{}: id {id:?}", self.state.describe()),
            None => self.state.describe(),
        }
    }
}

/// What a job's call writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Add,
    Set,
    Update,
}

/// What follows a cancel's write: a cancel that did not commit takes back what
/// it told the job's watchers, and one that did stops the job's handler, when
/// it runs here, and tells the watchers of the change.
fn cancel_settled(jobs: &Jobs, state: &QueueState, key: &str, cancelled: Cancelled, committed: bool) {
    if !committed {
        state.watchers.cancelled(key, false);
        return;
    }
    if let Cancelled::Job(job) = cancelled {
        if let Some(lease) = state.held(job) {
            lease.cancel();
            state.release(&lease);
        }
        state.room_made(jobs.now());
    }
    if !matches!(cancelled, Cancelled::Nothing) {
        state.watchers.changed();
    }
}

/// A worker being started: how many handlers it runs at once.
#[must_use = "a worker starts with work() or runs with run_due()"]
pub struct Workers<'a, V> {
    queue: &'a Queue<V>,
    handlers: usize,
}

impl<V> Workers<'_, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// Starts the worker; see [`Queue::work`].
    pub fn work<F, R, E>(self, handler: F) -> Result<Worker>
    where
        F: Fn(V, &Run) -> std::result::Result<R, E> + Send + Sync + 'static,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        let queue = self.queue;
        work::start(&queue.jobs, &queue.state, self.handlers.max(1), handler).map_err(|error| queue.fail(None, error))
    }

    /// Runs what is due; see [`Queue::run_due`].
    pub fn run_due<F, R, E>(self, handler: F) -> Result<usize>
    where
        F: Fn(V, &Run) -> std::result::Result<R, E> + Sync,
        R: Into<Outcome>,
        E: fmt::Display,
    {
        let queue = self.queue;
        work::run_due(&queue.jobs, &queue.state, self.handlers.max(1), &handler)
            .map_err(|error| queue.fail(None, error))
    }
}

impl<V> fmt::Debug for Workers<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workers").field("handlers", &self.handlers).finish()
    }
}

/// A job a program claimed for a loop of its own: its value, its run, and the
/// lease it settles.
pub struct Claimed<V> {
    pub value: V,
    run: Run,
}

impl<V> Claimed<V> {
    pub fn run(&self) -> &Run {
        &self.run
    }

    /// Settles the job by `outcome`, as a handler's answer does: `()` for done,
    /// or one of the run's answers. A job whose lease ended and that another
    /// claim took since, or that a cancel took, is a `conflict`.
    pub fn settle(self, outcome: impl Into<Outcome>) -> Result<()> {
        let (lease, queue, jobs) = (&self.run.lease, &self.run.queue, &self.run.jobs);
        let now = jobs.now();
        let how = outcome.into().how(now);
        let (settling, kept) = (Arc::clone(lease), Arc::clone(queue));
        let settled = jobs.file().write(0, move |tx| claim::settle(tx, &kept, &settling, &how, now))?;
        lease.apply(&settled, false);
        queue.release(lease);
        queue.watchers.changed();
        if let Some(due) = settled.due {
            queue.alarm.lower(due);
        }
        if let Some(failed) = &settled.failed {
            queue.failures.observe(&queue.name, failed);
        }
        queue.room_made(now);
        if settled.lost {
            let why = if lease.cancelled() {
                "a cancel took the job"
            } else {
                "the job's lease ended and another claim took it"
            };
            return Err(Error::new(ErrorKind::Conflict, why).within(self.run.describe()));
        }
        Ok(())
    }
}

impl<V: fmt::Debug> fmt::Debug for Claimed<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claimed").field("value", &self.value).field("run", &self.run).finish()
    }
}

/// Every job a filter names, a page at a time.
pub struct All<V> {
    queue: Queue<V>,
    /// The filter of the next page; none once the last page was read.
    filter: Option<Filter>,
    page: VecDeque<Job<V>>,
}

impl<V> Iterator for All<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    type Item = Result<Job<V>>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.page.is_empty() {
            let filter = self.filter.take()?;
            match self.queue.list(&filter) {
                Ok(page) => {
                    self.filter = page.next.map(|next| filter.after(next));
                    self.page.extend(page.jobs);
                }
                Err(error) => return Some(Err(error)),
            }
        }
        self.page.pop_front().map(Ok)
    }
}

impl<V> fmt::Debug for All<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All").field("queue", &self.queue).finish()
    }
}
