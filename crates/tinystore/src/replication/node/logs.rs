//! The logs of the files a node writes: opened for their writers, fenced and
//! leased, found on disk, and read by a snapshot or a follower.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Instant;

use super::State;
use crate::replication::DIR;
use crate::replication::catalog::FILE as CATALOG;
use crate::replication::copy::{self, Copy};
use crate::replication::entry::Point;
use crate::replication::log::{Log, segments_in};
use crate::replication::promote;
use crate::replication::snapshot::Source;
use crate::sqlite::Observe;
use crate::sync::lock;
use crate::{Error, ErrorKind, Result};

impl State {
    /// The epoch of this node's log of `name`: its last entry's, or the
    /// point a promotion left; 1 for a file it holds no log of.
    pub(super) fn epoch_of(&self, name: &str) -> Result<u64> {
        let dir = self.log_dir(name);
        let last = Log::last_in(&dir)?.map(|point| point.epoch);
        let promoted = promote::promoted_at(&dir)?.map(|point| point.epoch);
        Ok(last.max(promoted).unwrap_or(1))
    }

    /// Takes writes of `name` again, after a move that did not finish.
    /// Writes `name` again at `epoch`, the catalog naming this node its home
    /// there once a move away from it was called off: its log goes on at the
    /// epoch, and its fence goes.
    pub(crate) fn go_on_at(&self, name: &str, epoch: u64) -> Result<()> {
        match lock(&self.logs).get(name).and_then(Weak::upgrade) {
            Some(log) => log.go_on_at(epoch)?,
            None => Log::go_on_at_in(&self.log_dir(name), epoch)?,
        }
        self.unfence(name)
    }

    pub(crate) fn unfence(&self, name: &str) -> Result<()> {
        match lock(&self.logs).get(name).and_then(Weak::upgrade) {
            Some(log) => log.unfence(),
            None => Log::unfence_in(&self.log_dir(name)),
        }
    }

    /// Runs `read` with where this node's own file `name` stands, held still:
    /// by its writer while one is open; with none, nothing writes it.
    pub(crate) fn holding_home<T>(&self, name: &str, read: impl FnOnce(Point) -> Result<T>) -> Result<T> {
        if let Some(log) = lock(&self.logs).get(name).and_then(Weak::upgrade) {
            return log.holding(read);
        }
        let point = self.point_of(name).ok_or_else(|| {
            Error::new(ErrorKind::Unavailable, format!("{name}: this node holds no log of it it can read now"))
        })?;
        read(point)
    }

    /// Refuses every write of `name` from now on, and across restarts: this
    /// node is its old home, and another writes it at `epoch`.
    pub(crate) fn fence(&self, name: &str, epoch: u64) -> Result<()> {
        tracing::error!(
            target: "tinystore",
            file = name,
            epoch,
            "another node writes a file this node wrote, at a later epoch: this node takes no more writes of it"
        );
        // the logs held while the fence is kept: see `kept_log`
        let logs = lock(&self.logs);
        match logs.get(name).and_then(Weak::upgrade) {
            Some(log) => log.fence(epoch),
            None => Log::fence_in(&self.log_dir(name), epoch),
        }
    }

    /// The log of the file `name` this node writes, opened for its writer.
    pub(crate) fn open_log(&self, name: &str) -> Result<Arc<dyn Observe>> {
        let log = self.kept_log(name, self.log_of(name)?)?;
        // A stop wakes the logs it finds; one opened as the node stops, by an
        // engine's open the store's close raced, is kept before this looks,
        // so that either the stop finds it or this sees the stop.
        if self.stopping() {
            log.stop_waiting();
        }
        Ok(log)
    }

    /// The log of `name` opened, as this node writes it.
    pub(in crate::replication) fn log_of(&self, name: &str) -> Result<Log> {
        let mut log = Log::open(self.log_dir(name), self.bounds, Arc::clone(&self.notify))?
            .followed(name, Arc::clone(&self.acks));
        // the catalog's home moves only by a person's hand, never by a lease
        if self.options.failover.is_some() && name != CATALOG {
            log = log.judged(lock(&self.granted).remove(name).unwrap_or_else(Instant::now));
        }
        if self.options.synchronous || !self.options.synchronous_with.is_empty() {
            log = log.synchronous();
        }
        Ok(log)
    }

    /// Keeps `log` among the logs open, with the fence its directory keeps
    /// now: a fence of a log not kept yet goes to the directory alone, which
    /// `fence` writes holding the logs as this reads it, so that the log
    /// either is found there by the fence or reads it here.
    pub(in crate::replication) fn kept_log(&self, name: &str, log: Log) -> Result<Arc<Log>> {
        let log = Arc::new(log);
        let mut logs = lock(&self.logs);
        logs.insert(name.to_owned(), Arc::downgrade(&log));
        log.fenced_as_kept()?;
        drop(logs);
        Ok(log)
    }

    pub(crate) fn log_dir(&self, name: &str) -> PathBuf {
        self.dir.join(DIR).join(name)
    }

    /// Every file with a directory in `log/`, and each whose log is open with
    /// no entry yet, for a judged node's turn: a file born here asks its
    /// witness for its first lease before its first commit.
    pub(crate) fn files_logged_now(&self) -> Result<Vec<String>> {
        let mut files = self.files_logged()?;
        for name in lock(&self.logs).keys() {
            if !files.contains(name) {
                files.push(name.clone());
            }
        }
        Ok(files)
    }

    /// Keeps the lease a witness granted of the copy `file` until its log
    /// opens, as the copy is taken over.
    pub(crate) fn granted(&self, file: &str, until: Instant) {
        lock(&self.granted).insert(file.to_owned(), until);
    }

    /// The epoch this node's open log of `file` writes at.
    pub(crate) fn epoch_written(&self, file: &str) -> Option<u64> {
        lock(&self.logs).get(file).and_then(Weak::upgrade).map(|log| log.epoch())
    }

    /// The epoch this node writes `file` at, taking no writer: a write that
    /// holds one may wait for the lease this epoch renews. A log no engine
    /// holds open is read once and remembered, its epoch moving only while
    /// open.
    pub(crate) fn epoch_held(&self, file: &str) -> Option<u64> {
        if let Some(epoch) = self.epoch_written(file) {
            lock(&self.epochs).insert(file.to_owned(), epoch);
            return Some(epoch);
        }
        if let Some(&epoch) = lock(&self.epochs).get(file) {
            return Some(epoch);
        }
        // the epoch a move or a promotion left, which no entry may name yet, read
        // from the log's files rather than through the writer
        if !self.log_dir(file).exists() {
            return None;
        }
        let epoch = self.epoch_of(file).ok()?;
        lock(&self.epochs).insert(file.to_owned(), epoch);
        Some(epoch)
    }

    /// Keeps the lease of `file` this node writes on to `until`.
    pub(crate) fn lease_until(&self, file: &str, until: Instant) {
        if let Some(log) = lock(&self.logs).get(file).and_then(Weak::upgrade) {
            log.lease_until(until);
        }
    }

    /// Until when this node holds its lease of `file`, by its own clock.
    #[cfg(test)]
    #[allow(dead_code, reason = "the link tests call it, and they build only with every engine")]
    pub(crate) fn lease_of(&self, file: &str) -> Option<Instant> {
        lock(&self.logs).get(file).and_then(Weak::upgrade).and_then(|log| log.leased_until())
    }

    /// Every file with a directory in `log/`, by its name in the store's.
    pub(super) fn files_logged(&self) -> Result<Vec<String>> {
        files_logged_in(&self.dir.join(DIR))
    }

    /// What a snapshot of the file `name` reads, if this node serves it.
    pub(crate) fn source(&self, name: &str) -> Option<Box<dyn Source>> {
        if let Some(copy) = self.copy(name) {
            return Some(Box::new(CopySource(copy)));
        }
        let dir = self.log_dir(name);
        if copy::is_copy(&dir) || !dir.exists() {
            return None;
        }
        let log = lock(&self.logs).get(name).and_then(Weak::upgrade);
        Some(Box::new(HomeSource { path: self.dir.join(name), dir, log }))
    }

    /// Where the log of `name` stands, for a write's caller to wait for.
    pub(crate) fn point_of(&self, name: &str) -> Option<Point> {
        self.source(name).and_then(|source| source.stable().ok())
    }
}

/// Every file with a directory in `logs`, a store's `log/`, by its name in
/// the store's directory.
pub(crate) fn files_logged_in(logs: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    walk(logs, "", &mut names)?;
    names.sort();
    Ok(names)
}

/// Collects the names of the files whose log directories lie under `dir`: a
/// directory holding a segment, a copy's mark or a promotion's point is one.
fn walk(dir: &Path, prefix: &str, names: &mut Vec<String>) -> Result<()> {
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Error::io(dir.display().to_string(), error)),
    };
    for found in listed {
        let found = found.map_err(|error| Error::io(dir.display().to_string(), error))?;
        let Ok(name) = found.file_name().into_string() else { continue };
        if !found.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = found.path();
        let joined = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        // a promoted copy whose log kept no entry since its snapshot holds one too
        let holds_a_log =
            copy::is_copy(&path) || !segments_in(&path)?.is_empty() || path.join(promote::PROMOTED).exists();
        if holds_a_log {
            names.push(joined);
        } else {
            walk(&path, &joined, names)?;
        }
    }
    Ok(())
}

/// A file this node writes, its log open with its writer or closed.
struct HomeSource {
    path: PathBuf,
    dir: PathBuf,
    log: Option<Arc<Log>>,
}

impl Source for HomeSource {
    fn path(&self) -> &Path {
        &self.path
    }

    fn stable(&self) -> Result<Point> {
        if let Some(log) = &self.log {
            return Ok(log.stable());
        }
        // With no writer open, a WAL that holds frames may hold commits a
        // process that died made and its log never took: they reach the log
        // when the file's engine opens it.
        if fs::metadata(crate::sqlite::wal_of(&self.path)).is_ok_and(|wal| wal.len() > 0) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "its WAL holds commits its log may lack until its engine opens it",
            ));
        }
        Log::last_in(&self.dir)?.ok_or_else(|| Error::new(ErrorKind::Unavailable, "its log holds no entry yet"))
    }
}

/// A copy this node holds whole.
struct CopySource(Arc<Copy>);

impl Source for CopySource {
    fn path(&self) -> &Path {
        self.0.path()
    }

    fn stable(&self) -> Result<Point> {
        Ok(self.0.point())
    }
}

impl crate::probe::Sample for State {
    /// How far each node copying a file of this home is behind it.
    fn sample(&self, probe: &crate::probe::Probe) {
        let logs: Vec<(String, Arc<Log>)> =
            lock(&self.logs).iter().filter_map(|(name, log)| Some((name.clone(), log.upgrade()?))).collect();
        for (name, log) in logs {
            let file = crate::probe::label(&name);
            for (node, behind) in self.acks.behind(&name, log.last()) {
                let labels = [("file", file.as_str()), ("node", node.as_str())];
                probe.set("tinystore_replication_behind_entries", &labels, behind as f64);
            }
        }
    }
}
