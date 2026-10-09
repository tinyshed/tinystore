use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use rusqlite::Connection;

use super::Config;
use super::connection::{self, Role, execute, sql_error};
use crate::{Error, ErrorKind, Result};

/// A file's readers: `query_only` connections opened as reads need them, up to
/// the configured count. One stays open; the others close once unused for the
/// configured time, since a burst of reads otherwise keeps their memory for
/// good.
pub(crate) struct Readers {
    path: PathBuf,
    config: Arc<Config>,
    pool: Mutex<Pool>,
    freed: Condvar,
}

#[derive(Default)]
struct Pool {
    /// The least recently used first.
    idle: Vec<Idle>,
    open: usize,
    closed: bool,
}

struct Idle {
    connection: Connection,
    since: Instant,
}

impl Readers {
    pub(crate) fn new(path: PathBuf, config: Arc<Config>) -> Self {
        Self { path, config, pool: Mutex::new(Pool::default()), freed: Condvar::new() }
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
        let idle = {
            let mut pool = self.lock();
            pool.closed = true;
            let idle = std::mem::take(&mut pool.idle);
            pool.open -= idle.len();
            idle
        };
        drop(idle);
        self.freed.notify_all();
    }

    pub(crate) fn open(&self) -> usize {
        self.lock().open
    }

    fn lease(&self) -> Result<Lease<'_>> {
        let connection = self.take()?;
        Ok(Lease { readers: self, connection: Some(connection) })
    }

    fn take(&self) -> Result<Connection> {
        let deadline = Instant::now() + self.config.reader_patience;
        let mut pool = self.lock();
        loop {
            if pool.closed {
                return Err(Error::closed(format!("{}: a read", self.path.display())));
            }
            if let Some(idle) = pool.idle.pop() {
                return Ok(idle.connection);
            }
            if pool.open < self.config.readers {
                pool.open += 1;
                drop(pool);
                return self.open_one();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::new(
                    ErrorKind::Unavailable,
                    format!("{}: all {} readers are busy", self.path.display(), self.config.readers),
                ));
            }
            pool = self.freed.wait_timeout(pool, left).unwrap_or_else(PoisonError::into_inner).0;
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
        pool.idle.push(Idle { connection, since: Instant::now() });
        drop(pool);
        self.freed.notify_one();
    }

    fn discard(&self) {
        self.lock().open -= 1;
        self.freed.notify_one();
    }

    fn lock(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
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

const BEGIN_READ: &str = "begin";
const END_READ: &str = "commit";

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
