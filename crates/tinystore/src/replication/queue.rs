//! Operations that commute, queued on the node that made them and sent to
//! their home after: a counter's add answers at once on a copy, and its home
//! counts it once however often it arrives.
//!
//! ```text
//! copy:  add +1 ──► log/queue.db (op 41) ──► answered: the copy's value and what the queue holds
//!                         │ a thread sends the ops in order, many at once
//!                         ▼
//! home:  applies op 41 of this node only past the last of its it applied, in one commit
//!                         │ the commit's pages come back
//!                         ▼
//! copy:  holds op 41 in its own file, and lets it go from the queue
//! ```
//!
//! A node is known to its home by an id it makes once and keeps in `log/node`;
//! its ops are numbered by the queue's file, which never gives a number twice,
//! so that the home keeps one number a node, the last it applied.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use rusqlite::params;
use sha2::{Digest, Sha256};

use super::forward;
use super::node::State;
use crate::sqlite::{Config, File, sql_error};
use crate::{Error, Result, Store};

// subject: what an op changes, as a counter's key, whose ops a read on the
// copy adds; its engine says what it is
const SCHEMA: &str = "create table if not exists queued (
    op      integer primary key autoincrement,
    file    text    not null,
    subject blob    not null,
    request blob    not null
) strict;
create index if not exists queued_subject on queued (file, subject, op)";
const ENQUEUE: &str = "insert into queued (file, subject, request) values (?1, ?2, ?3) returning op";
const NEXT: &str = "select op, file, request from queued order by op limit ?1";
const OF_SUBJECT: &str = "select op, request from queued where file = ?1 and subject = ?2 order by op";
const SENT: &str = "delete from queued where file = ?1 and op <= ?2";
/// The queue's file in the node's `log/`.
pub(crate) const FILE: &str = "queue.db";
/// The ops one send carries at most.
const BATCH: i64 = 256;

/// An op the queue holds.
pub(crate) struct Queued {
    pub(crate) op: u64,
    pub(crate) request: Vec<u8>,
}

/// A node's queue: its file, and what wakes its sender.
pub(crate) struct Queue {
    file: File,
    queued: Mutex<bool>,
    wake: Condvar,
    /// The node's id, which its home counts its ops by.
    pub(crate) node: String,
}

impl Queue {
    /// Opens the queue of the node in `dir`, its id made the first time.
    pub(crate) fn open(dir: &Path) -> Result<Queue> {
        let logs = dir.join(super::DIR);
        std::fs::create_dir_all(&logs).map_err(|error| Error::io(logs.display().to_string(), error))?;
        let file = File::open(logs.join(FILE), Config::default())?;
        file.transaction(|tx| tx.execute_batch(SCHEMA).map_err(|error| sql_error("a node's queue", error)))?;
        Ok(Queue { file, queued: Mutex::new(false), wake: Condvar::new(), node: node_id(&logs)? })
    }

    /// Queues `request`, a write of `file` its home applies that changes
    /// `subject`, and gives its op.
    pub(crate) fn push(&self, file: &str, subject: Vec<u8>, request: Vec<u8>) -> Result<u64> {
        let file = file.to_owned();
        let op: i64 = self.file.write(request.len(), move |tx| {
            tx.query_row(ENQUEUE, params![file, subject, request], |row| row.get(0))
                .map_err(|error| sql_error("an op queued", error))
        })?;
        *self.queued.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
        Ok(op as u64)
    }

    /// The ops queued of `file` that change `subject`, oldest first: what a
    /// copy's reads add of those its copy does not hold yet.
    pub(crate) fn of_subject(&self, file: &str, subject: &[u8]) -> Result<Vec<Queued>> {
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(OF_SUBJECT).map_err(|error| sql_error("the queue", error))?;
            let rows = select
                .query_map(params![file, subject], |row| {
                    Ok(Queued { op: row.get::<_, i64>(0)? as u64, request: row.get(1)? })
                })
                .map_err(|error| sql_error("the queue", error))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|error| sql_error("the queue", error))
        })
    }

    /// Whether the queue holds no op.
    #[cfg(test)]
    #[allow(dead_code, reason = "the link tests read it, and they build only with every engine")]
    pub(crate) fn is_empty(&self) -> Result<bool> {
        const ANY: &str = "select exists (select 1 from queued)";
        self.file.read(|connection| {
            let any: bool =
                connection.query_row(ANY, [], |row| row.get(0)).map_err(|error| sql_error("the queue", error))?;
            Ok(!any)
        })
    }

    /// The oldest ops queued, each with its file and request.
    fn next(&self) -> Result<Vec<(i64, String, Vec<u8>)>> {
        self.file.read(|connection| {
            let mut select = connection.prepare_cached(NEXT).map_err(|error| sql_error("the queue", error))?;
            let rows = select
                .query_map([BATCH], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|error| sql_error("the queue", error))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|error| sql_error("the queue", error))
        })
    }

    /// Lets go of the ops of `file` through `through`, which its copy holds.
    fn sent_of(&self, file: &str, through: i64) -> Result<()> {
        let file = file.to_owned();
        self.file.write(0, move |tx| {
            tx.execute(SENT, params![file, through]).map(drop).map_err(|error| sql_error("the queue", error))
        })
    }

    /// Waits for an op queued, `at_most` long.
    fn wait(&self, at_most: Duration) {
        let queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
        let mut queued =
            self.wake.wait_timeout_while(queued, at_most, |queued| !*queued).unwrap_or_else(PoisonError::into_inner).0;
        *queued = false;
    }

    pub(crate) fn close(&self) {
        let _ = self.file.close();
    }
}

/// Sends the queue's ops to the home, many at once and in order, until the
/// node stops; an op answered and held by this node's copy leaves the queue.
pub(crate) fn send(state: &Arc<State>, queue: &Queue) {
    while !state.stopping() {
        let sent = state.store().map_or(Ok(false), |store| send_some(state, &store, queue));
        match sent {
            Ok(true) => continue,
            Ok(false) => queue.wait(Duration::from_millis(500)),
            Err(error) => {
                tracing::debug!(target: "tinystore", %error, "a node's queued ops wait for their home");
                queue.wait(Duration::from_secs(1));
            }
        }
    }
}

/// Sends the oldest ops queued of one file; whether there were any. Those of
/// a file this node writes now, a copy promoted since, run here.
fn send_some(state: &State, store: &Store, queue: &Queue) -> Result<bool> {
    let mut ops = queue.next()?;
    let Some((_, file, _)) = ops.first() else {
        return Ok(false);
    };
    let file = file.clone();
    ops.retain(|(_, of, _)| *of == file);
    let last = ops.last().map_or(0, |&(op, _, _)| op);
    let requests: Vec<_> =
        ops.into_iter().map(|(op, _, request)| forward::from_node(request, &queue.node, op as u64)).collect();
    if state.takes(&file) {
        forward::pass_on_all(store, &file, &requests)?;
    } else {
        forward::run_here(store, &requests)?;
    }
    queue.sent_of(&file, last)?;
    Ok(true)
}

/// The node's id, made once from its directory, its process and the time,
/// and kept in `log/node`.
fn node_id(logs: &Path) -> Result<String> {
    let path = logs.join("node");
    if let Ok(kept) = std::fs::read_to_string(&path) {
        let kept = kept.trim();
        if kept.len() == 32 && kept.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(kept.to_owned());
        }
    }
    let mut hash = Sha256::new();
    hash.update(logs.display().to_string());
    hash.update(std::process::id().to_le_bytes());
    hash.update(format!("{:?}", std::time::SystemTime::now()));
    hash.update(std::hash::BuildHasher::hash_one(&std::collections::hash_map::RandomState::new(), 0_u64).to_le_bytes());
    let id: String = hash.finalize()[..16].iter().map(|byte| format!("{byte:02x}")).collect();
    std::fs::write(&path, &id).map_err(|error| Error::io(path.display().to_string(), error))?;
    crate::durable::sync_directory(logs).map_err(|error| Error::io(logs.display().to_string(), error))?;
    Ok(id)
}
