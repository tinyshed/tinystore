use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::Config;
use super::connection::{self, Role, execute, sql_error};
use super::given::own;
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
    /// Readers given to a waiting read, by its ticket, until it wakes to take them.
    handed: Vec<(u64, Connection)>,
    tickets: u64,
    /// When a reader that comes back next goes to the longest waiting read.
    fair_from: Option<Instant>,
    closed: bool,
}

struct Idle {
    connection: Connection,
    since: Instant,
}

struct Waiter {
    ticket: u64,
    thread: Thread,
}

impl Readers {
    pub(crate) fn new(path: PathBuf, config: Arc<Config>) -> Self {
        Self { path, config, pool: Mutex::new(Pool::default()) }
    }

    /// Runs `read` in one read transaction, so every statement it runs sees the
    /// same snapshot of the file. The snapshot covers fetching bytes: decode
    /// after it ends, or a heavy query holds the WAL from shrinking.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let lease = self.lease()?;
        snapshot(lease.connection(), read)
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
            let waiting: Vec<Thread> = pool.waiting.iter().map(|waiter| waiter.thread.clone()).collect();
            (idle, handed, waiting)
        };
        drop((idle, handed));
        waiting.iter().for_each(Thread::unpark);
    }

    pub(crate) fn open(&self) -> usize {
        self.lock().open
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
            if pool.open < self.config.readers {
                pool.leave(ticket);
                pool.open += 1;
                drop(pool);
                return self.open_one();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                pool.leave(ticket);
                return Err(Error::new(
                    ErrorKind::Unavailable,
                    format!("{}: all {} readers are busy", self.path.display(), self.config.readers),
                ));
            }
            if ticket.is_none() {
                ticket = Some(pool.queue());
            }
            drop(pool);
            thread::park_timeout(left);
            pool = self.lock();
        }
    }

    fn open_one(&self) -> Result<Connection> {
        connection::open(&self.path, &self.config, Role::Reader).inspect_err(|_| self.discard())
    }

    fn give_back(&self, connection: Connection) {
        let mut pool = self.lock();
        if pool.closed {
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
            self.handed.push((oldest.ticket, connection));
            return Some(oldest.thread);
        }
        self.idle.push(Idle { connection, since: now });
        self.waiting.front().map(|waiter| waiter.thread.clone())
    }

    fn queue(&mut self) -> u64 {
        let ticket = self.tickets;
        self.tickets += 1;
        self.waiting.push_back(Waiter { ticket, thread: thread::current() });
        ticket
    }

    fn leave(&mut self, ticket: Option<u64>) {
        if let Some(ticket) = ticket {
            self.waiting.retain(|waiter| waiter.ticket != ticket);
        }
    }

    fn handed_to(&mut self, ticket: u64) -> Option<Connection> {
        let at = self.handed.iter().position(|(to, _)| *to == ticket)?;
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
    let outcome = read(connection);
    if connection.is_autocommit() {
        return outcome;
    }
    match execute(connection, END_READ) {
        Err(error) if outcome.is_ok() => Err(sql_error("a read: its snapshot", error)),
        _ => outcome,
    }
}
