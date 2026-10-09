//! jobs' methods of protocol 2, protocol/jobs.wire: handles on queues and
//! schedules, calls on their jobs, and workers whose handlers are a client's.
//! A value travels as its JSON, checked and kept as it came.
//!
//! A client's worker is a stream both ways. The server's loop claims the
//! queue's jobs as on any worker and hands each to the stream, which sends it
//! as a held job once one of the client's handlers is free; the client answers
//! each on the same stream. Every job the stream was handed is answered to the
//! loop once: by the client, by a cancel that took it, or by the stream's end.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use serde_json::value::RawValue;

use super::Route;
use super::codec::Message;
use super::protocol::{
    Empty, Failure, Handle, JobsAnswer, JobsCall, JobsChanged, JobsHeld, JobsId, JobsJob, JobsKept, JobsList, JobsPage,
    JobsQueueOpen, JobsScheduleOpen, JobsStep, JobsWork, method,
};
use crate::clock::from_unix_millis;
use crate::jobs::{
    Answers, Concurrency, Ended, Filter, Hand, How, Job, JobCall, Lease, Queue, Remote, RemoteWork, State,
    keep_encoded, kept, read_value, start_remote,
};
use crate::{Error, ErrorKind, Store, unix_millis};

/// A job's value as the wire carries it.
type Raw = Box<RawValue>;

pub(crate) type Answered = Result<Vec<u8>, Failure>;

/// Puts a DATA of a work stream on its connection.
pub(crate) type Sender = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Ends a work stream with its last frame.
pub(crate) type Finisher = Arc<dyn Fn(Answered) + Send + Sync>;

/// A connection's workers, by stream.
type Works = Mutex<HashMap<u32, Arc<Work>>>;

/// A progress, as JSON, is at most this long: memory keeps it while its job
/// runs.
const MAX_PROGRESS: usize = 4 << 10;
/// The handlers a client's worker runs at once at most.
const MAX_HANDLERS: u64 = 1024;

pub(crate) fn route(called: u16) -> Route {
    match called {
        method::JOBS_WORK => Route::Exchange,
        _ => Route::Worker,
    }
}

/// What a connection opened, by handle, and its workers, by stream.
#[derive(Default)]
pub(crate) struct Handles {
    open: Mutex<HashMap<u64, Opened>>,
    last: AtomicU64,
    works: Arc<Works>,
    /// The last run given: a run's number names its job on the connection, to
    /// its answer, a step and a keep.
    runs: Arc<AtomicU64>,
}

#[derive(Clone)]
struct Opened {
    queue: Queue<Raw>,
    /// A schedule's one job is its repeat, which no call adds, sets or updates.
    schedule: bool,
}

/// Answers a call that runs to its end on this thread.
pub(crate) fn call(store: &Store, handles: &Handles, called: u16, body: &[u8], max_body: usize) -> Answered {
    match called {
        method::JOBS_QUEUE_OPEN => handles.open(open_queue(store, JobsQueueOpen::decode(body)?)?, false),
        method::JOBS_SCHEDULE_OPEN => handles.open(open_schedule(store, JobsScheduleOpen::decode(body)?)?, true),
        method::JOBS_ADD => {
            let added = on_job(handles, JobsCall::decode(body)?, |job, value| job.add(&value))?;
            Ok(JobsChanged { changed: added }.encode())
        }
        method::JOBS_SET => {
            on_job(handles, JobsCall::decode(body)?, |job, value| job.set(&value))?;
            Ok(Empty {}.encode())
        }
        method::JOBS_UPDATE => {
            let changed = on_job(handles, JobsCall::decode(body)?, |job, value| job.update(&value))?;
            Ok(JobsChanged { changed }.encode())
        }
        method::JOBS_CANCEL => {
            let id = JobsId::decode(body)?;
            let changed = handles.opened(id.handle)?.queue.cancel(&id.id)?;
            Ok(JobsChanged { changed }.encode())
        }
        method::JOBS_GET => {
            let id = JobsId::decode(body)?;
            let job = handles.opened(id.handle)?.queue.get(&id.id)?;
            within(job.map_or_else(JobsJob::default, job_of).encode(), max_body)
        }
        method::JOBS_LIST => list(handles, JobsList::decode(body)?, max_body),
        method::JOBS_STEP => step(handles, JobsStep::decode(body)?, max_body),
        method::JOBS_KEEP => keep(handles, JobsStep::decode(body)?),
        other => Err(Failure::unimplemented(format!("method {other:#06x}"))),
    }
}

fn open_queue(store: &Store, open: JobsQueueOpen) -> Result<Queue<Raw>, Failure> {
    let mut builder = store.queue::<Raw>(&open.name);
    if let Some(attempts) = open.attempts {
        builder = builder.attempts(small(attempts, "attempts")?);
    }
    if let Some(backoff) = open.backoff {
        builder = builder.backoff(span(backoff.initial), span(backoff.max));
    }
    if let Some(timeout) = open.timeout {
        builder = builder.timeout(span(timeout));
    }
    if let Some(concurrency) = open.concurrency {
        let mut bound = match concurrency.total {
            Some(total) => Concurrency::total(small(total, "a concurrency")?),
            None => Concurrency::default(),
        };
        if let Some(group) = concurrency.group {
            bound = bound.group(small(group, "a group's concurrency")?);
        }
        builder = builder.concurrency(bound);
    }
    if let Some(rate) = open.rate {
        builder = builder.rate(small(rate.count, "a rate")?, span(rate.per));
    }
    if let Some(dedupe) = open.dedupe {
        builder = builder.dedupe(span(dedupe));
    }
    if let Some(keep) = open.keep {
        builder = builder.keep(span(keep));
    }
    if let Some(max_waiting) = open.max_waiting {
        builder = builder.max_waiting(max_waiting);
    }
    Ok(builder.open()?)
}

fn open_schedule(store: &Store, open: JobsScheduleOpen) -> Result<Queue<Raw>, Failure> {
    let builder = store.schedule(&open.name);
    let mut builder = match (open.every, open.cron, open.time_zone) {
        (Some(every), None, None) => builder.every(span(every)),
        (None, Some(cron), zone) => builder.cron(&cron, zone.as_deref().unwrap_or_default()),
        (None, None, _) => return Err(Failure::invalid("a schedule repeats by every or by cron")),
        _ => return Err(Failure::invalid("a schedule repeats by every or by cron, not both; a time zone is a cron's")),
    };
    if let Some(attempts) = open.attempts {
        builder = builder.attempts(small(attempts, "attempts")?);
    }
    if let Some(backoff) = open.backoff {
        builder = builder.backoff(span(backoff.initial), span(backoff.max));
    }
    if let Some(timeout) = open.timeout {
        builder = builder.timeout(span(timeout));
    }
    Ok(builder.open()?.queue().retyped())
}

/// Runs an add, a set or an update on the job a call names.
fn on_job<T>(
    handles: &Handles,
    call: JobsCall,
    write: impl FnOnce(JobCall<'_, Raw>, Raw) -> crate::Result<T>,
) -> Result<T, Failure> {
    let opened = handles.opened(call.handle)?;
    if opened.schedule {
        return Err(Failure::invalid("a schedule's one job is its repeat, which no call adds, sets or updates"));
    }
    let value = RawValue::from_string(call.value)
        .map_err(|error| Failure::invalid(format!("a value that is not JSON: {error}")))?;
    let mut job = JobCall::new(&opened.queue);
    if let Some(id) = &call.id {
        job = job.id(id);
    }
    if let Some(at) = call.at {
        job = job.at(from_unix_millis(at));
    }
    if let Some(delay) = call.delay {
        job = job.delay(span(delay));
    }
    if let Some(group) = &call.group {
        job = job.group(group);
    }
    job = match (call.every, &call.cron, &call.time_zone) {
        (None, None, None) => job,
        (Some(every), None, None) => job.every(span(every)),
        (None, Some(cron), zone) => job.cron(cron, zone.as_deref().unwrap_or_default()),
        _ => return Err(Failure::invalid("a repeat by every or by cron, not both; a time zone is a cron's")),
    };
    Ok(write(job, value)?)
}

/// A page of jobs within the body agreed: a page past it is read again with
/// half its jobs.
fn list(handles: &Handles, list: JobsList, max_body: usize) -> Answered {
    let queue = handles.opened(list.handle)?.queue;
    let mut filter = Filter::prefix(list.prefix.unwrap_or_default());
    if let Some(state) = &list.state {
        filter = filter.state(state_of(state)?);
    }
    if let Some(after) = list.after {
        filter = filter.after(after);
    }
    let mut limit = list.limit.map_or(100, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
    loop {
        let page = queue.list(&filter.clone().limit(limit))?;
        let shown = page.jobs.len();
        let encoded = JobsPage { jobs: page.jobs.into_iter().map(job_of).collect(), next: page.next }.encode();
        if encoded.len() <= max_body || shown <= 1 {
            return within(encoded, max_body);
        }
        limit = shown / 2;
    }
}

/// The answer a held job's run kept under a step's name.
fn step(handles: &Handles, step: JobsStep, max_body: usize) -> Answered {
    let (lease, queue) = handles.held(step.run)?;
    let named = |error: Error| queue.fail(lease.key.as_deref(), error.within(format!("step {:?}", step.name)));
    let answer = kept(&queue.jobs, &lease, &step.name).map_err(named)?;
    let answer = answer
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| named(Error::new(ErrorKind::Corrupt, "a kept answer that is not UTF-8")))?;
    within(JobsKept { found: answer.is_some(), answer }.encode(), max_body)
}

/// Keeps a step's answer for a held job's run, while its attempt holds the job.
fn keep(handles: &Handles, step: JobsStep) -> Answered {
    let (lease, queue) = handles.held(step.run)?;
    let answer = step.answer.ok_or_else(|| Failure::invalid("a keep without its answer"))?;
    serde_json::from_str::<&RawValue>(&answer)
        .map_err(|error| Failure::invalid(format!("a step's answer that is not JSON: {error}")))?;
    keep_encoded(&queue.jobs, &lease, &step.name, answer.into_bytes())
        .map_err(|error| queue.fail(lease.key.as_deref(), error.within(format!("step {:?}", step.name))))?;
    Ok(Empty {}.encode())
}

fn job_of(job: Job<Raw>) -> JobsJob {
    JobsJob {
        found: true,
        id: job.id,
        value: Some(text_of(job.value)),
        state: state_name(job.state).to_owned(),
        at: Some(unix_millis(job.at)),
        attempt: u64::from(job.attempt),
        ahead: u64::from(job.ahead),
        progress: job.progress.map(|progress| progress.to_string()),
        error: job.error,
        group: job.group,
        repeat: job.repeat,
        started_at: job.last_run.map(|run| unix_millis(run.started_at)),
        ended_at: job.last_run.map(|run| unix_millis(run.ended_at)),
    }
}

fn state_name(state: State) -> &'static str {
    match state {
        State::Scheduled => "scheduled",
        State::Waiting => "waiting",
        State::Running => "running",
        State::Done => "done",
        State::Failed => "failed",
        State::Cancelled => "cancelled",
    }
}

fn state_of(name: &str) -> Result<State, Failure> {
    match name {
        "scheduled" => Ok(State::Scheduled),
        "waiting" => Ok(State::Waiting),
        "running" => Ok(State::Running),
        "failed" => Ok(State::Failed),
        other => Err(Failure::invalid(format!("a page of {other:?} jobs: scheduled, waiting, running or failed"))),
    }
}

/// What a work stream needs of its connection.
pub(crate) struct Link {
    pub(crate) send: Sender,
    /// Ends the stream when the worker's loop ended by itself.
    pub(crate) finish: Finisher,
    /// The DATA bytes the client's HELLO lets the server send before it grants
    /// more.
    pub(crate) credit: u64,
    pub(crate) max_body: usize,
}

/// A client's worker: the loop on the server, and the stream that hands its
/// jobs to the client.
pub(crate) struct Work {
    number: u32,
    works: Weak<Works>,
    remote: Remote,
    stream: Arc<Stream>,
    /// The client's DATA bytes let go of since they were last granted back.
    released: Mutex<u64>,
}

/// Starts the worker a `jobs.work` asks for. Its stream sends nothing until
/// [`Work::begin`], once the stream's `RESPONSE` went out.
pub(crate) fn work(handles: &Handles, number: u32, body: &[u8], link: Link) -> Result<Arc<Work>, Failure> {
    let asked = JobsWork::decode(body)?;
    let queue = handles.opened(asked.handle)?.queue;
    let local = match asked.concurrency {
        None => u64::from(queue.state.policy.local()).min(MAX_HANDLERS),
        Some(0) => return Err(Failure::invalid("a worker of no handlers")),
        Some(handlers) if handlers > MAX_HANDLERS => {
            return Err(Failure::invalid(format!("a worker of {handlers} handlers, past {MAX_HANDLERS}")));
        }
        Some(handlers) => handlers,
    };
    let local = usize::try_from(local).unwrap_or(1);
    let answers = Answers::default();
    let stream = Arc::new_cyclic(|me| Stream {
        me: me.clone(),
        queue: queue.clone(),
        answers: answers.clone(),
        runs: Arc::clone(&handles.runs),
        local,
        state: Mutex::new(StreamState { credit: link.credit, ..StreamState::default() }),
        link,
    });
    let works = Arc::downgrade(&handles.works);
    let remote = start_remote(
        &queue.jobs,
        &queue.state,
        RemoteWork {
            local,
            until_idle: asked.until_idle,
            answers,
            hand: Arc::clone(&stream) as Arc<dyn Hand>,
            ended: on_loop_end(Arc::downgrade(&stream), Weak::clone(&works), number),
        },
    )?;
    let work = Arc::new(Work { number, works, remote, stream, released: Mutex::new(0) });
    handles.lock_works().insert(number, Arc::clone(&work));
    Ok(work)
}

/// What hears a worker's loop end by itself, once nothing was due or the
/// store closed: the stream ends then, unless the session ended it first, or
/// at its begin when it had not begun.
fn on_loop_end(stream: Weak<Stream>, works: Weak<Works>, number: u32) -> Ended {
    Box::new(move |ran| {
        let Some(ran) = stream.upgrade().and_then(|stream| stream.loop_ended(ran)) else {
            return;
        };
        let taken = works.upgrade().and_then(|works| locked(&works).remove(&number));
        if let Some(work) = taken {
            work.finish(ran);
        }
    })
}

impl Work {
    /// Lets the stream send what its worker hands it: its `RESPONSE` went out.
    /// A worker whose loop ended before, nothing being due, ends here.
    pub(crate) fn begin(&self) {
        let Some(ran) = self.stream.begin() else {
            return;
        };
        let taken = self.works.upgrade().and_then(|works| locked(&works).remove(&self.number));
        if let Some(work) = taken {
            work.finish(ran);
        }
    }

    /// Ends a worker whose loop ended by itself, already out of use, and its
    /// stream: `{}` once nothing was due, an error when the store closed or
    /// the loop failed.
    fn finish(&self, ran: crate::Result<usize>) {
        self.end();
        let last = match ran {
            Ok(_) if self.stream.queue.jobs.closed() => {
                Err(Failure::from(Error::closed(self.stream.queue.state.describe())))
            }
            Ok(_) => Ok(Empty {}.encode()),
            Err(error) => Err(Failure::from(error)),
        };
        (self.stream.link.finish)(last);
    }

    /// The client's item: an answer for a job it holds, a progress, or a stop.
    pub(crate) fn item(&self, body: &[u8]) -> Result<(), Failure> {
        let answer = JobsAnswer::decode(body)?;
        match answer.how.as_str() {
            "stop" => self.halt(),
            "progress" => self.stream.progress(answer.run, answer.progress)?,
            _ => {
                let run = answer.run;
                let how = how_of(answer, self.stream.queue.jobs.now())?;
                self.stream.answer(run, how);
            }
        }
        Ok(())
    }

    /// Credit the client granted the stream for the server's DATA.
    pub(crate) fn grant(&self, credit: u32) {
        self.stream.lock().credit += u64::from(credit);
        self.stream.flush();
    }

    /// Takes no job more: the client asked to stop, or the server is closing.
    /// The jobs the client holds are still its to answer.
    pub(crate) fn halt(&self) {
        self.stream.lock().halted = true;
        self.remote.halt();
    }

    /// Ends the worker: the jobs the client holds unanswered fail their
    /// attempt, as a worker's that died, those never sent go back uncounted,
    /// and the loop writes what was answered. It waits for the loop, so it
    /// runs off the connection's own thread.
    pub(crate) fn end(&self) {
        for lease in self.stream.end() {
            let cause = "the client's worker ended with the job in hand".to_owned();
            self.stream.answers.answer(lease, Some(How::Retry { at: None, cause }));
        }
        self.remote.stop();
    }

    /// Counts the bytes of the client's DATA let go of, and says what to grant
    /// back once half the stream's window has gone.
    pub(crate) fn release(&self, bytes: u64, window: u64) -> Option<u32> {
        let mut released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
        *released += bytes;
        if *released < window / 2 {
            return None;
        }
        Some(u32::try_from(std::mem::take(&mut *released)).unwrap_or(u32::MAX))
    }
}

/// How a client's answer settles its job; a time given as a delay counts from
/// `now`, the server's clock.
fn how_of(answer: JobsAnswer, now: i64) -> Result<How, Failure> {
    let at = match (answer.at, answer.delay) {
        (Some(_), Some(_)) => return Err(Failure::invalid("an answer's time given twice: at or delay, not both")),
        (at, None) => at,
        (None, Some(delay)) => Some(now.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX))),
    };
    Ok(match answer.how.as_str() {
        "done" => How::Done,
        "retry" => How::Retry { at, cause: answer.error.unwrap_or_else(|| "its handler asked for a retry".to_owned()) },
        "snooze" => How::Snooze { at: at.ok_or_else(|| Failure::invalid("a snooze without its time"))? },
        "fail" => How::Fail { cause: answer.error.unwrap_or_else(|| "its handler failed it".to_owned()) },
        "back" => How::GiveBack,
        other => {
            let known = "done, retry, snooze, fail, back, progress or stop";
            return Err(Failure::invalid(format!("an answer of {other:?}, not {known}")));
        }
    })
}

/// A work stream's side of its worker: the jobs the loop hands it go out in
/// the order handed, no more unanswered at once than the client's handlers,
/// within the credit the client granted.
struct Stream {
    me: Weak<Stream>,
    queue: Queue<Raw>,
    answers: Answers,
    link: Link,
    runs: Arc<AtomicU64>,
    /// The client's handlers.
    local: usize,
    state: Mutex<StreamState>,
}

#[derive(Default)]
struct StreamState {
    /// The DATA bytes the client still takes.
    credit: u64,
    /// The jobs it holds by run, sent or waiting to be.
    held: HashMap<u64, Holding>,
    /// Held jobs in the order handed, waiting for a handler and credit.
    waiting: VecDeque<(u64, Vec<u8>)>,
    /// Notices of cancels, which go before the jobs waiting.
    notices: VecDeque<Vec<u8>>,
    /// Jobs sent and not yet answered.
    out: usize,
    /// Its `RESPONSE` went out, and DATA may follow.
    begun: bool,
    /// It sends no job more.
    halted: bool,
    /// It sends nothing more: its last frame comes.
    ended: bool,
    /// How the worker's loop ended, when it ended by itself before the stream
    /// began.
    ran: Option<crate::Result<usize>>,
}

struct Holding {
    lease: Arc<Lease>,
    sent: bool,
}

impl Hand for Stream {
    fn hand(&self, lease: Arc<Lease>) -> Result<(), Option<How>> {
        if lease.cancelled() {
            return Err(None);
        }
        if self.lock().halted {
            return Err(Some(How::GiveBack));
        }
        let (run, body) = self.held_job(&lease).map_err(Some)?;
        {
            let mut state = self.lock();
            if state.halted {
                return Err(Some(How::GiveBack));
            }
            state.held.insert(run, Holding { lease: Arc::clone(&lease), sent: false });
            state.waiting.push_back((run, body));
        }
        let me = self.me.clone();
        lease.when_cancelled(Box::new(move || {
            if let Some(stream) = me.upgrade() {
                stream.cancelled(run);
            }
        }));
        self.flush();
        Ok(())
    }

    fn close(&self) -> Vec<Arc<Lease>> {
        let mut state = self.lock();
        state.halted = true;
        let unsent: Vec<u64> = state.held.iter().filter(|(_, holding)| !holding.sent).map(|(run, _)| *run).collect();
        state.waiting.clear();
        unsent.into_iter().filter_map(|run| state.held.remove(&run)).map(|holding| holding.lease).collect()
    }
}

impl Stream {
    /// Lets the stream send: its `RESPONSE` went out. A loop that ended
    /// before gives how it ended.
    fn begin(&self) -> Option<crate::Result<usize>> {
        let ran = {
            let mut state = self.lock();
            state.begun = true;
            state.ran.take()
        };
        if ran.is_none() {
            self.flush();
        }
        ran
    }

    /// Hears that the worker's loop ended by itself, and gives that back when
    /// the stream has begun; one not yet begun keeps it for its begin.
    fn loop_ended(&self, ran: crate::Result<usize>) -> Option<crate::Result<usize>> {
        let mut state = self.lock();
        if state.begun {
            return Some(ran);
        }
        state.ran = Some(ran);
        None
    }

    /// The held job a lease goes out as, and its run; a value that cannot be
    /// read or sent says how to settle the job instead.
    fn held_job(&self, lease: &Lease) -> Result<(u64, Vec<u8>), How> {
        let value = match read_value::<Raw>(&self.queue.jobs, lease) {
            Ok(value) => value,
            Err(error) if error.kind() == ErrorKind::Invalid => return Err(How::Fail { cause: error.to_string() }),
            Err(error) => return Err(How::Retry { at: None, cause: error.to_string() }),
        };
        let run = self.runs.fetch_add(1, Ordering::Relaxed) + 1;
        let held = JobsHeld {
            run,
            id: lease.key.clone(),
            value: Some(text_of(value)),
            at: lease.at,
            attempt: u64::try_from(lease.attempt).unwrap_or(0),
            group: lease.group.clone(),
            cancelled: false,
        };
        let body = held.encode();
        if body.len() > self.link.max_body {
            // another worker, whose client takes larger bodies, may run it
            let cause = format!("a job of {} bytes, past the {} its client takes", body.len(), self.link.max_body);
            return Err(How::Retry { at: None, cause });
        }
        Ok((run, body))
    }

    /// Sends what waits while the client's credit takes it: the notices
    /// first, then the jobs in order while a handler is free. A job sent is
    /// the client's to answer; one a cancel took first goes nowhere.
    fn flush(&self) {
        let mut taken = Vec::new();
        let mut state = self.lock();
        let now = self.queue.jobs.now();
        while state.begun && !state.ended {
            if let Some(size) = state.notices.front().map(|notice| notice.len() as u64) {
                if size > state.credit {
                    break;
                }
                state.credit -= size;
                if let Some(notice) = state.notices.pop_front() {
                    (self.link.send)(notice);
                }
                continue;
            }
            if state.halted || state.out >= self.local {
                break;
            }
            let Some((run, size)) = state.waiting.front().map(|(run, body)| (*run, body.len() as u64)) else {
                break;
            };
            let Some(holding) = state.held.get(&run) else {
                state.waiting.pop_front();
                continue;
            };
            if size > state.credit {
                break;
            }
            if !holding.lease.begin(now) {
                taken.extend(state.held.remove(&run).map(|holding| holding.lease));
                state.waiting.pop_front();
                continue;
            }
            if let Some((_, body)) = state.waiting.pop_front() {
                state.credit -= size;
                state.out += 1;
                if let Some(holding) = state.held.get_mut(&run) {
                    holding.sent = true;
                }
                (self.link.send)(body);
            }
        }
        drop(state);
        for lease in taken {
            self.answers.answer(lease, None);
        }
    }

    /// The client's answer for a job it was sent; one a cancel took first, or
    /// a run it never was sent, is dropped.
    fn answer(&self, run: u64, how: How) {
        let mut state = self.lock();
        let lease = match state.held.remove(&run) {
            Some(holding) if holding.sent => holding.lease,
            Some(unsent) => {
                state.held.insert(run, unsent);
                return;
            }
            None => return,
        };
        state.out -= 1;
        drop(state);
        self.answers.answer(lease, Some(how));
        self.flush();
    }

    /// What the handler of a job the client was sent reports of it; one that
    /// is not JSON, or past 4 KiB, fails the stream.
    fn progress(&self, run: u64, report: Option<String>) -> Result<(), Failure> {
        let report = report.ok_or_else(|| Failure::invalid("a progress without its report"))?;
        if report.len() > MAX_PROGRESS {
            let refused = format!("a progress of {} bytes, past {MAX_PROGRESS}", report.len());
            return Err(Failure::from(Error::limit(refused)));
        }
        let report = serde_json::from_str(&report)
            .map_err(|error| Failure::invalid(format!("a progress that is not JSON: {error}")))?;
        if let Some(holding) = self.lock().held.get(&run).filter(|holding| holding.sent) {
            holding.lease.set_progress(report);
        }
        Ok(())
    }

    /// Answers a job a cancel took for the client, and tells the client when it
    /// was sent the job, so that its handler stops: what the client says of it
    /// later settles nothing.
    fn cancelled(&self, run: u64) {
        let mut state = self.lock();
        let Some(holding) = state.held.remove(&run) else {
            return;
        };
        if holding.sent {
            state.out -= 1;
            state.notices.push_back(JobsHeld { run, cancelled: true, ..JobsHeld::default() }.encode());
        }
        drop(state);
        self.answers.answer(holding.lease, None);
        self.flush();
    }

    /// The job a run names, sent to the client and not yet answered.
    fn held(&self, run: u64) -> Option<Arc<Lease>> {
        self.lock().held.get(&run).filter(|holding| holding.sent).map(|holding| Arc::clone(&holding.lease))
    }

    /// Sends nothing more, and lets go of the jobs the client was sent and has
    /// not answered.
    fn end(&self) -> Vec<Arc<Lease>> {
        let mut state = self.lock();
        (state.halted, state.ended, state.out) = (true, true, 0);
        let sent: Vec<u64> = state.held.iter().filter(|(_, holding)| holding.sent).map(|(run, _)| *run).collect();
        sent.into_iter().filter_map(|run| state.held.remove(&run)).map(|holding| holding.lease).collect()
    }

    fn lock(&self) -> MutexGuard<'_, StreamState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Handles {
    fn open(&self, queue: Queue<Raw>, schedule: bool) -> Answered {
        let handle = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        self.lock().insert(handle, Opened { queue, schedule });
        Ok(Handle { handle }.encode())
    }

    fn opened(&self, handle: u64) -> Result<Opened, Failure> {
        self.lock()
            .get(&handle)
            .cloned()
            .ok_or_else(|| Failure::invalid(format!("handle {handle} names no queue this connection opened")))
    }

    /// The job a run of this connection's workers holds, and its queue.
    fn held(&self, run: u64) -> Result<(Arc<Lease>, Queue<Raw>), Failure> {
        let works: Vec<Arc<Work>> = self.lock_works().values().cloned().collect();
        let found = works.iter().find_map(|work| work.stream.held(run).map(|lease| (lease, work.stream.queue.clone())));
        found.ok_or_else(|| {
            let ended = format!("run {run} holds no job: it was answered, a cancel took it, or its worker ended");
            Failure::from(Error::new(ErrorKind::Conflict, ended))
        })
    }

    /// The worker on a stream.
    pub(crate) fn work(&self, stream: u32) -> Option<Arc<Work>> {
        self.lock_works().get(&stream).cloned()
    }

    /// Takes the worker on a stream out of use, its stream's last frame to come.
    pub(crate) fn take_work(&self, stream: u32) -> Option<Arc<Work>> {
        self.lock_works().remove(&stream)
    }

    /// Takes every worker out of use: the connection ends.
    pub(crate) fn take_works(&self) -> Vec<Arc<Work>> {
        self.lock_works().drain().map(|(_, work)| work).collect()
    }

    /// Halts every worker: the server is closing.
    pub(crate) fn halt_works(&self) {
        let works: Vec<Arc<Work>> = self.lock_works().values().cloned().collect();
        for work in works {
            work.halt();
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Opened>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_works(&self) -> MutexGuard<'_, HashMap<u32, Arc<Work>>> {
        locked(&self.works)
    }
}

fn locked(works: &Works) -> MutexGuard<'_, HashMap<u32, Arc<Work>>> {
    works.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An answer within the body agreed: a larger one is `limit`.
fn within(encoded: Vec<u8>, max_body: usize) -> Answered {
    if encoded.len() > max_body {
        let refused = format!("an answer of {} bytes, past the {max_body} this connection agreed", encoded.len());
        return Err(Failure::from(Error::limit(refused)));
    }
    Ok(encoded)
}

fn text_of(value: Raw) -> String {
    Box::<str>::from(value).into_string()
}

fn span(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn small(n: u64, what: &str) -> Result<u32, Failure> {
    u32::try_from(n).map_err(|_| Failure::invalid(format!("{what} of {n}, past {}", u32::MAX)))
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
