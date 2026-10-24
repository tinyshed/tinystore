use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::Config;
use super::connection::{self, Role, execute, sql_error};
use super::given::own;
use crate::cancel::{self, Given};
use crate::{Error, ErrorKind, Result};

/// How often a reader that comes back goes to the read that has waited
/// longest, rather than to the first thread that asks. A thread already
/// running uses a reader at once where a woken one idles it for a wake; but
/// left to that alone, a read woke to find its reader taken, slept again, and
/// waited while thousands passed it.
const FAIR_EVERY: Duration = Duration::from_millis(1);

/// A file's readers: `query_only` connections opened as reads need them, up to
/// the configured count. One stays open; the others close once unused for the
/// configured time, since a burst of reads otherwise keeps their memory for
/// good.
pub(crate) struct Readers {
    path: PathBuf,
    config: Arc<Config>,
    pool: Mutex<Pool>,
}

#[derive(Default)]
struct Pool {
    /// The least recently used first.
    idle: Vec<Idle>,
    open: usize,
    /// Reads waiting for a reader, the longest waiting first. A reader that
    /// comes back wakes the first only when there is one: a wake is a system
    /// call, and it was one a read.
    waiting: VecDeque<Waiter>,
    /// Readers given to a waiting read, with its place in line, until it
    /// wakes to take them.
    handed: Vec<(Waiter, Connection)>,
    tickets: u64,
    /// When a reader that comes back next goes to the longest waiting read.
    fair_from: Option<Instant>,
    closed: bool,
    /// The file is being replaced, a copy by a snapshot: no reader opens, one
    /// that comes back closes, and reads wait.
    paused: bool,
}

struct Idle {
    connection: Connection,
    since: Instant,
}

struct Waiter {
    ticket: u64,
    thread: Thread,
    /// What wakes the thread when its call is cancelled, until it leaves the
    /// queue.
    _woken: Option<Given>,
}

impl Readers {
    pub(crate) fn new(path: PathBuf, config: Arc<Config>) -> Self {
        Self { path, config, pool: Mutex::new(Pool::default()) }
    }

    /// Runs `read` in one read transaction, so every statement it runs sees the
    /// same snapshot of the file. The snapshot covers fetching bytes: decode
    /// after it ends, or a heavy query holds the WAL from shrinking.
    ///
    /// A read whose call was cancelled takes no reader, stops waiting for one,
    /// and stops its statement, giving the reader back: it fails `Cancelled`.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let what = || format!("{}: a read", self.path.display());
        cancel::check(what)?;
        let lease = self.lease()?;
        snapshot(lease.connection(), read).map_err(|error| cancel::named(error, what))
    }

    /// Closes the readers unused for longer than the configured time, keeping
    /// the most recently used one open; says how many it closed.
    pub(crate) fn sweep(&self) -> usize {
        let stale = {
            let mut pool = self.lock();
            let now = Instant::now();
            let keep_from = pool
                .idle
                .iter()
                .position(|idle| now.duration_since(idle.since) < self.config.reader_idle)
                .unwrap_or(pool.idle.len())
                .min(pool.idle.len().saturating_sub(1));
            let stale: Vec<Idle> = pool.idle.drain(..keep_from).collect();
            pool.open -= stale.len();
            stale
        };
        stale.len()
    }

    /// Closes the idle readers and those in use as they come back; reads that
    /// wait for a reader fail as closed.
    pub(crate) fn close(&self) {
        let (idle, handed, waiting) = {
            let mut pool = self.lock();
            pool.closed = true;
            let idle = std::mem::take(&mut pool.idle);
            let handed = std::mem::take(&mut pool.handed);
            pool.open -= idle.len() + handed.len();
            let given = handed.iter().map(|(waiter, _)| waiter);
            let waiting: Vec<Thread> = pool.waiting.iter().chain(given).map(|waiter| waiter.thread.clone()).collect();
            (idle, handed, waiting)
        };
        drop((idle, handed));
        waiting.iter().for_each(Thread::unpark);
    }

    /// The read connections open now.
    pub(crate) fn open(&self) -> usize {
        self.lock().open
    }

    /// Closes every reader, the idle at once and those in use as they come
    /// back, and holds new reads waiting until `resume`: a copy's file is then
    /// open nowhere, and a snapshot can take its place. A read still under way
    /// after `at_most` fails the pause, and the readers go on.
    ///
    /// A reader given to a read that has not woken to take it yet closes too,
    /// and that read goes back to the front of the line, where `resume` wakes
    /// it: out of the line, it would sleep out its whole patience.
    #[cfg(feature = "replication")]
    pub(crate) fn pause(&self, at_most: std::time::Duration) -> Result<()> {
        let (idle, handed) = {
            let mut pool = self.lock();
            pool.paused = true;
            let idle = std::mem::take(&mut pool.idle);
            let handed = std::mem::take(&mut pool.handed);
            pool.open -= idle.len() + handed.len();
            let mut taken = Vec::with_capacity(handed.len());
            for (waiter, connection) in handed.into_iter().rev() {
                pool.waiting.push_front(waiter);
                taken.push(connection);
            }
            (idle, taken)
        };
        drop((idle, handed));
        let deadline = Instant::now() + at_most;
        while self.lock().open > 0 {
            if Instant::now() >= deadline {
                self.resume();
                return Err(Error::new(
                    ErrorKind::Unavailable,
                    format!("{}: a read went on past the pause of its readers", self.path.display()),
                ));
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(())
    }

    /// Lets reads open readers again after a `pause`.
    #[cfg(feature = "replication")]
    pub(crate) fn resume(&self) {
        let waiting: Vec<Thread> = {
            let mut pool = self.lock();
            pool.paused = false;
            pool.waiting.iter().map(|waiter| waiter.thread.clone()).collect()
        };
        waiting.iter().for_each(Thread::unpark);
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.lock().waiting.len()
    }

    fn lease(&self) -> Result<Lease<'_>> {
        let connection = self.take()?;
        Ok(Lease { readers: self, connection: Some(connection) })
    }

    fn take(&self) -> Result<Connection> {
        if crate::polled::waits_refused() {
            return self.take_idle();
        }
        // a write's submit reads nothing: a read with a reader idle may still run long
        crate::polled::may_wait(|| format!("{}: a read", self.path.display()))?;
        let deadline = Instant::now() + self.config.reader_patience;
        let mut ticket = None;
        let mut pool = self.lock();
        loop {
            if let Some(connection) = ticket.and_then(|ticket| pool.handed_to(ticket)) {
                return Ok(connection);
            }
            if pool.closed {
                pool.leave(ticket);
                return Err(Error::closed(format!("{}: a read", self.path.display())));
            }
            if let Some(idle) = pool.idle.pop() {
                pool.leave(ticket);
                return Ok(idle.connection);
            }
            if pool.open < self.config.readers && !pool.paused {
                pool.leave(ticket);
                pool.open += 1;
                drop(pool);
                return self.open_one();
            }
            if let Err(cancelled) = cancel::check(|| format!("{}: a read", self.path.display())) {
                pool.leave(ticket);
                return Err(cancelled);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                pool.leave(ticket);
                let why = if pool.paused {
                    "its readers are paused while a snapshot takes the file's place".to_owned()
                } else {
                    format!("all {} readers are busy", self.config.readers)
                };
                return Err(Error::new(ErrorKind::Unavailable, format!("{}: {why}", self.path.display())));
            }
            if ticket.is_none() {
                // a cancel from here on wakes the thread: look again before it sleeps
                ticket = Some(pool.queue());
                continue;
            }
            drop(pool);
            let timed_out = cancel::deadline().map(|at| at.saturating_duration_since(Instant::now()));
            thread::park_timeout(timed_out.map_or(left, |timed_out| timed_out.min(left)));
            pool = self.lock();
        }
    }

    /// A reader for a point read tried in the poll of an awaited call: one
    /// idle when no read waits in line for one, or the call is refused, to
    /// run again on the pool. Opening a reader is a file's open and its
    /// pragmas, which the pool does.
    fn take_idle(&self) -> Result<Connection> {
        let mut pool = self.lock();
        if pool.closed {
            return Err(Error::closed(format!("{}: a read", self.path.display())));
        }
        if pool.waiting.is_empty()
            && !pool.paused
            && let Some(idle) = pool.idle.pop()
        {
            return Ok(idle.connection);
        }
        drop(pool);
        Err(crate::polled::refused(|| format!("{}: a read with no reader idle", self.path.display())))
    }

    fn open_one(&self) -> Result<Connection> {
        connection::open(&self.path, &self.config, Role::Reader).inspect_err(|_| self.discard())
    }

    fn give_back(&self, connection: Connection) {
        let mut pool = self.lock();
        if pool.closed || pool.paused {
            pool.open -= 1;
            drop(pool);
            drop(connection);
            return;
        }
        let wake = pool.give(connection);
        drop(pool);
        if let Some(thread) = wake {
            thread.unpark();
        }
    }

    fn discard(&self) {
        let mut pool = self.lock();
        pool.open -= 1;
        let wake = pool.waiting.front().map(|waiter| waiter.thread.clone());
        drop(pool);
        if let Some(thread) = wake {
            thread.unpark();
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Pool {
    /// Takes a reader back: to the longest waiting read once a fair turn is
    /// due, otherwise to the idle, waking that read to try for it. Says whom
    /// to wake.
    fn give(&mut self, connection: Connection) -> Option<Thread> {
        let now = Instant::now();
        let due = self.fair_from.is_none_or(|from| now >= from);
        if due && let Some(oldest) = self.waiting.pop_front() {
            self.fair_from = Some(now + FAIR_EVERY);
            let thread = oldest.thread.clone();
            self.handed.push((oldest, connection));
            return Some(thread);
        }
        self.idle.push(Idle { connection, since: now });
        self.waiting.front().map(|waiter| waiter.thread.clone())
    }

    fn queue(&mut self) -> u64 {
        let ticket = self.tickets;
        self.tickets += 1;
        let thread = thread::current();
        let woken = cancel::on_cancel({
            let thread = thread.clone();
            move || thread.unpark()
        });
        self.waiting.push_back(Waiter { ticket, thread, _woken: woken });
        ticket
    }

    fn leave(&mut self, ticket: Option<u64>) {
        if let Some(ticket) = ticket {
            self.waiting.retain(|waiter| waiter.ticket != ticket);
        }
    }

    fn handed_to(&mut self, ticket: u64) -> Option<Connection> {
        let at = self.handed.iter().position(|(to, _)| to.ticket == ticket)?;
        Some(self.handed.swap_remove(at).1)
    }
}

/// A reader taken from the pool, given back when dropped; one left inside a
/// transaction, or dropped by a panic, is closed rather than reused.
struct Lease<'a> {
    readers: &'a Readers,
    connection: Option<Connection>,
}

impl Lease<'_> {
    fn connection(&self) -> &Connection {
        self.connection.as_ref().expect("a lease holds its connection until it is dropped")
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if std::thread::panicking() || !connection.is_autocommit() {
            drop(connection);
            self.readers.discard();
        } else {
            self.readers.give_back(connection);
        }
    }
}

const BEGIN_READ: &str = own!("begin");
const END_READ: &str = own!("commit");

fn snapshot<T>(connection: &Connection, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    execute(connection, BEGIN_READ).map_err(|error| sql_error("a read: its snapshot", error))?;
    let outcome = interruptible(connection, read);
    if connection.is_autocommit() {
        return outcome;
    }
    match execute(connection, END_READ) {
        Err(error) if outcome.is_ok() => Err(sql_error("a read: its snapshot", error)),
        _ => outcome,
    }
}

/// Runs `read` with the reader given to the tokens of its call, which
/// interrupt the reader when cancelled, until `read` returns: the reader then
/// goes back to the call's own last statement, which no cancel reaches.
fn interruptible<T>(connection: &Connection, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let _given = cancel::in_scope().then(|| {
        let interrupt = connection.get_interrupt_handle();
        cancel::on_cancel(move || interrupt.interrupt())
    });
    // a cancel that came before the reader was given interrupted nothing
    cancel::check(|| "a read".to_owned())?;
    read(connection)
}

#[cfg(test)]
#[path = "readers_tests.rs"]
mod tests;
