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
//! the two never overlap. Each lease's holder and epoch are kept durably
//! before the witness answers a grant, so that a witness that starts again
//! knows who held what: it grants nothing for a lease's span, during which a
//! holder renews what it held, and no epoch goes back.
//!
//! A home at a later epoch than the holder's took the file by a move or a
//! promotion: it takes the lease once the holder's ran out, before any copy
//! standing for it. A copy takes a lease only at an epoch past every one a
//! home asked for, so that no two nodes ever hold one epoch of a file.
//!
//! A synchronous home says which copies its commits wait for, and the witness
//! keeps them with the holder: only those copies hold every commit the home
//! answered, so the lease goes to the freshest of them, once all of them
//! stand, and to no other copy.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::DIR;
use crate::sync::lock;
use crate::{Error, Result};

/// How long a lease lasts from the renewal that reached the witness.
pub(crate) const TTL: Duration = Duration::from_secs(5);
/// How long after a lease ran out the witness gathers the copies standing,
/// so that the freshest takes it rather than the first to ask.
const GATHER: Duration = Duration::from_secs(1);
/// A copy silent this long stands no more.
const STANDING: Duration = Duration::from_secs(3);
/// The file in a witness's `log/` that keeps each lease's holder, a line a
/// file: `epoch node address copies file`, its copies those a synchronous
/// home waits for, by their ids or addresses and split by commas, or `-`.
const HOLDERS: &str = "witness";

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
    /// The copies a synchronous home's commits wait for, by their ids or
    /// addresses; none for another home or a copy.
    pub(crate) synchronous: &'a [String],
}

/// The holder of a file's lease, as the witness answers it: none while no
/// node holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Holder {
    pub(crate) node: String,
    pub(crate) address: String,
    pub(crate) epoch: u64,
}

/// What the witness keeps of a file's lease on disk.
#[derive(Clone, Default, PartialEq, Eq)]
struct Kept {
    holder: Holder,
    synchronous: Vec<String>,
}

#[derive(Clone)]
struct Lease {
    kept: Kept,
    expires: Instant,
    standing: HashMap<String, Standing>,
    /// A home at a later epoch waiting for the holder's lease to run out.
    claimant: Option<Claimant>,
}

#[derive(Clone)]
struct Claimant {
    epoch: u64,
    asked: Instant,
}

#[derive(Clone)]
struct Standing {
    address: String,
    /// The epoch of the last entry the copy applied.
    epoch: u64,
    position: u64,
    seen: Instant,
}

pub(crate) struct Witness {
    leases: Mutex<HashMap<String, Lease>>,
    /// Each lease as kept on disk.
    kept: Mutex<HashMap<String, Kept>>,
    path: PathBuf,
    started: Instant,
    /// The nodes a test cut off: their asks go unanswered.
    cut: Mutex<HashSet<String>>,
}

impl Witness {
    /// Opens the witness of the node in `dir`, the holders it kept read again.
    pub(crate) fn open(dir: &Path) -> Result<Witness> {
        let path = dir.join(DIR).join(HOLDERS);
        let kept = match std::fs::read_to_string(&path) {
            Ok(kept) => kept.lines().filter_map(read_holder).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => return Err(Error::io(path.display().to_string(), error)),
        };
        Ok(Witness {
            leases: Mutex::default(),
            kept: Mutex::new(kept),
            path,
            started: Instant::now(),
            cut: Mutex::default(),
        })
    }

    /// Answers a node's ask with the file's holder now: renewed when the node
    /// holds it, granted to the freshest copy standing once it ran out; none
    /// for a node a test cut off. A grant is kept on disk before it answers,
    /// and one that cannot be kept is not made.
    pub(crate) fn ask(&self, ask: &Ask<'_>) -> Result<Option<Holder>> {
        if lock(&self.cut).contains(ask.node) {
            return Ok(None);
        }
        let now = Instant::now();
        let mut leases = lock(&self.leases);
        let mut lease = match leases.get(ask.file) {
            Some(lease) => lease.clone(),
            None => Lease {
                kept: lock(&self.kept).get(ask.file).cloned().unwrap_or_default(),
                expires: self.started + TTL,
                standing: HashMap::new(),
                claimant: None,
            },
        };
        let before = lease.kept.holder.clone();
        let changed = if ask.holding { renew(&mut lease, ask, now) } else { stand(&mut lease, ask, now) };
        if changed {
            self.keep(ask.file, &lease.kept)?;
        }
        let Holder { node, epoch, .. } = &lease.kept.holder;
        if *node != before.node || *epoch != before.epoch {
            tracing::info!(
                target: "tinystore",
                file = ask.file,
                holder = %node,
                epoch,
                "a witness granted a file's lease",
            );
        }
        let holder = lease.kept.holder.clone();
        leases.insert(ask.file.to_owned(), lease);
        Ok(Some(holder))
    }

    /// Keeps durably the holder a file's lease was granted to: written beside
    /// the file, synced and put in its place, so that a crash leaves the old
    /// holders or the new, whole.
    fn keep(&self, file: &str, lease: &Kept) -> Result<()> {
        let mut kept = lock(&self.kept).clone();
        kept.insert(file.to_owned(), lease.clone());
        let lines: String = kept.iter().map(|(file, kept)| written(file, kept)).collect();
        let io = |error| Error::io(self.path.display().to_string(), error);
        let parent = self.path.parent().expect("the witness's file lies in log/");
        std::fs::create_dir_all(parent).map_err(io)?;
        let beside = self.path.with_extension("new");
        let mut written = std::fs::File::create(&beside).map_err(io)?;
        written.write_all(lines.as_bytes()).and_then(|()| written.sync_all()).map_err(io)?;
        drop(written);
        std::fs::rename(&beside, &self.path).map_err(io)?;
        crate::durable::sync_directory(parent).map_err(io)?;
        *lock(&self.kept) = kept;
        Ok(())
    }
}

/// A lease's line in the witness's file.
fn written(file: &str, kept: &Kept) -> String {
    let Holder { node, address, epoch } = &kept.holder;
    let copies = if kept.synchronous.is_empty() { "-".to_owned() } else { kept.synchronous.join(",") };
    format!("{epoch} {node} {address} {copies} {file}\n")
}

/// A lease the witness's file keeps, and its file.
fn read_holder(line: &str) -> Option<(String, Kept)> {
    let (epoch, rest) = line.split_once(' ')?;
    let (node, rest) = rest.split_once(' ')?;
    let (address, rest) = rest.split_once(' ')?;
    let (copies, file) = rest.split_once(' ')?;
    let holder = Holder { node: node.to_owned(), address: address.to_owned(), epoch: epoch.parse().ok()? };
    let synchronous = if copies == "-" { Vec::new() } else { copies.split(',').map(str::to_owned).collect() };
    Some((file.to_owned(), Kept { holder, synchronous }))
}

#[cfg(test)]
#[allow(dead_code, reason = "the link tests call them, and they build only with every engine")]
impl Witness {
    pub(crate) fn holder(&self, file: &str) -> Option<Holder> {
        lock(&self.leases).get(file).map(|lease| lease.kept.holder.clone())
    }

    pub(crate) fn cut(&self, node: &str) {
        lock(&self.cut).insert(node.to_owned());
    }

    pub(crate) fn heal(&self, node: &str) {
        lock(&self.cut).remove(node);
    }
}

/// A home's renewal: its own lease lasts on; a lease no node holds, it takes
/// at its epoch when that is no older than the witness kept; and one held at
/// an older epoch, it takes once that ran out. Whether what the witness keeps
/// changed: the holder, the epoch or the copies waited for.
fn renew(lease: &mut Lease, ask: &Ask<'_>, now: Instant) -> bool {
    let holder = &lease.kept.holder;
    let renewed = holder.node == ask.node && holder.epoch == ask.epoch;
    let first = holder.node.is_empty() && ask.epoch >= holder.epoch;
    let moved_in = !holder.node.is_empty() && ask.epoch > holder.epoch;
    if moved_in && now < lease.expires {
        let epoch = lease.claimant.as_ref().map_or(ask.epoch, |claimant| claimant.epoch.max(ask.epoch));
        lease.claimant = Some(Claimant { epoch, asked: now });
        return false;
    }
    if !renewed && !first && !moved_in {
        return false;
    }
    let kept = Kept {
        holder: Holder { node: ask.node.to_owned(), address: ask.address.to_owned(), epoch: ask.epoch },
        synchronous: ask.synchronous.to_vec(),
    };
    lease.expires = now + TTL;
    if !renewed {
        (lease.claimant, lease.standing) = (None, HashMap::new());
    }
    let changed = kept != lease.kept;
    lease.kept = kept;
    changed
}

/// A copy standing for the lease: it takes it at the next epoch once the
/// lease ran out and the gathering passed, when it is the freshest copy
/// standing. Whether it took it.
fn stand(lease: &mut Lease, ask: &Ask<'_>, now: Instant) -> bool {
    let standing = Standing { address: ask.address.to_owned(), epoch: ask.epoch, position: ask.position, seen: now };
    lease.standing.insert(ask.node.to_owned(), standing);
    lease.standing.retain(|_, standing| now.duration_since(standing.seen) < STANDING);
    let holder = &lease.kept.holder;
    // a lease no node held is its home's to take first, as a file born there
    if now < lease.expires + GATHER || holder.node == ask.node || holder.epoch == 0 {
        return false;
    }
    // a home at a later epoch, moved in, takes it before any copy
    if lease.claimant.as_ref().is_some_and(|claimant| now.duration_since(claimant.asked) < TTL) {
        return false;
    }
    if freshest(lease).as_deref() != Some(ask.node) {
        return false;
    }
    let claimed = lease.claimant.as_ref().map_or(0, |claimant| claimant.epoch);
    let epoch = holder.epoch.max(claimed) + 1;
    let holder = Holder { node: ask.node.to_owned(), address: ask.address.to_owned(), epoch };
    lease.claimant = None;
    lease.kept = Kept { holder, synchronous: Vec::new() };
    lease.expires = now + TTL;
    lease.standing.clear();
    true
}

/// The copy that may take a lease that ran out: the freshest standing, or,
/// for a synchronous home's, the freshest of the copies it waited for, once
/// every one of them stands. The freshest is of the latest epoch, then the
/// furthest on: a copy further on in an older epoch holds an old home's
/// commits that a later home never had, and lacks that home's.
fn freshest(lease: &Lease) -> Option<String> {
    let waited = &lease.kept.synchronous;
    let names = |node: &str, standing: &Standing, name: &String| *name == node || *name == standing.address;
    let candidates: Vec<(&String, &Standing)> = lease
        .standing
        .iter()
        .filter(|(node, standing)| waited.is_empty() || waited.iter().any(|name| names(node, standing, name)))
        .collect();
    let every_one_stands =
        waited.iter().all(|name| candidates.iter().any(|(node, standing)| names(node, standing, name)));
    if !every_one_stands {
        return None;
    }
    let latest = |(_, standing): &(&String, &Standing)| (standing.epoch, standing.position);
    candidates.into_iter().max_by_key(latest).map(|(node, _)| node.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked<'a>(node: &'a str, epoch: u64, position: u64) -> Ask<'a> {
        Ask { file: "kv.db", node, address: node, epoch, position, holding: false, synchronous: &[] }
    }

    // A copy back with the old home's chain stands further on than one that
    // took the current home's commits: those commits are its home's newest.
    #[test]
    fn the_copy_of_the_latest_epoch_takes_a_lease_before_one_further_on_in_an_older() {
        let ran_out = Instant::now();
        let mut lease = Lease {
            kept: Kept { holder: Holder { node: "home".into(), address: "home".into(), epoch: 2 }, ..Kept::default() },
            expires: ran_out,
            standing: HashMap::new(),
            claimant: None,
        };
        let (old_chain, current) = (asked("came back", 1, 1500), asked("followed", 2, 1420));
        let before = ran_out.checked_sub(Duration::from_secs(1)).unwrap();
        assert!(!stand(&mut lease, &old_chain, before) && !stand(&mut lease, &current, before), "the home holds it");
        let gathered = ran_out + GATHER + Duration::from_millis(100);
        assert!(!stand(&mut lease, &old_chain, gathered), "the copy on an older epoch takes nothing");
        assert!(stand(&mut lease, &current, gathered));
        assert_eq!((lease.kept.holder.node.as_str(), lease.kept.holder.epoch), ("followed", 3));
    }
}
