use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::{Duration, SystemTime};

use futures_core::Stream;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::run::Run;
use crate::jobs::{self, Change, Concurrency, Filter, Job, Outcome, Page, Took};
use crate::pool::Pool;
use crate::{ErrorKind, Result, Transaction, TxHandle};

/// A queue being opened, as [`jobs::QueueBuilder`]: its name, its type, and
/// how it treats its jobs.
#[must_use = "a queue opens with open()"]
pub struct QueueBuilder<V> {
    builder: jobs::QueueBuilder<V>,
    pool: Arc<Pool>,
}

impl<V> fmt::Debug for QueueBuilder<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.builder, f)
    }
}

impl<V> QueueBuilder<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    pub(crate) fn new(builder: jobs::QueueBuilder<V>, pool: Arc<Pool>) -> Self {
        QueueBuilder { builder, pool }
    }

    fn with(self, option: impl FnOnce(jobs::QueueBuilder<V>) -> jobs::QueueBuilder<V>) -> Self {
        QueueBuilder { builder: option(self.builder), pool: self.pool }
    }

    /// Runs a job gets before it fails for good, the first counted: 10.
    pub fn attempts(self, attempts: u32) -> Self {
        self.with(|builder| builder.attempts(attempts))
    }

    /// The wait before a retry: `initial`, doubling each time up to `max`, a
    /// tenth longer or shorter by the job: 1 s to 1 h.
    pub fn backoff(self, initial: Duration, max: Duration) -> Self {
        self.with(|builder| builder.backoff(initial, max))
    }

    /// How long one run may take before it is told to stop: a minute.
    pub fn timeout(self, timeout: Duration) -> Self {
        self.with(|builder| builder.timeout(timeout))
    }

    /// Jobs running at once across every worker of the store, `8`, or in all
    /// and in each group, `Concurrency::total(8).group(2)`. Without it each
    /// worker runs one at a time.
    pub fn concurrency(self, concurrency: impl Into<Concurrency>) -> Self {
        let concurrency = concurrency.into();
        self.with(|builder| builder.concurrency(concurrency))
    }

    /// Jobs started in any span of `per`: 30 a second, as an API's limit asks.
    /// It counts starts, retries among them, and lives in memory.
    pub fn rate(self, count: u32, per: Duration) -> Self {
        self.with(|builder| builder.rate(count, per))
    }

    /// How long a done job keeps its id taken, so that adding it again adds
    /// nothing: an id runs once.
    pub fn dedupe(self, span: Duration) -> Self {
        self.with(|builder| builder.dedupe(span))
    }

    /// How long a failed job stays, with its error, to be found and started
    /// again: a week.
    pub fn keep(self, span: Duration) -> Self {
        self.with(|builder| builder.keep(span))
    }

    /// Jobs that may wait; an add past it is a `limit`: ten million.
    pub fn max_waiting(self, jobs: u64) -> Self {
        self.with(|builder| builder.max_waiting(jobs))
    }

    /// Opens the queue, a plain call made once at start: the first open of
    /// the engine migrates jobs.db and gives back what a dead process held.
    pub fn open(self) -> Result<Queue<V>> {
        Ok(Queue { queue: self.builder.open()?, pool: self.pool })
    }
}

/// Jobs of one type in the order of their time, as [`jobs::Queue`], its
/// calls that wait awaited. A handle is cheap to clone and to keep.
pub struct Queue<V> {
    pub(super) queue: jobs::Queue<V>,
    pub(super) pool: Arc<Pool>,
}

impl<V> Clone for Queue<V> {
    fn clone(&self) -> Self {
        Queue { queue: self.queue.clone(), pool: Arc::clone(&self.pool) }
    }
}

impl<V> fmt::Debug for Queue<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.queue, f)
    }
}

impl<V> From<jobs::Queue<V>> for Queue<V> {
    /// The same queue, its calls awaited.
    fn from(queue: jobs::Queue<V>) -> Queue<V> {
        let pool = super::pool_of(&queue);
        Queue { queue, pool }
    }
}

impl<V> Queue<V> {
    /// The same queue, for its blocking calls.
    pub fn blocking(&self) -> &jobs::Queue<V> {
        &self.queue
    }
}

impl<V> Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    /// Adds a job that runs as soon as a worker is free, and answers once it
    /// is on disk; adds from many callers share a commit. Its write is queued
    /// at jobs.db's writer at the first poll, so a future polled once commits
    /// whether or not it is still awaited.
    pub fn add(&self, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.call().add(value)
    }

    /// Makes the job under `id` this value, due now, whatever it was; see
    /// [`JobCall::set`].
    pub fn set(&self, id: impl AsRef<str>, value: &V) -> impl Future<Output = Result<()>> + Send + 'static {
        self.id(id).set(value)
    }

    /// Gives the job under `id` this value when it has not started, and says
    /// whether it did; see [`JobCall::update`].
    pub fn update(&self, id: impl AsRef<str>, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.id(id).update(value)
    }

    /// Takes the job under `id`, whatever its state, and says whether there
    /// was one. A running one's handler is told to stop: `run.stopped()` turns
    /// true, and what it returns settles nothing. A repeating job stops.
    pub fn cancel(&self, id: impl AsRef<str>) -> impl Future<Output = Result<bool>> + Send + 'static {
        let (queue, id) = (self.queue.clone(), id.as_ref().to_owned());
        crate::aio::run::write(&self.pool, format!("{queue:?}: id {id:?}: a cancel"), move |answer| {
            queue.cancel_then(&id, move |found| answer.send(found));
        })
    }

    /// Where the job under `id` is, as [`jobs::Queue::get`] says; none for an
    /// id with no job. It reads on the store's pool, since it counts up to
    /// 10,000 jobs ahead of a waiting one.
    pub fn get(&self, id: impl AsRef<str>) -> impl Future<Output = Result<Option<Job<V>>>> + Send + 'static {
        let (queue, id) = (self.queue.clone(), id.as_ref().to_owned());
        crate::aio::run::read(&self.pool, format!("{queue:?}: id {id:?}: a get"), move || queue.get(&id))
    }

    /// A page of the queue's jobs, read on the store's pool; see [`Filter`].
    pub fn list(&self, filter: &Filter) -> impl Future<Output = Result<Page<V>>> + Send + 'static {
        let (queue, filter) = (self.queue.clone(), filter.clone());
        crate::aio::run::read(&self.pool, format!("{queue:?}: a list"), move || queue.list(&filter))
    }

    /// Every job `filter` names, a page at a time, each page read on the
    /// store's pool once the one before is used up and nothing held between
    /// pages; a job written during the walk may or may not be met.
    pub fn all(&self, filter: Filter) -> All<V> {
        All { queue: self.clone(), filter: Some(filter), page: VecDeque::new(), reading: None }
    }

    /// A call on the job with this id; see [`JobCall`].
    pub fn id(&self, id: impl AsRef<str>) -> JobCall<'_, V> {
        JobCall { call: self.queue.id(id), queue: self }
    }

    pub fn at(&self, time: SystemTime) -> JobCall<'_, V> {
        JobCall { call: self.queue.at(time), queue: self }
    }

    pub fn delay(&self, span: Duration) -> JobCall<'_, V> {
        JobCall { call: self.queue.delay(span), queue: self }
    }

    pub fn group(&self, group: impl AsRef<str>) -> JobCall<'_, V> {
        JobCall { call: self.queue.group(group), queue: self }
    }

    fn call(&self) -> JobCall<'_, V> {
        JobCall { call: jobs::JobCall::new(&self.queue, None), queue: self }
    }

    /// Takes the next due job for a loop of the program's own, which settles
    /// it with [`Claimed::settle`]; none when nothing is due, never waiting
    /// for a job. Its lease is a write at jobs.db's writer, and a value kept
    /// in a row of its own is read after the commit, in the poll when a
    /// reader is idle. Dropped once its write was queued, the job comes back
    /// when its lease, the queue's timeout, ends, the attempt counted.
    pub fn claim(&self) -> impl Future<Output = Result<Option<Claimed<V>>>> + Send + 'static {
        let queue = self.clone();
        async move {
            loop {
                let (claiming, what) = (queue.queue.clone(), format!("{:?}: a claim", queue.queue));
                let took = crate::aio::run::write(&queue.pool, what, move |answer| {
                    claiming.claim_then(move |took| answer.send(took));
                });
                let lease = match took.await? {
                    Took::Job(lease) => lease,
                    Took::More => continue,
                    Took::Nothing => return Ok(None),
                };
                let run = Run::of(&queue, lease);
                let value = match run.value::<V>().await {
                    Ok(value) => value,
                    Err(error) if error.kind() == ErrorKind::Invalid => {
                        Claimed { value: (), run }.settle(Outcome::fail_with(error.to_string())).await?;
                        continue;
                    }
                    Err(error) => return Err(queue.queue.fail(run.id(), error)),
                };
                if run.begin() {
                    return Ok(Some(Claimed { value, run }));
                }
                // a cancel took it as it was claimed
            }
        }
    }
}

impl<V> crate::transaction::sealed::Sealed for Queue<V> {}

impl<V> TxHandle for Queue<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    type InTx<'t>
        = jobs::TxQueue<'t, V>
    where
        Self: 't;

    /// The blocking [`jobs::TxQueue`]'s calls, inside a transaction's closure,
    /// which runs on the store's pool and must not wait for the program's
    /// awaits.
    fn in_tx<'t>(&'t self, tx: &'t Transaction<'t>) -> jobs::TxQueue<'t, V> {
        self.queue.in_tx(tx)
    }
}

/// A call on one job with what it asks beside its value, run by its last
/// step, as [`jobs::JobCall`]: `later.id(&key).at(nine_am).add(&draft).await`.
#[must_use = "a job's call runs at its last step: add, set or update"]
pub struct JobCall<'a, V> {
    call: jobs::JobCall<'a, V>,
    queue: &'a Queue<V>,
}

impl<V> fmt::Debug for JobCall<'_, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.call, f)
    }
}

impl<'a, V> JobCall<'a, V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    fn with(self, step: impl FnOnce(jobs::JobCall<'a, V>) -> jobs::JobCall<'a, V>) -> Self {
        JobCall { call: step(self.call), queue: self.queue }
    }

    /// The job's id, 1 to 1024 bytes of text: what finds it again, and what
    /// adds it once however many times its caller asks.
    pub fn id(self, id: impl AsRef<str>) -> Self {
        self.with(|call| call.id(id))
    }

    /// Runs the job at `time`; a time in the past runs it now.
    pub fn at(self, time: SystemTime) -> Self {
        self.with(|call| call.at(time))
    }

    /// Runs the job `span` from now.
    pub fn delay(self, span: Duration) -> Self {
        self.with(|call| call.delay(span))
    }

    /// The group whose running jobs the queue's concurrency bounds.
    pub fn group(self, group: impl AsRef<str>) -> Self {
        self.with(|call| call.group(group))
    }

    /// Repeats the job every `span`; see [`jobs::JobCall::every`].
    pub fn every(self, span: Duration) -> Self {
        self.with(|call| call.every(span))
    }

    /// Repeats the job by a five-field cron expression on the wall clock of
    /// `zone`, an IANA name, `"UTC"` among them.
    pub fn cron(self, expr: &str, zone: &str) -> Self {
        self.with(|call| call.cron(expr, zone))
    }

    /// Adds the job unless its id is taken, and says whether it added one;
    /// see [`jobs::JobCall::add`]. The call is checked and its value encoded
    /// now, and its write queued at the first poll: polled once, it commits
    /// whether or not it is still awaited.
    #[expect(clippy::should_implement_trait, reason = "a queue's add puts a job in it, as BullMQ's does: no sum")]
    pub fn add(self, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.write(Change::Add, "an add", value)
    }

    /// Makes the id's job this value at this time, whatever it was; see
    /// [`jobs::JobCall::set`].
    pub fn set(self, value: &V) -> impl Future<Output = Result<()>> + Send + 'static {
        let written = self.write(Change::Set, "a set", value);
        async move { written.await.map(drop) }
    }

    /// Changes the id's job when it has not started, and says whether it did;
    /// see [`jobs::JobCall::update`].
    pub fn update(self, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        self.write(Change::Update, "an update", value)
    }

    /// The blocking call, which the job's add for a database's batch is made
    /// from.
    #[cfg(feature = "sql")]
    pub(crate) fn into_blocking(self) -> jobs::JobCall<'a, V> {
        self.call
    }

    fn write(self, change: Change, what: &str, value: &V) -> impl Future<Output = Result<bool>> + Send + 'static {
        let what = format!("{:?}: {what}", self.queue.queue);
        let writing = self.call.writing(change, value);
        crate::aio::run::write(&self.queue.pool, what, move |answer| {
            writing.submit(move |written| answer.send(written))
        })
    }
}

/// A job a program claimed for a loop of its own: its value, its run, and
/// the lease it settles.
pub struct Claimed<V> {
    pub value: V,
    run: Run,
}

impl<V> Claimed<V> {
    pub fn run(&self) -> &Run {
        &self.run
    }

    /// Settles the job by `outcome`, as a handler's answer does: `()` for done,
    /// or one of the run's answers. Its write is queued at jobs.db's writer at
    /// the first poll, and the job is let go as it commits, whether or not the
    /// future is still awaited. A job whose lease ended and that another
    /// claim, a change or a cancel took since is a `conflict`.
    pub fn settle(self, outcome: impl Into<Outcome>) -> impl Future<Output = Result<()>> + Send + 'static {
        let settling = jobs::Settling::of(&self.run.run, outcome.into());
        let (pool, what) = (Arc::clone(&self.run.pool), format!("{}: a settlement", self.run.run.describe()));
        async move {
            crate::aio::run::write(&pool, what, move |answer| settling.submit(move |settled| answer.send(settled)))
                .await
        }
    }
}

impl<V: fmt::Debug> fmt::Debug for Claimed<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claimed").field("value", &self.value).field("run", &self.run).finish()
    }
}

/// Every job a filter names, a page at a time: a [`Stream`] whose next page
/// is read on the store's pool once the last one is used up. Dropped, it
/// stops, and so does a page under way.
pub struct All<V> {
    queue: Queue<V>,
    /// The filter of the next page; none once the last page was read.
    filter: Option<Filter>,
    page: VecDeque<Job<V>>,
    /// The page under way, and the filter it was asked with.
    reading: Option<(Filter, Reading<V>)>,
}

/// A page read on the store's pool.
type Reading<V> = Pin<Box<dyn Future<Output = Result<Page<V>>> + Send>>;

/// No field is pinned: the page under way is boxed.
impl<V> Unpin for All<V> {}

impl<V> Stream for All<V>
where
    V: Serialize + DeserializeOwned + Send + 'static,
{
    type Item = Result<Job<V>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let all = self.get_mut();
        loop {
            if let Some(job) = all.page.pop_front() {
                return Poll::Ready(Some(Ok(job)));
            }
            let (filter, reading) = match &mut all.reading {
                Some(reading) => reading,
                None => {
                    let Some(filter) = all.filter.take() else {
                        return Poll::Ready(None);
                    };
                    let reading = Box::pin(all.queue.list(&filter));
                    all.reading.insert((filter, reading))
                }
            };
            let page = ready!(reading.as_mut().poll(context));
            let filter = filter.clone();
            all.reading = None;
            match page {
                Ok(page) => {
                    all.filter = page.next.map(|next| filter.after(next));
                    all.page.extend(page.jobs);
                }
                Err(error) => return Poll::Ready(Some(Err(error))),
            }
        }
    }
}

impl<V> fmt::Debug for All<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("All").field("queue", &self.queue).finish()
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
