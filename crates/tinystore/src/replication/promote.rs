//! A copy made its file's home, by a person, once the old home is known to be
//! stopped: no node can tell a dead home from a cut cable, and two homes of a
//! file would split it for good ([plan/scaling.md], changing a file's home).
//!
//! The node is stopped while it runs. Each copy's mark goes, and its position
//! becomes the point its log goes on from at the next epoch: the next commit
//! is the entry after the last the copy applied, so the nodes that copied the
//! old home go on from where they stand, and an entry of the old epoch that
//! the copy never had, which another node may hold, conflicts by its check.
//!
//! [plan/scaling.md]: https://github.com/tinyshed/tinystore/blob/rust/plan/scaling.md

use std::fs;
use std::path::Path;

use super::copy::{self, Position, io_error};
use super::entry::Point;
use super::{DIR, node};
use crate::{Error, ErrorKind, Result};

/// The file in a log's directory that holds the point a promoted copy's log
/// goes on from, and the epoch its entries take.
pub(crate) const PROMOTED: &str = "promoted";

/// A file a promotion made this node's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promoted {
    /// The file, as `kv.db` or `sql/app.db`.
    pub file: String,
    /// The entry of its old home its log goes on after.
    pub sequence: u64,
    /// The epoch its entries take from now on.
    pub epoch: u64,
}

/// Makes the copies of the stopped store in `dir` its own files: those named,
/// or every whole copy it holds when none is. A copy that is not whole, or a
/// name that is no copy, is refused before any changes.
pub fn promote(dir: &Path, files: &[String]) -> Result<Vec<Promoted>> {
    let what = || format!("promote {}", dir.display());
    let _lock = crate::store::take_lock(dir).map_err(|error| error.within(what()))?;
    let logs = dir.join(DIR);
    let names = if files.is_empty() { node::files_logged_in(&logs)? } else { files.to_vec() };
    let mut promoting = Vec::new();
    for name in names {
        let log = logs.join(&name);
        if !copy::is_copy(&log) {
            if files.is_empty() {
                continue;
            }
            return Err(Error::invalid(format!("{name} is no copy this node holds")).within(what()));
        }
        if !copy::is_ready(&log, &dir.join(&name)) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                format!("{name}: its copy is not whole yet, and a copy becomes a home only whole"),
            )
            .within(what()));
        }
        promoting.push((name, log));
    }
    promoting.into_iter().map(|(name, log)| promote_one(name, &log).map_err(|error| error.within(what()))).collect()
}

pub(crate) fn promote_one(file: String, log: &Path) -> Result<Promoted> {
    let applied = Position::read(log.join(Position::FILE))?.point();
    let point = Point { epoch: applied.epoch + 1, ..applied };
    let mut promoted = Position::read(log.join(PROMOTED))?;
    promoted.keep_durably(point)?;
    for gone in [copy::MARK, Position::FILE] {
        fs::remove_file(log.join(gone)).map_err(|error| io_error(&log.join(gone), error))?;
    }
    // a copy a direct link took is taken from no node once it is a home
    match fs::remove_file(log.join("from")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&log.join("from"), error)),
    }
    crate::durable::sync_directory(log).map_err(|error| io_error(log, error))?;
    Ok(Promoted { file, sequence: point.sequence, epoch: point.epoch })
}

/// A file a demotion made a copy again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demoted {
    /// The file, as `kv.db` or `sql/app.db`.
    pub file: String,
    /// Where its log stood, which the commits its new home never had are lost
    /// past; `None` for a log that held no entry.
    pub last: Option<(u64, u64)>,
}

/// Makes the files of the stopped store in `dir` copies again, an old home
/// coming back once another took its place: those named, or every file it
/// writes but its own records and metrics. Each takes a snapshot of its new
/// home once the node starts with `from` reaching it, and the commits it made
/// that its new home never had are lost.
pub fn demote(dir: &Path, files: &[String]) -> Result<Vec<Demoted>> {
    let what = || format!("demote {}", dir.display());
    let _lock = crate::store::take_lock(dir).map_err(|error| error.within(what()))?;
    let logs = dir.join(DIR);
    let names = if files.is_empty() {
        node::files_logged_in(&logs)?.into_iter().filter(|name| !super::NODE_LOCAL.contains(&name.as_str())).collect()
    } else {
        files.to_vec()
    };
    let mut demoting = Vec::new();
    for name in names {
        let log = logs.join(&name);
        if copy::is_copy(&log) {
            if files.is_empty() {
                continue;
            }
            return Err(Error::invalid(format!("{name} is a copy already")).within(what()));
        }
        if !log.exists() {
            return Err(Error::invalid(format!("{name} keeps no log on this node")).within(what()));
        }
        demoting.push((name, log));
    }
    demoting.into_iter().map(|(name, log)| demote_one(name, &log).map_err(|error| error.within(what()))).collect()
}

fn demote_one(file: String, log: &Path) -> Result<Demoted> {
    let last = super::log::Log::last_in(log)?.map(|point| (point.sequence, point.epoch));
    tracing::warn!(
        target: "tinystore",
        file = %file,
        last = ?last,
        "a file becomes a copy again: the commits its new home never had are lost"
    );
    super::log::Log::clear(log)?;
    match fs::remove_file(log.join(PROMOTED)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(&log.join(PROMOTED), error)),
    }
    copy::mark(log)?;
    Ok(Demoted { file, last })
}

/// The point a promoted copy's log goes on from, if the log in `dir` is one.
pub(crate) fn promoted_at(dir: &Path) -> Result<Option<Point>> {
    Position::read_point(&dir.join(PROMOTED))
}
