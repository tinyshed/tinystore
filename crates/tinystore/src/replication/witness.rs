//! A witness: a node that holds no file of the network and keeps each file's
//! lease, which the nodes judged by it ask for. A draft, as its names are.
//!
//! ```text
//! the home    ──"I write kv.db at epoch 2"──▶ the witness: renewed for 5 s
//! a copy      ──"I copy kv.db, at entry 90"─▶ the witness: standing
//!             ...the home falls silent, its lease runs out, a second passes...
//! a copy      ──"I copy kv.db, at entry 93"─▶ the witness: the freshest copy
//!                                             standing takes it, at epoch 3
//! ```
//!
//! The home stops writing before its lease can run out here, by its own
//! clock and a margin; the witness counts from when a renewal reached it, so
//! the two never overlap. A witness that starts again grants nothing for a
//! lease's span, during which the home renews what it held, and it keeps each
//! file's epoch so that one never goes back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::DIR;
use super::node::lock;
use crate::{Error, Result};

/// How long a lease lasts from the renewal that reached the witness.
pub(crate) const TTL: Duration = Duration::from_secs(5);
/// How long after a lease ran out the witness gathers the copies standing,
/// so that the freshest takes it rather than the first to ask.
const GATHER: Duration = Duration::from_secs(1);
/// A copy silent this long stands no more.
const STANDING: Duration = Duration::from_secs(3);
/// The file in a witness's `log/` that keeps each file's epoch.
const EPOCHS: &str = "witness";

/// What a judged node asks of its witness for a file.
pub(crate) struct Ask<'a> {
    pub(crate) file: &'a str,
    pub(crate) node: &'a str,
    pub(crate) address: &'a str,
    /// The epoch of the node's log of the file.
    pub(crate) epoch: u64,
    /// The last entry it holds of it.
    pub(crate) position: u64,
    /// It writes the file and renews its lease, rather than stand for it.
    pub(crate) holding: bool,
}

/// The holder of a file's lease, as the witness answers it: none while no
/// node holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) node: String,
    pub(crate) address: String,
    pub(crate) epoch: u64,
}

struct Lease {
    holder: Holder,
    expires: Instant,
    standing: HashMap<String, Standing>,
}

struct Standing {
    position: u64,
    seen: Instant,
}

pub(crate) struct Witness {
    leases: Mutex<HashMap<String, Lease>>,
    epochs: Mutex<HashMap<String, u64>>,
    path: PathBuf,
    started: Instant,
    /// The nodes a test cut off: their asks go unanswered.
    cut: Mutex<HashSet<String>>,
}

impl Witness {
    /// Opens the witness of the node in `dir`, the epochs it kept read again.
    pub(crate) fn open(dir: &Path) -> Result<Witness> {
        let path = dir.join(DIR).join(EPOCHS);
        let epochs = match std::fs::read_to_string(&path) {
            Ok(kept) => kept
                .lines()
                .filter_map(|line| line.split_once(' '))
                .filter_map(|(epoch, file)| Some((file.to_owned(), epoch.parse().ok()?)))
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(Error::io(path.display().to_string(), error)),
        };
        Ok(Witness {
            leases: Mutex::default(),
            epochs: Mutex::new(epochs),
            path,
            started: Instant::now(),
            cut: Mutex::default(),
        })
    }

    /// Answers a node's ask with the file's holder now: renewed when the node
    /// holds it, granted to the freshest copy standing once it ran out; none
    /// for a node a test cut off.
    pub(crate) fn ask(&self, ask: &Ask<'_>) -> Result<Option<Holder>> {
        if lock(&self.cut).contains(ask.node) {
            return Ok(None);
        }
        let now = Instant::now();
        let kept = lock(&self.epochs).get(ask.file).copied().unwrap_or(0);
        let mut leases = lock(&self.leases);
        let lease = leases.entry(ask.file.to_owned()).or_insert_with(|| Lease {
            holder: Holder { epoch: kept, ..Holder::default() },
            expires: self.started + TTL,
            standing: HashMap::new(),
        });
        let granted = if ask.holding { renew(lease, ask, now) } else { stand(lease, ask, now) };
        let holder = lease.holder.clone();
        drop(leases);
        if granted {
            self.keep_epoch(ask.file, holder.epoch)?;
        }
        Ok(Some(holder))
    }

    /// Keeps durably the epoch a file's lease reached.
    fn keep_epoch(&self, file: &str, epoch: u64) -> Result<()> {
        let mut epochs = lock(&self.epochs);
        if epochs.get(file).is_some_and(|&kept| kept >= epoch) {
            return Ok(());
        }
        epochs.insert(file.to_owned(), epoch);
        let kept: String = epochs.iter().map(|(file, epoch)| format!("{epoch} {file}\n")).collect();
        let io = |error| Error::io(self.path.display().to_string(), error);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::write(&self.path, kept).map_err(io)
    }
}

#[cfg(test)]
#[allow(dead_code, reason = "the link tests call them, and they build only with every engine")]
impl Witness {
    pub(crate) fn holder(&self, file: &str) -> Option<Holder> {
        lock(&self.leases).get(file).map(|lease| lease.holder.clone())
    }

    pub(crate) fn cut(&self, node: &str) {
        lock(&self.cut).insert(node.to_owned());
    }

    pub(crate) fn heal(&self, node: &str) {
        lock(&self.cut).remove(node);
    }
}

/// A home's renewal: its own lease lasts on; a lease no node holds, it takes
/// at its epoch when that is no older than the witness kept. Whether the
/// holder or the epoch changed.
fn renew(lease: &mut Lease, ask: &Ask<'_>, now: Instant) -> bool {
    if lease.holder.node == ask.node && lease.holder.epoch == ask.epoch {
        lease.expires = now + TTL;
        return false;
    }
    if lease.holder.node.is_empty() && ask.epoch >= lease.holder.epoch {
        lease.holder = Holder { node: ask.node.to_owned(), address: ask.address.to_owned(), epoch: ask.epoch };
        lease.expires = now + TTL;
        return true;
    }
    false
}

/// A copy standing for the lease: it takes it at the next epoch once the
/// lease ran out and the gathering passed, when it is the freshest copy
/// standing. Whether it took it.
fn stand(lease: &mut Lease, ask: &Ask<'_>, now: Instant) -> bool {
    let standing = Standing { position: ask.position, seen: now };
    lease.standing.insert(ask.node.to_owned(), standing);
    lease.standing.retain(|_, standing| now.duration_since(standing.seen) < STANDING);
    // a lease no node held is its home's to take first, as a file born there
    if now < lease.expires + GATHER || lease.holder.node == ask.node || lease.holder.epoch == 0 {
        return false;
    }
    let freshest = lease.standing.iter().max_by_key(|(_, standing)| standing.position).map(|(node, _)| node.clone());
    if freshest.as_deref() != Some(ask.node) {
        return false;
    }
    let epoch = lease.holder.epoch + 1;
    lease.holder = Holder { node: ask.node.to_owned(), address: ask.address.to_owned(), epoch };
    lease.expires = now + TTL;
    lease.standing.clear();
    true
}
