//! A scan: the folder walked beside the list in one pass, folder by folder
//! in the byte order of whole paths, and the differences written a thousand
//! lines a commit, so that no write of the program's files waits long
//! behind it. A file is compared by its size and modification time alone,
//! rsync's quick check.
//!
//! ```text
//! the folder   a.txt  b/1.mkv         c.txt
//! the list     a.txt  b/1.mkv  b/2.mkv       d.txt
//! told                         removed added removed
//! ```
//!
//! What it could not read is not gone: a folder it cannot list, a link, a
//! folder on another file system keep what the list holds under them, and
//! the directory's own folder missing, unreadable, or empty while its list
//! is not fails the scan before it writes anything (BLOB-12).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use rusqlite::params;

use super::changes::{MARK_REMOVED, take_changes};
use super::listed::usage_of;
use super::names::{Kind, Name, Names, Sorted};
use super::{Opened, Root, nanos_of};
use crate::blobs::engine::{Blobs, rows_of};
use crate::blobs::list::PAST_EVERY;
use crate::blobs::paths::{MAX_PATH, MAX_SEGMENTS, check_segment};
use crate::sqlite::{Tx, sql_error};
use crate::{Error, ErrorKind, Result};

/// Differences a commit writes at most.
const COMMIT_LINES: usize = 1000;
/// Lines of the list a page reads.
const LIST_PAGE: usize = 1000;

const SELECT_LIST: &str = "select path, size, modified, removed is not null from _tinystore_blobs_listed
    where directory = ?1 and path > ?2 order by path limit cast(?3 as integer)";
const UPSERT: &str = "insert into _tinystore_blobs_listed (directory, path, size, modified, change, added)
    values (?1, ?2, ?3, ?4, ?5, ?5)
    on conflict (directory, path) do update set size = excluded.size, modified = excluded.modified,
        change = excluded.change, added = case when removed is null then added else excluded.change end,
        removed = null";

/// What a scan found: the files it told added, changed and removed, what it
/// met and could not list, and what the list holds once it ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub added: u64,
    pub changed: u64,
    pub removed: u64,
    pub skipped: u64,
    pub count: u64,
    pub size: u64,
}

/// Why a scan passed over what it met, each logged the first time an open
/// directory meets it.
#[derive(Clone, Copy, Debug)]
enum Skipped {
    Unreadable = 0,
    Link = 1,
    /// On Windows a volume mounted in a folder is a reparse point, a link.
    #[cfg(unix)]
    OtherDisk = 2,
    NotText = 3,
    TooLong = 4,
    NotAFile = 5,
}

impl Skipped {
    fn says(self) -> &'static str {
        match self {
            Skipped::Unreadable => "a folder it cannot read, whose files stay listed",
            Skipped::Link => "a link, which a scan does not follow",
            #[cfg(unix)]
            Skipped::OtherDisk => "a folder on another file system, which may be opened as a directory of its own",
            Skipped::NotText => "a name that is not text a path may hold",
            Skipped::TooLong => "a path past 1 KiB or 16 segments",
            Skipped::NotAFile => "neither a file nor a folder",
        }
    }
}

/// The kinds of what was passed over that an open directory logged, a bit each.
#[derive(Debug, Default)]
pub(super) struct Logged(AtomicU8);

/// Scans the directory: its folder walked beside its list, the differences
/// written, and what the list holds then.
pub(super) fn scan(blobs: &Arc<Blobs>, opened: &Opened) -> Result<Scan> {
    let describe = format!("directory {}", opened.name);
    let scanned = (|| {
        if blobs.file.is_copy() {
            let why = "a scan writes the list, so it runs on the home of blobs.db";
            return Err(Error::new(ErrorKind::Unavailable, why));
        }
        let root = opened
            .root
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::Unavailable, "no folder of this machine was given it to scan"))?;
        let mut walk = Walk::new(blobs, opened, root);
        let walked = walk.top();
        // what a cancel or a failure part way left to write stays, as what was written does
        let written = walk.writes.flush(blobs, opened.id);
        walked.and(written)?;
        let listed = usage_of(blobs, opened.id, "", &PAST_EVERY)?;
        Ok(Scan { count: listed.count, size: listed.size, ..walk.scan })
    })();
    let scan = scanned.map_err(|error| error.within(describe))?;
    blobs.probe.count("tinystore_blobs_scans_total", &[], 1.0);
    let told = scan.added + scan.changed + scan.removed;
    blobs.probe.count("tinystore_blobs_scan_changes_total", &[], told as f64);
    Ok(scan)
}

struct Walk<'a> {
    blobs: &'a Arc<Blobs>,
    opened: &'a Opened,
    root: &'a Root,
    names: Names,
    list: ListCursor,
    writes: Writes,
    scan: Scan,
}

impl<'a> Walk<'a> {
    fn new(blobs: &'a Arc<Blobs>, opened: &'a Opened, root: &'a Root) -> Self {
        let writes = Writes { pending: Vec::new(), now: blobs.now() };
        let list = ListCursor { directory: opened.id, page: VecDeque::new(), after: String::new(), ended: false };
        Walk { blobs, opened, root, names: Names::default(), list, writes, scan: Scan::default() }
    }

    /// The directory's own folder, which must be there, readable, and not
    /// empty while the list holds files; then every folder in it.
    fn top(&mut self) -> Result<()> {
        let path = self.root.path.clone();
        let unavailable = |why: String| Error::new(ErrorKind::Unavailable, format!("{}: {why}", path.display()));
        let mut seen = 0;
        let sorted =
            self.read(&path, "", &mut seen).map_err(|error| unavailable(format!("cannot be read: {error}")))?;
        let mut sorted = sorted.ok_or_else(|| unavailable("cannot be read whole".to_owned()))?;
        if seen == 0 {
            let listed = usage_of(self.blobs, self.opened.id, "", &PAST_EVERY)?;
            if listed.count > 0 {
                let why = format!(
                    "found empty while its list holds {} files: a disk not mounted is an empty folder, \
                     and `forget` empties the list of a folder truly emptied",
                    listed.count
                );
                return Err(unavailable(why));
            }
        }
        self.walk_sorted(&mut sorted, &path, "")?;
        self.remove_before(None)
    }

    /// One folder whose path in the list is `prefix`, then the folders in it
    /// at their places in its order; one it cannot read keeps what is under it.
    fn folder(&mut self, path: &Path, prefix: &str) -> Result<()> {
        crate::cancel::check(|| format!("directory {}: a scan, at {prefix:?}", self.opened.name))?;
        let mut seen = 0;
        match self.read(path, prefix, &mut seen) {
            Ok(Some(mut sorted)) => self.walk_sorted(&mut sorted, path, prefix),
            Ok(None) | Err(_) => {
                self.skip(Skipped::Unreadable, prefix);
                self.keep_under(prefix)
            }
        }
    }

    fn walk_sorted(&mut self, sorted: &mut Sorted, path: &Path, prefix: &str) -> Result<()> {
        while let Some(name) = self.names.next(sorted)? {
            let whole = format!("{prefix}{}", name.key);
            match name.kind {
                Kind::File { size, modified } => self.file(whole, size, modified)?,
                Kind::Folder => {
                    let own = name.key.strip_suffix('/').unwrap_or(&name.key);
                    self.folder(&path.join(own), &whole)?;
                }
                Kind::Kept => self.keep_under(&whole)?,
            }
        }
        self.names.done(sorted)
    }

    /// A folder's names sorted, `seen` counting every entry met, those
    /// ignored among them; none when the listing failed part way, since what
    /// it did not give is not known to be gone. `Err` when it cannot be read.
    fn read(&mut self, path: &Path, prefix: &str, seen: &mut usize) -> std::io::Result<Option<Sorted>> {
        let entries = std::fs::read_dir(path)?;
        let depth = prefix.matches('/').count();
        let mut failed = false;
        let (opened, root, scan) = (self.opened, self.root, &mut self.scan);
        let names = entries.filter_map(|entry| match entry {
            Err(_) => {
                failed = true;
                None
            }
            Ok(entry) => {
                *seen += 1;
                let (name, skipped) = name_of(opened, root, &entry, prefix, depth);
                if let Some(skipped) = skipped {
                    scan.skipped += 1;
                    log_skipped(opened, skipped, prefix, &entry);
                }
                name
            }
        });
        let sorted = self.names.sort(depth, names).map_err(std::io::Error::other)?;
        if failed {
            self.names.done(&sorted).map_err(std::io::Error::other)?;
            return Ok(None);
        }
        Ok(Some(sorted))
    }

    /// Merges one file of the folder against the list.
    fn file(&mut self, whole: String, size: u64, modified: i64) -> Result<()> {
        self.remove_before(Some(&whole))?;
        let listed = match self.list.peek(self.blobs)? {
            Some(line) if line.path == whole => Some(self.list.pop()),
            _ => None,
        };
        match listed {
            Some(line) if !line.removed && line.size == size && line.modified == modified => return Ok(()),
            Some(line) if !line.removed => self.scan.changed += 1,
            _ => self.scan.added += 1,
        }
        self.writes.push(self.blobs, self.opened.id, Write::Put { path: whole, size, modified })
    }

    /// Tells removed every listed file before `until`, or all that are left.
    fn remove_before(&mut self, until: Option<&str>) -> Result<()> {
        while let Some(line) = self.list.peek(self.blobs)? {
            if until.is_some_and(|until| line.path.as_str() >= until) {
                return Ok(());
            }
            let line = self.list.pop();
            if !line.removed {
                self.scan.removed += 1;
                self.writes.push(self.blobs, self.opened.id, Write::Remove { path: line.path })?;
            }
        }
        Ok(())
    }

    /// Passes over the listed files under a folder the scan did not read.
    fn keep_under(&mut self, prefix: &str) -> Result<()> {
        self.remove_before(Some(prefix))?;
        while self.list.peek(self.blobs)?.is_some_and(|line| line.path.starts_with(prefix)) {
            self.list.pop();
        }
        Ok(())
    }

    fn skip(&mut self, skipped: Skipped, path: &str) {
        self.scan.skipped += 1;
        log_first(self.opened, skipped, path);
    }
}

/// What a folder's entry is to the scan, and why it is passed over when it
/// is: none for a name `ignore` names, which is not counted.
fn name_of(
    opened: &Opened,
    root: &Root,
    entry: &std::fs::DirEntry,
    prefix: &str,
    depth: usize,
) -> (Option<Name>, Option<Skipped>) {
    let os = entry.file_name();
    let Some(name) = os.to_str() else { return (None, Some(Skipped::NotText)) };
    if opened.ignore.passes_over(name) {
        return (None, None);
    }
    if check_segment(name).is_err() {
        return (None, Some(Skipped::NotText));
    }
    if prefix.len() + name.len() > MAX_PATH || depth + 1 > MAX_SEGMENTS {
        return (None, Some(Skipped::TooLong));
    }
    let (kind, skipped) = match kind_of(root, entry) {
        Ok(kind) => (kind, None),
        Err(skipped) => (Kind::Kept, Some(skipped)),
    };
    let key = if matches!(kind, Kind::File { .. }) { name.to_owned() } else { format!("{name}/") };
    (Some(Name { key, kind }), skipped)
}

/// What an entry is, from the listing's own attributes on Windows and a
/// stat that follows no link on Unix; a link, a folder on another disk and
/// what is neither file nor folder are passed over, keeping what the list
/// holds under them.
fn kind_of(root: &Root, entry: &std::fs::DirEntry) -> std::result::Result<Kind, Skipped> {
    let file_type = entry.file_type().map_err(|_| Skipped::Unreadable)?;
    if file_type.is_symlink() {
        return Err(Skipped::Link);
    }
    let metadata = entry.metadata().map_err(|_| Skipped::Unreadable)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // a junction and a volume mounted in a folder are reparse points, as are other links
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Skipped::Link);
        }
        let _ = root;
    }
    #[cfg(unix)]
    if metadata.is_dir() && std::os::unix::fs::MetadataExt::dev(&metadata) != root.device {
        return Err(Skipped::OtherDisk);
    }
    if metadata.is_dir() {
        return Ok(Kind::Folder);
    }
    if !metadata.is_file() {
        return Err(Skipped::NotAFile);
    }
    let modified = metadata.modified().map_err(|_| Skipped::Unreadable)?;
    Ok(Kind::File { size: metadata.len(), modified: nanos_of(modified) })
}

fn log_skipped(opened: &Opened, skipped: Skipped, prefix: &str, entry: &std::fs::DirEntry) {
    let name = entry.file_name();
    log_first(opened, skipped, &format!("{prefix}{}", name.to_string_lossy()));
}

/// Logs the first of each kind an open directory passes over, and nothing
/// after it: a scan every few minutes would otherwise log the same link each time.
fn log_first(opened: &Opened, skipped: Skipped, path: &str) {
    let bit = 1 << skipped as u8;
    if opened.logged.0.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        tracing::info!(target: "tinystore", directory = %opened.name, %path, "blobs: a scan passes over {}", skipped.says());
    }
}

/// A line of the list as the scan compares it.
struct Line {
    path: String,
    size: u64,
    modified: i64,
    removed: bool,
}

/// The list read in path order a page at a time, so that what it holds in
/// memory is one page, whatever the list's length. A page is read before the
/// scan writes a path past the last it read, so it never reads back a line
/// the scan itself added.
struct ListCursor {
    directory: i64,
    page: VecDeque<Line>,
    after: String,
    ended: bool,
}

impl ListCursor {
    fn peek(&mut self, blobs: &Blobs) -> Result<Option<&Line>> {
        if self.page.is_empty() && !self.ended {
            let limit = i64::try_from(LIST_PAGE).unwrap_or(i64::MAX);
            let (directory, after) = (self.directory, self.after.clone());
            let lines: Vec<Line> = blobs.file.read(|connection| {
                rows_of(
                    connection,
                    (SELECT_LIST, "blobs: a directory's list"),
                    params![directory, after, limit],
                    |row| {
                        Ok(Line {
                            path: row.get(0)?,
                            size: row.get::<_, i64>(1)?.cast_unsigned(),
                            modified: row.get(2)?,
                            removed: row.get(3)?,
                        })
                    },
                )
            })?;
            self.ended = lines.len() < LIST_PAGE;
            if let Some(last) = lines.last() {
                self.after.clone_from(&last.path);
            }
            self.page.extend(lines);
        }
        Ok(self.page.front())
    }

    fn pop(&mut self) -> Line {
        self.page.pop_front().expect("a line peeked before it is taken")
    }
}

enum Write {
    Put { path: String, size: u64, modified: i64 },
    Remove { path: String },
}

/// The scan's differences, written a thousand lines a commit, each line a
/// change number of its own in the order the scan met them.
struct Writes {
    pending: Vec<Write>,
    /// The store's clock when the scan began, as a removed line keeps it.
    now: i64,
}

impl Writes {
    fn push(&mut self, blobs: &Blobs, directory: i64, write: Write) -> Result<()> {
        self.pending.push(write);
        if self.pending.len() >= COMMIT_LINES {
            return self.flush(blobs, directory);
        }
        Ok(())
    }

    fn flush(&mut self, blobs: &Blobs, directory: i64) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = std::mem::take(&mut self.pending);
        let now = self.now;
        blobs.file.write(0, move |tx| write_lines(tx, directory, &pending, now))
    }
}

fn write_lines(tx: &Tx<'_>, directory: i64, pending: &[Write], now: i64) -> Result<()> {
    let failed = |error| sql_error("blobs: a directory's list", error);
    let last = take_changes(tx, directory, pending.len())?;
    let first = last - i64::try_from(pending.len()).unwrap_or(0);
    let mut put = tx.prepare_cached(UPSERT).map_err(failed)?;
    for (write, change) in pending.iter().zip(first + 1..) {
        match write {
            Write::Put { path, size, modified } => {
                put.execute(params![directory, path, size.cast_signed(), modified, change]).map_err(failed)?;
            }
            Write::Remove { path } => {
                let mut mark = tx.prepare_cached(MARK_REMOVED).map_err(failed)?;
                mark.execute(params![directory, path, now, change]).map_err(failed)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
