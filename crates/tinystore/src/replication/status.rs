//! What a node holds, read from its directory: each file it logs, whether it
//! writes the file or copies it, the entries its log keeps and where its
//! copy stands. It reads files alone, so a node running meanwhile is read as
//! it stands.

use std::path::Path;

use super::copy::{self, Position};
use super::log::{Log, segments_in};
use super::{DIR, node, promote};
use crate::Result;

/// One file of a node as its directory holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// The file, as `kv.db` or `sql/app.db`.
    pub file: String,
    /// Whether this node writes the file, or copies it from another.
    pub home: bool,
    /// The first and the last entry its log keeps, none for a log that keeps
    /// none.
    pub entries: Option<(u64, u64)>,
    /// The epoch of its last entry, or of the point a promotion left or a
    /// copy applied.
    pub epoch: Option<u64>,
    /// A copy's last entry applied; none for a home's file or a copy that
    /// waits for its first snapshot.
    pub applied: Option<u64>,
    /// A snapshot of it is being put together.
    pub taking: bool,
    /// The file became this node's own by a promotion.
    pub promoted: bool,
}

/// Every file the node in `dir` logs.
pub fn status(dir: &Path) -> Result<Vec<FileStatus>> {
    let logs = dir.join(DIR);
    let mut files = Vec::new();
    for name in node::files_logged_in(&logs)?.into_iter().chain(promoted_alone(&logs)?) {
        let log = logs.join(&name);
        let home = !copy::is_copy(&log);
        let last = Log::last_in(&log)?;
        let first = segments_in(&log)?.first().copied();
        let promoted = promote::promoted_at(&log)?;
        let applied = if home || !copy::is_ready(&log, &dir.join(&name)) {
            None
        } else {
            Position::read_point(&log.join(Position::FILE))?
        };
        files.push(FileStatus {
            entries: first.zip(last.map(|point| point.sequence)),
            epoch: last.or(promoted).or(applied).map(|point| point.epoch),
            applied: applied.map(|point| point.sequence),
            taking: log.join("taking").exists(),
            promoted: promoted.is_some(),
            home,
            file: name,
        });
    }
    files.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(files)
}

/// The files a promotion made a node's own whose logs have kept no entry
/// since, which hold no segment yet.
fn promoted_alone(logs: &Path) -> Result<Vec<String>> {
    let logged = node::files_logged_in(logs)?;
    let mut alone = Vec::new();
    walk(logs, "", &mut |name, path| {
        if path.join(promote::PROMOTED).exists() && !logged.iter().any(|known| known == name) {
            alone.push(name.to_owned());
        }
    })?;
    Ok(alone)
}

fn walk(dir: &Path, prefix: &str, found: &mut dyn FnMut(&str, &Path)) -> Result<()> {
    let Ok(listed) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in listed.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else { continue };
        let joined = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        found(&joined, &entry.path());
        walk(&entry.path(), &joined, found)?;
    }
    Ok(())
}
