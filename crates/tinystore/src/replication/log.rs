//! A file's log: its commits as entries, in segment files of
//! `log/<file>/`, each named by the sequence of its first entry.
//!
//! A home's log is appended while the file's writer is held, with no sync of
//! its own: the WAL holds the commit durably until the capture's checkpoint,
//! and the checkpoint syncs the log first. A segment's tail torn by a crash is
//! cut off at open, and the capture hands what it lost again from the WAL.
//!
//! A copy keeps a log too, of the entries it applied as they came, so that
//! the nodes that copy it read the same bytes its home wrote.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::entry::{self, Entry, Head, Kind, MAGIC, Point};
use crate::sqlite::{Commit, Observe, Position};
use crate::{Error, ErrorKind, Result};

/// How large a log grows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bounds {
    /// A segment past these bytes is closed, and the next entry starts one.
    pub(crate) segment: u64,
    /// The bytes of a file's segments kept; the oldest go past them, and a
    /// copy that needed them starts again from a snapshot.
    pub(crate) keep: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Bounds { segment: 16 << 20, keep: 256 << 20 }
    }
}

/// What a node's links wait on for the next entry of any of its logs.
#[derive(Default)]
pub(crate) struct Notify {
    appended: Mutex<u64>,
    changed: Condvar,
}

impl Notify {
    /// How many entries were appended so far, to wait past.
    pub(crate) fn seen(&self) -> u64 {
        *self.appended.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until an entry is appended after `seen`, `at_most` long.
    pub(crate) fn wait_past(&self, seen: u64, at_most: Duration) {
        let appended = self.appended.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self.changed.wait_timeout_while(appended, at_most, |appended| *appended == seen);
    }

    pub(crate) fn appended(&self) {
        *self.appended.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.changed.notify_all();
    }
}

/// What a synchronous home's commits wait for: the entry of each file that
/// a node copying it said it holds.
#[derive(Default)]
pub(crate) struct Acks {
    held: Mutex<HashMap<String, u64>>,
    changed: Condvar,
    /// Links that follow a file of this node now: with none, a commit waits
    /// for no one, and says so once.
    linked: AtomicUsize,
    said_alone: AtomicBool,
}

/// How long a synchronous commit waits for a copy to hold it before it
/// answers anyway, logged.
const ACK_WAIT: Duration = Duration::from_secs(5);

impl Acks {
    /// A node that copies `file` holds its entries up to `sequence`.
    pub(crate) fn held(&self, file: &str, sequence: u64) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let at = held.entry(file.to_owned()).or_default();
        if sequence > *at {
            *at = sequence;
            self.changed.notify_all();
        }
    }

    pub(crate) fn linked(&self) {
        self.linked.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn unlinked(&self) {
        self.linked.fetch_sub(1, Ordering::SeqCst);
        self.changed.notify_all();
    }

    /// Waits until a node that copies `file` holds entry `sequence`, a bound
    /// at most, or not at all while none follows it.
    fn wait(&self, file: &str, sequence: u64) {
        let deadline = Instant::now() + ACK_WAIT;
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while held.get(file).copied().unwrap_or(0) < sequence {
            if self.linked.load(Ordering::SeqCst) == 0 {
                if !self.said_alone.swap(true, Ordering::SeqCst) {
                    tracing::warn!(target: "tinystore", file, "a synchronous home commits with no node copying it");
                }
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                tracing::warn!(target: "tinystore", file, entry = sequence, "no copy held a synchronous commit in time");
                return;
            }
            held = self.changed.wait_timeout(held, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
        self.said_alone.store(false, Ordering::SeqCst);
    }
}

pub(crate) struct Log {
    dir: PathBuf,
    bounds: Bounds,
    notify: Arc<Notify>,
    /// A synchronous home's file and what its commits wait for.
    synchronous: Option<(String, Arc<Acks>)>,
    /// The writer of a home's file, held to read where the log stands with
    /// no commit between the file and the log.
    writer: Mutex<Option<Weak<Mutex<Connection>>>>,
    tail: Mutex<Tail>,
    /// The later epoch another node writes the file at, which this node, an
    /// old home, learned of; 0 while none.
    fenced: AtomicU64,
}

/// The file in a home's log directory that keeps the later epoch it learned
/// another node writes the file at: its writes are refused from then on, and
/// across its restarts, until a person demotes it.
pub(crate) const FENCED: &str = "fenced";

struct Tail {
    /// Every segment, oldest first; appends go to the last.
    segments: VecDeque<Segment>,
    appending: Option<fs::File>,
    next: u64,
    epoch: u64,
    check: [u8; 16],
    last: Option<Position>,
    unsynced: bool,
    /// A segment was made since the directory's last sync.
    named: bool,
}

struct Segment {
    first: u64,
    bytes: u64,
}

impl Log {
    /// Opens the log in `dir`, its last segment's torn tail cut off.
    pub(crate) fn open(dir: PathBuf, bounds: Bounds, notify: Arc<Notify>) -> Result<Log> {
        fs::create_dir_all(&dir).map_err(|error| Error::io(format!("log {}", dir.display()), error))?;
        let mut tail = Tail {
            segments: VecDeque::new(),
            appending: None,
            next: 1,
            epoch: 1,
            check: [0; 16],
            last: None,
            unsynced: false,
            named: false,
        };
        for first in segments_in(&dir)? {
            let path = segment_path(&dir, first);
            let bytes = fs::metadata(&path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
            tail.segments.push_back(Segment { first, bytes });
        }
        let fenced = AtomicU64::new(Self::fenced_in(&dir)?.unwrap_or(0));
        let log =
            Log { dir, bounds, notify, synchronous: None, writer: Mutex::new(None), tail: Mutex::new(tail), fenced };
        log.find_the_end().map_err(|error| error.within(format!("log {}", log.dir.display())))?;
        log.go_on_from_a_promotion()?;
        Ok(log)
    }

    /// Makes each commit of the file `name` wait until a node copying it holds
    /// it, while the writer is held: the commits queued meanwhile share the
    /// next, and its wait.
    pub(crate) fn synchronous(mut self, name: &str, acks: Arc<Acks>) -> Log {
        self.synchronous = Some((name.to_owned(), acks));
        self
    }

    /// Every entry the log in `dir` holds, oldest first.
    #[cfg(test)]
    pub(crate) fn read(dir: &Path, mut each: impl FnMut(Entry<'_>) -> Result<()>) -> Result<()> {
        for first in segments_in(dir)? {
            let bytes = read_segment(&segment_path(dir, first))?;
            let mut at = MAGIC.len();
            while let Some((entry, used)) = entry::decode(&bytes[at..])? {
                each(entry)?;
                at += used;
            }
        }
        Ok(())
    }

    /// Where the log in `dir` ends, read from its segments with no writer
    /// open on it: `None` for a log that holds no entry.
    pub(crate) fn last_in(dir: &Path) -> Result<Option<Point>> {
        for first in segments_in(dir)?.into_iter().rev() {
            let bytes = read_segment(&segment_path(dir, first))?;
            let mut at = MAGIC.len();
            let mut last = None;
            while let Some((entry, used)) = entry::decode(&bytes[at..]).unwrap_or(None) {
                if last.is_some_and(|point| !entry.follows(&point)) {
                    break;
                }
                last = Some(entry.point());
                at += used;
            }
            if last.is_some() {
                return Ok(last);
            }
        }
        Ok(None)
    }

    /// Removes every segment of the log in `dir`, for a copy that starts again
    /// from a snapshot.
    pub(crate) fn clear(dir: &Path) -> Result<()> {
        for first in segments_in(dir)? {
            let path = segment_path(dir, first);
            fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
        }
        match fs::remove_file(dir.join(FENCED)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(&dir.join(FENCED), error)),
        }
    }

    /// Refuses this log's file every write from now on: another node writes it
    /// at `epoch`, later than this log's.
    pub(crate) fn fence(&self, epoch: u64) -> Result<()> {
        Self::fence_in(&self.dir, epoch)?;
        self.fenced.fetch_max(epoch, Ordering::SeqCst);
        Ok(())
    }

    /// Keeps durably, in the log directory `dir`, that another node writes its
    /// file at `epoch`.
    pub(crate) fn fence_in(dir: &Path, epoch: u64) -> Result<()> {
        fs::create_dir_all(dir).map_err(|error| io_error(dir, error))?;
        let path = dir.join(FENCED);
        let mut file = fs::File::create(&path).map_err(|error| io_error(&path, error))?;
        file.write_all(epoch.to_string().as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| io_error(&path, error))?;
        crate::durable::sync_directory(dir).map_err(|error| io_error(dir, error))
    }

    /// The later epoch another node writes the file of the log in `dir` at,
    /// if this node learned of one.
    pub(crate) fn fenced_in(dir: &Path) -> Result<Option<u64>> {
        match fs::read_to_string(dir.join(FENCED)) {
            Ok(kept) => kept
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| Error::corrupt(format!("{}: no epoch", dir.join(FENCED).display()))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_error(&dir.join(FENCED), error)),
        }
    }

    /// Where the log ends now. For a home's file the writer is held while it
    /// reads, so that no commit is in the file and not yet in the log: every
    /// reader of the file that ended before sees no more than this.
    pub(crate) fn stable(&self) -> Point {
        let writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(Weak::upgrade);
        let _held = writer.as_ref().map(|writer| writer.lock().unwrap_or_else(PoisonError::into_inner));
        let tail = self.lock();
        Point { sequence: tail.next - 1, epoch: tail.epoch, check: tail.check }
    }

    /// Keeps an entry a copy applied, as its home wrote it: the first of an
    /// empty log, or the one after its last.
    pub(crate) fn keep(&self, entry: &Entry<'_>, bytes: &[u8]) -> Result<()> {
        let mut tail = self.lock();
        let empty = tail.segments.is_empty();
        if !empty && !entry.follows(&Point { sequence: tail.next - 1, epoch: tail.epoch, check: tail.check }) {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "log {}: entry {} does not follow its last, {}",
                    self.dir.display(),
                    entry.sequence,
                    tail.next - 1
                ),
            ));
        }
        if empty {
            tail.next = entry.sequence;
        }
        self.write(&mut tail, bytes)?;
        (tail.next, tail.epoch, tail.check, tail.last) =
            (entry.sequence + 1, entry.epoch, entry.check, Some(entry.position));
        drop(tail);
        self.notify.appended();
        Ok(())
    }

    /// Goes on from the point a promoted copy stood at, at its new epoch: a
    /// copy whose log kept no entry since its snapshot goes on after its
    /// position, and the WAL it holds is no generation of its old home's.
    fn go_on_from_a_promotion(&self) -> Result<()> {
        let Some(point) = super::promote::promoted_at(&self.dir)? else {
            return Ok(());
        };
        let mut tail = self.lock();
        if tail.segments.is_empty() {
            (tail.next, tail.check, tail.last) = (point.sequence + 1, point.check, Some(Position::default()));
        }
        tail.epoch = tail.epoch.max(point.epoch);
        Ok(())
    }

    /// Reads the last segment that holds an entry to the end of its last
    /// whole one, cuts what follows, and removes the segments after it,
    /// which a crash left before their first entry.
    fn find_the_end(&self) -> Result<()> {
        let mut tail = self.lock();
        while let Some(segment) = tail.segments.back() {
            let path = segment_path(&self.dir, segment.first);
            let bytes = read_segment(&path)?;
            let mut at = MAGIC.len();
            let mut last: Option<(Point, Position)> = None;
            while let Some((entry, used)) = entry::decode(&bytes[at..]).unwrap_or(None) {
                if last.is_some_and(|(point, _)| !entry.follows(&point)) {
                    break;
                }
                last = Some((entry.point(), entry.position));
                at += used;
            }
            let Some((point, position)) = last else {
                fs::remove_file(&path).map_err(|error| Error::io(path.display().to_string(), error))?;
                tail.segments.pop_back();
                continue;
            };
            if at < bytes.len() {
                let file = OpenOptions::new().write(true).open(&path).map_err(|error| io_error(&path, error))?;
                file.set_len(at as u64).and_then(|()| file.sync_data()).map_err(|error| io_error(&path, error))?;
            }
            tail.segments.back_mut().expect("the segment just read").bytes = at as u64;
            (tail.next, tail.epoch, tail.check, tail.last) =
                (point.sequence + 1, point.epoch, point.check, Some(position));
            tail.appending = Some(OpenOptions::new().append(true).open(&path).map_err(|error| io_error(&path, error))?);
            return Ok(());
        }
        Ok(())
    }

    fn append(&self, kind: Kind, position: Position, page_size: u32, size: u32, pages: &[(u32, &[u8])]) -> Result<()> {
        let mut tail = self.lock();
        let head =
            Head { kind, sequence: tail.next, epoch: tail.epoch, previous: tail.check, position, page_size, size };
        let (bytes, check) = entry::encode(&head, pages);
        self.write(&mut tail, &bytes)?;
        (tail.next, tail.check, tail.last) = (head.sequence + 1, check, Some(position));
        drop(tail);
        self.notify.appended();
        if let (Kind::Commit, Some((file, acks))) = (kind, &self.synchronous) {
            acks.wait(file, head.sequence);
        }
        Ok(())
    }

    /// Appends an entry's bytes to the last segment, or to a new one when
    /// the last is full.
    fn write(&self, tail: &mut Tail, bytes: &[u8]) -> Result<()> {
        let full = tail.segments.back().is_none_or(|segment| segment.bytes >= self.bounds.segment);
        if full {
            self.start_segment(tail)?;
        }
        let path = segment_path(&self.dir, tail.segments.back().expect("a segment to append to").first);
        let file = tail.appending.as_mut().expect("the last segment is open");
        file.write_all(bytes).map_err(|error| io_error(&path, error))?;
        tail.segments.back_mut().expect("the segment just written").bytes += bytes.len() as u64;
        tail.unsynced = true;
        Ok(())
    }

    /// Closes the segment appended to, durably, starts the next at the next
    /// entry's sequence, and removes the oldest past the bytes kept.
    fn start_segment(&self, tail: &mut Tail) -> Result<()> {
        if let Some(file) = tail.appending.take() {
            file.sync_data().map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
        }
        let path = segment_path(&self.dir, tail.next);
        let mut file =
            OpenOptions::new().append(true).create_new(true).open(&path).map_err(|error| io_error(&path, error))?;
        file.write_all(MAGIC).map_err(|error| io_error(&path, error))?;
        tail.segments.push_back(Segment { first: tail.next, bytes: MAGIC.len() as u64 });
        tail.appending = Some(file);
        tail.named = true;
        while tail.segments.len() > 1
            && tail.segments.iter().map(|segment| segment.bytes).sum::<u64>() > self.bounds.keep
        {
            let oldest = tail.segments.pop_front().expect("more than one segment");
            let path = segment_path(&self.dir, oldest.first);
            fs::remove_file(&path).map_err(|error| io_error(&path, error))?;
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, Tail> {
        self.tail.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Observe for Log {
    fn last(&self) -> Option<Position> {
        self.lock().last
    }

    fn begin(&self, position: Position, size: u32) -> Result<()> {
        self.append(Kind::Begin, position, 0, size, &[])
    }

    fn committed(&self, commit: &Commit<'_>) -> Result<()> {
        self.append(Kind::Commit, commit.position, commit.page_size, commit.size, &commit.pages)
    }

    fn gap(&self, position: Position) -> Result<()> {
        tracing::warn!(
            target: "tinystore",
            log = %self.dir.display(),
            "the file was written beside the store's writer: a copy of it starts again from a snapshot"
        );
        self.append(Kind::Gap, position, 0, 0, &[])
    }

    fn sync(&self) -> Result<()> {
        let mut tail = self.lock();
        if tail.unsynced {
            let file = tail.appending.as_ref().expect("an entry was appended");
            file.sync_data().map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
            tail.unsynced = false;
        }
        if tail.named {
            crate::durable::sync_directory(&self.dir)
                .map_err(|error| Error::io(format!("log {}", self.dir.display()), error))?;
            tail.named = false;
        }
        Ok(())
    }

    fn writer(&self, writer: Weak<Mutex<Connection>>) {
        *self.writer.lock().unwrap_or_else(PoisonError::into_inner) = Some(writer);
    }

    fn refuse(&self) -> Result<()> {
        let epoch = self.fenced.load(Ordering::SeqCst);
        if epoch == 0 {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "another node writes this file at epoch {epoch}: this node is its old home, which takes no more writes \
                 of it until `tinystore demote` makes it a copy"
            ),
        ))
    }
}

/// The first sequences of the segments in `dir`, in order.
pub(crate) fn segments_in(dir: &Path) -> Result<Vec<u64>> {
    let mut firsts = Vec::new();
    let listed = match fs::read_dir(dir) {
        Ok(listed) => listed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(firsts),
        Err(error) => return Err(io_error(dir, error)),
    };
    for found in listed {
        let found = found.map_err(|error| io_error(dir, error))?;
        let name = found.file_name();
        let first = name.to_str().and_then(|name| name.strip_suffix(".log")).and_then(|first| first.parse().ok());
        if let Some(first) = first {
            firsts.push(first);
        }
    }
    firsts.sort_unstable();
    Ok(firsts)
}

pub(crate) fn segment_path(dir: &Path, first: u64) -> PathBuf {
    dir.join(format!("{first:020}.log"))
}

fn read_segment(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).map_err(|error| io_error(path, error))?;
    if bytes.len() < MAGIC.len() {
        return Ok(MAGIC.to_vec());
    }
    if bytes[..MAGIC.len()] != MAGIC[..] {
        return Err(Error::new(ErrorKind::Corrupt, format!("{}: a segment of another format", path.display())));
    }
    Ok(bytes)
}

fn io_error(path: &Path, error: io::Error) -> Error {
    Error::io(path.display().to_string(), error)
}
