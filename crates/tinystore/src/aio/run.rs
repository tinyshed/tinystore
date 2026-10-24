//! How an awaited call runs: the helpers every engine's async calls are
//! built on, so that each says where its work runs and what its future's
//! drop does in one word.
//!
//! | helper                | runs                                                 | its future dropped once polled  |
//! |-----------------------|------------------------------------------------------|---------------------------------|
//! | [`point`]             | in the first poll when a reader is idle, else pool   | answered in that poll, or stops |
//! | [`step`]              | in the first poll, else on the pool                  | applied all the same            |
//! | [`read`]              | on the store's pool, under a token of its own        | stops; its reader goes back     |
//! | [`write`]             | a submit in the first poll, its commit on the pool   | commits; its answer is let go   |
//! | [`reserve`]           | a waiter in the store's memory, on no thread         | leaves the line                 |
//! | [`blocking`]          | blocking code on the store's pool, in a line or not  | runs to its end                 |
//! | [`passed_on`]         | a call to a copy's home, on a thread of its own      | runs to its end                 |
//! | [`passed_on_in_line`] | its turn in a line of the pool, then as `passed_on`  | runs to its end                 |
//!
//! Each does nothing before its first poll. A call clones its handle and
//! owns its arguments, so that its future is `Send + 'static` and a program
//! can spawn it.
//!
//! The pool's sixteen threads at most are shared by every awaited call,
//! transaction and commit's lead, and by the pipe's connections, so a call
//! passed on to a copy's home waits for its answer on a thread of its own: a
//! round trip, or up to 25 s for a once run's claim, which the home answers
//! once the run under way there ends. Two waits for another node still hold
//! a thread of the pool: a read on a copy that passes an idle key's renewal
//! on, and a commit's lead at a home whose copies are synchronous.
//!
//! What runs in a poll, a point read or a write's submit, parks its thread
//! nowhere: each wait `polled::may_wait` guards refuses a point read, which
//! runs again on the pool, and fails a submit `Internal`. Some waits no check
//! guards yet, and an engine keeps them out of a poll itself: a search's
//! wait for its index to settle, a records tail's wait for what arrives, a
//! blob's sync of its directory and its retries of a rename, a channel's
//! receive, a job's watch and a worker's wait for jobs, and the waits of a
//! home with synchronous copies, inside a commit.

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use super::oneshot::{Answer, pending};
use crate::cancel::{self, Captured};
use crate::polled::{self, Lead};
use crate::pool::Pool;
use crate::{CancellationToken, Error, Memory, Reservation, Result};

/// A point read, as a key's value or a counter: tried in the future's first
/// poll, on the polling thread, where one of its file's readers is idle and
/// it answers in microseconds; run as [`read`] runs on the pool where it
/// would wait for a reader or open one, write, submitted or blocking, wait
/// for memory, or make any other wait `polled::may_wait` guards.
///
/// So `read` may run twice, in the poll up to a wait and then on the pool:
/// it must submit nothing and change nothing before its last possible wait,
/// nor after a wait it was refused, whose error it passes up unchanged. The
/// tokens around this call reach it in both places, as [`read`] says.
#[allow(dead_code, reason = "the engines' async point reads are built on it")]
pub(crate) fn point<T, R>(pool: &Arc<Pool>, what: String, read: R) -> impl Future<Output = Result<T>> + Send + 'static
where
    R: Fn() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tokens, pool) = (cancel::captured(), Arc::clone(pool));
    async move {
        if let Some(answer) = polled::without_waiting(|| tokens.run(None, &read)) {
            return answer;
        }
        on_the_pool(pool, tokens, what, read).await
    }
}

/// A step that changes what memory holds, as an add of counters kept in
/// memory or a rate limit's allow: tried in the future's first poll as
/// [`point`] tries a read, and run where it would wait there as [`blocking`]
/// runs, on the pool and to its end, whether or not anyone still awaits it. A
/// step is a write that commits with a later flush, so it must not stop half
/// way, as a read whose future is dropped does (DUR-11).
///
/// `step` may run twice, as [`point`]'s read may: it must change nothing
/// before its last possible wait, nor after a wait it was refused. The tokens
/// around this call reach it in both places, and stop its reads as they stop
/// its blocking twin's.
#[allow(dead_code, reason = "kv's awaited steps in memory are built on it")]
pub(crate) fn step<T, S>(pool: &Arc<Pool>, what: String, step: S) -> impl Future<Output = Result<T>> + Send + 'static
where
    S: Fn() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tokens, pool) = (cancel::captured(), Arc::clone(pool));
    async move {
        if let Some(answer) = polled::without_waiting(|| tokens.run(None, &step)) {
            return answer;
        }
        blocking(&pool, None, what, move || tokens.run(None, step)).await
    }
}

/// Runs `read` on the store's pool once the future is first polled, under a
/// token of its own that the future's drop cancels: a read whose future is
/// dropped stops where it is, its statement interrupted and its reader given
/// back at once, and one whose future went before a thread took it never
/// runs. `tokio::time::timeout` around the call stops it so.
///
/// The tokens of the thread that calls this, a program's
/// `token.run(|| db.all(q))`, reach the read on the pool too: an engine calls
/// it in its method's body, where the future is made, never inside an
/// `async` block, which runs where it is polled.
#[allow(dead_code, reason = "the engines' async reads are built on it")]
pub(crate) fn read<T, R>(pool: &Arc<Pool>, what: String, read: R) -> impl Future<Output = Result<T>> + Send + 'static
where
    R: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    on_the_pool(Arc::clone(pool), cancel::captured(), what, read)
}

async fn on_the_pool<T, R>(pool: Arc<Pool>, tokens: Captured, what: String, read: R) -> Result<T>
where
    R: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    refuse_inside_a_transaction(&what)?;
    let token = CancellationToken::new();
    let _dropped = CancelOnDrop(token.clone());
    let (answer, answered) = pending(what);
    pool.run(Box::new(move || {
        if !answer.is_awaited() {
            return;
        }
        match catch_unwind(AssertUnwindSafe(|| tokens.run(Some(&token), read))) {
            Ok(outcome) => answer.send(outcome),
            Err(payload) => answer.panicked(payload),
        }
    }));
    answered.await
}

/// Cancels an awaited read's own token as its future is dropped: what the
/// read runs then stops, and what it has not started never starts.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Takes `bytes` of the store's memory as [`Memory::reserve`] does, in the
/// same line, its waiters served in the order they came, but parks no
/// thread: the future is woken by whoever gives the bytes back, or fails
/// `Limit` on the store's timer once `patience` passed. Dropped while it
/// waits, it leaves the line, and the waiters behind it wait for less.
#[allow(dead_code, reason = "the engines' async writes that hold memory reserve through it")]
pub(crate) fn reserve(
    memory: &Arc<Memory>,
    bytes: u64,
    what: String,
    patience: Duration,
) -> impl Future<Output = Result<Reservation>> + Send + 'static {
    let memory = Arc::clone(memory);
    async move {
        let (answer, answered) = pending(what.clone());
        let _waiting = memory.reserve_then(bytes, &what, patience, move |reserved| answer.send(reserved));
        answered.await
    }
}

/// Runs `submit` in the future's first poll, on the polling thread, with
/// every commit a write it submits would lead there handed to the store's
/// pool, and answers with what `submit` gives its [`Answer`]: an engine's
/// `_then` form, its `done` sending the answer. A write that refuses before
/// it is queued, as one on a closed file, answers in that poll.
///
/// `submit` encodes its arguments and calls `File::submit`, nothing more: a
/// read, a blocking write or a patient reservation in it fails `Internal`,
/// and a wait `polled::may_wait` does not guard parks the executor. A form
/// that reads first, as sql's `rows_then` does, runs that read through
/// [`read`] or [`blocking`] before it submits, or is split in two.
///
/// Dropped before its first poll it writes nothing. Once polled the write
/// is queued, and it commits with its group whether or not anyone still
/// awaits it: only its answer is let go.
///
/// ```ignore
/// let (bucket, key, bytes) = (self.blocking.clone(), key.to_owned(), encoded);
/// run::write(&self.pool, format!("{bucket:?}: a set"), move |answer| {
///     bucket.put_raw_then(&key, bytes, move |written| answer.send(written))
/// })
/// ```
#[allow(dead_code, reason = "the engines' async writes are built on it")]
pub(crate) fn write<T, S>(pool: &Arc<Pool>, what: String, submit: S) -> impl Future<Output = Result<T>> + Send + 'static
where
    S: FnOnce(Answer<T>) + Send + 'static,
    T: Send + 'static,
{
    let lead = lead_on(pool);
    async move {
        let (answer, answered) = pending(what);
        polled::leading_on(&lead, || submit(answer));
        answered.await
    }
}

/// What hands a commit's lead to `pool`, or, once the pool stopped as its
/// store closes, to a thread of its own: never back to the polling thread.
pub(crate) fn lead_on(pool: &Arc<Pool>) -> Lead {
    let pool = Arc::clone(pool);
    Arc::new(move |job| pool.run_always(job))
}

/// Runs `work` on the store's pool once the future is first polled, after
/// the jobs queued on `line` before it when one is named, and answers with
/// what it returns. Once polled it runs to its end whether or not anyone
/// still awaits it: a transaction's closure, which holds its file's writer,
/// or a write the core answers no other way. A panic in `work` is resumed
/// where the future is awaited.
///
/// `what` names the call in its errors: `store data: a transaction`.
#[allow(dead_code, reason = "the store's transactions run on it with kv or jobs, and the engines' blocking work")]
pub(crate) fn blocking<T, F>(
    pool: &Arc<Pool>,
    line: Option<String>,
    what: String,
    work: F,
) -> impl Future<Output = Result<T>> + Send + 'static + use<T, F>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let pool = Arc::clone(pool);
    async move {
        refuse_inside_a_transaction(&what)?;
        let (answer, answered) = pending(what);
        let job = Box::new(move || match catch_unwind(AssertUnwindSafe(work)) {
            Ok(outcome) => answer.send(outcome),
            Err(payload) => answer.panicked(payload),
        });
        match line {
            Some(line) => pool.run_in_line(&line, job),
            None => pool.run(job),
        }
        answered.await
    }
}

/// The name of the threads that wait for a copy's home.
const PASSED_ON: &str = "tinystore-passed-on";

/// Runs `call`, which passes on to the home of a copy and blocks until the
/// home answers, on a thread of its own once the future is first polled, and
/// answers with what it returns. Once polled it runs to its end whether or
/// not anyone still awaits it, as a write does. A panic in `call` is resumed
/// where the future is awaited.
#[allow(dead_code, reason = "the calls of kv and blobs passed on to a copy's home run on it")]
pub(crate) async fn passed_on<T, F>(what: String, call: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    refuse_inside_a_transaction(&what)?;
    let (answer, answered) = pending(what.clone());
    let started = own_thread(move || match catch_unwind(AssertUnwindSafe(call)) {
        Ok(outcome) => answer.send(outcome),
        Err(payload) => answer.panicked(payload),
    });
    if let Err(error) = started {
        return Err(Error::io(format!("{what}: a thread to wait for the home's answer on"), error));
    }
    answered.await
}

/// Takes what a call to a copy's home needs with `take`, in its turn in
/// `line` of the store's pool, after the work the line held before it, then
/// runs `call` with it as [`passed_on`] does, on a thread of its own: an
/// upload's commit on a copy, whose pieces are written in its line and whose
/// home's answer would hold a thread of the pool. What `take` took is the
/// thread's, so that work the line holds after it finds it gone.
#[cfg(feature = "blobs")]
pub(crate) fn passed_on_in_line<A, T, Take, Call>(
    pool: &Arc<Pool>,
    line: String,
    what: String,
    (take, call): (Take, Call),
) -> impl Future<Output = Result<T>> + Send + 'static + use<A, T, Take, Call>
where
    Take: FnOnce() -> Result<A> + Send + 'static,
    Call: FnOnce(A) -> Result<T> + Send + 'static,
    A: Send + 'static,
    T: Send + 'static,
{
    let pool = Arc::clone(pool);
    async move {
        refuse_inside_a_transaction(&what)?;
        let (answer, answered) = pending(what.clone());
        let job = Box::new(move || match catch_unwind(AssertUnwindSafe(take)) {
            Ok(Ok(taken)) => pass_on_taken(what, answer, move || call(taken)),
            Ok(Err(error)) => answer.send(Err(error)),
            Err(payload) => answer.panicked(payload),
        });
        pool.run_in_line(&line, job);
        answered.await
    }
}

/// Runs `call` on a thread of its own and answers with it, or answers why no
/// thread started.
#[cfg(feature = "blobs")]
fn pass_on_taken<T: Send + 'static>(
    what: String,
    answer: Answer<T>,
    call: impl FnOnce() -> Result<T> + Send + 'static,
) {
    let handed = Arc::new(std::sync::Mutex::new(Some(answer)));
    let answering = Arc::clone(&handed);
    let started = own_thread(move || {
        if let Some(answer) = crate::sync::lock(&answering).take() {
            answer_with(answer, call);
        }
    });
    // a thread that never started dropped its job, and left the answer here
    if let Err(error) = started
        && let Some(answer) = crate::sync::lock(&handed).take()
    {
        answer.send(Err(Error::io(format!("{what}: a thread to wait for the home's answer on"), error)));
    }
}

/// Starts `job` on a thread of its own, one of those that wait for a copy's
/// home.
#[allow(dead_code, reason = "the calls of kv and blobs passed on to a copy's home run on it")]
pub(crate) fn own_thread(job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new().name(PASSED_ON.to_owned()).spawn(job).map(drop)
}

/// Refuses a call that would hand its work to the pool from inside a
/// transaction this thread holds, as a program's `block_on` in a
/// transaction's closure would: the pool's thread would wait for the writer
/// this one holds, or read past what the transaction wrote.
pub(crate) fn refuse_inside_a_transaction(what: &str) -> Result<()> {
    if !crate::sqlite::in_a_transaction() {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "{what}: an awaited call from inside a transaction would wait for the writer the transaction holds: \
         make it through the transaction, tx.with(&handle)"
    )))
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
