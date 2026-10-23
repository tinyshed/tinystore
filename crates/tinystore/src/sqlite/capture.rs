//! A file's commits handed, as the pages each one wrote, to an observer that
//! keeps them: what other nodes' copies of the file are made from.
//!
//! The writer's WAL hook says where the WAL ends after each commit. The frames
//! between the last commit handed and that end are the commit's, and they are
//! read back from the `-wal` file while the writer is still held, so that
//! commits are handed in the order they committed.
//!
//! The hook replaces SQLite's automatic checkpoint. A checkpoint lets the next
//! write start the WAL over, and the frames it held are gone; so the capture
//! checkpoints by itself, the writer held, only once the observer has made
//! every commit handed durable. Until then the WAL keeps them, and a process
//! that dies loses nothing: the next open hands the observer what the WAL
//! holds past its last position before any write.
//!
//! A commit SQLite made and the capture could not hand is in the file but not
//! in the observer's keeping. Its write answers `OutcomeUnknown`, and the file
//! takes no write until it is opened again and its WAL handed. A commit the
//! observer kept and no copy held in time answers `CommittedLocally`: it is in
//! the file and the log alike, and the file takes writes on.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use rusqlite::Connection;
use rusqlite::hooks::Wal;

use super::Config;
use super::checkpoint::{HARD, restart, restart_again_at};
use super::connection::sql_error;
use super::given::own;
use crate::{Error, ErrorKind, Result};

const HEADER: usize = 32;
const FRAME_HEADER: usize = 24;
/// What SQLite's own automatic checkpoint waits for.
pub(super) const CHECKPOINT_AT: u32 = 1000;
const CHECKPOINT: &str = own!("pragma wal_checkpoint(passive)");
/// How long a commit's frames wait for room in the store's memory before they
/// are held past its limit.
const HOLD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1);

thread_local! {
    /// Where the WAL ends after the commit this thread just made, left by the
    /// writer's hook for the code that called the commit.
    static WAL_END: Cell<Option<u32>> = const { Cell::new(None) };
    /// The commit this thread is making answers a caller, whom a synchronous
    /// home answers once a copy holds it.
    static ANSWERING: Cell<bool> = const { Cell::new(false) };
    /// This thread does the store's own work, as [`own_work`] runs it.
    static OWN_WORK: Cell<bool> = const { Cell::new(false) };
}

/// Runs `work` as the store's own: an engine's opening, its migrations, the
/// names it makes and its maintenance. Its commits answer no caller, so a
/// synchronous home ships them without waiting for a copy, and none of it
/// fails for want of one.
pub(crate) fn own_work<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            OWN_WORK.set(self.0);
        }
    }
    let _restore = Restore(OWN_WORK.replace(true));
    work()
}

pub(crate) fn is_own_work() -> bool {
    OWN_WORK.get()
}

/// Whether the commit being handed answers a caller.
pub(crate) fn answering() -> bool {
    ANSWERING.get()
}

/// Where a file's WAL stood: its generation, which each start of the WAL
/// over changes, and a frame of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Position {
    pub(crate) checkpoints: u32,
    pub(crate) salts: [u32; 2],
    pub(crate) frame: u32,
}

impl Position {
    pub(crate) fn same_generation(&self, other: &Position) -> bool {
        self.checkpoints == other.checkpoints && self.salts == other.salts
    }

    /// Whether a WAL that now stands at `next` started over only by the
    /// capture's own checkpoint, once, or is the first since there was none:
    /// what it held before is all handed.
    fn followed_by(&self, next: &Position) -> bool {
        *self == Position::default() || next.checkpoints == self.checkpoints.wrapping_add(1)
    }
}

/// One commit's pages, each once, as the commit left it.
pub(crate) struct Commit<'a> {
    /// Where the WAL ends with this commit.
    pub(crate) position: Position,
    pub(crate) page_size: u32,
    /// The database's pages after it: fewer than before truncated the file.
    pub(crate) size: u32,
    pub(crate) pages: Vec<(u32, &'a [u8])>,
}

/// What keeps a file's commits. Its calls run with the file's writer held,
/// so none may wait for anything that waits for that writer.
pub(crate) trait Observe: Send + Sync {
    /// Where the commits it keeps end, or `None` when it keeps none yet.
    fn last(&self) -> Option<Position>;

    /// It starts keeping the file as it stands at `position`, `size` pages;
    /// what came before is kept nowhere.
    fn begin(&self, position: Position, size: u32) -> Result<()>;

    /// Keeps a commit; `CommittedLocally` keeps it and says no copy held it.
    fn committed(&self, commit: &Commit<'_>) -> Result<()>;

    /// The WAL started over without the capture's checkpoint: what a writer
    /// beside the store wrote before `position` is kept nowhere.
    fn gap(&self, position: Position) -> Result<()>;

    /// Makes every commit handed so far durable, before a checkpoint lets the
    /// WAL forget them.
    fn sync(&self) -> Result<()>;

    /// The file's writer, which commits and hands each commit while held:
    /// holding it, the observer knows that what it keeps is the whole file.
    fn writer(&self, writer: Weak<Mutex<Connection>>) {
        let _ = writer;
    }

    /// Refuses a write before it runs, as a log whose file another node writes
    /// now at a later epoch refuses every one.
    fn refuse(&self) -> Result<()> {
        Ok(())
    }
}

/// What hands a file's commits to its log, shared by the file and its
/// group: a node sets it and takes it away as the file's home moves, and a
/// file no log keeps holds none.
pub(crate) type Logged = Arc<std::sync::RwLock<Option<Arc<Capture>>>>;

/// Whether a commit's outcome leaves it in the file, its hooks to run: kept,
/// or kept with no copy holding it in time.
pub(crate) fn in_the_file(outcome: &Result<()>) -> bool {
    outcome.as_ref().err().is_none_or(|error| error.kind() == ErrorKind::CommittedLocally)
}

/// The capture a file's commits go through now, if any.
pub(crate) fn logged(cell: &Logged) -> Option<Arc<Capture>> {
    cell.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// A file's capture: its writer's hook, and where the last commit it handed
/// ended.
pub(crate) struct Capture {
    observer: Arc<dyn Observe>,
    wal: PathBuf,
    /// The store's memory, which a commit's frames are held under while read.
    memory: Option<Arc<crate::Memory>>,
    /// The writer's own, which a restart of the WAL shortens a moment.
    busy_timeout: std::time::Duration,
    state: Mutex<State>,
    broken: AtomicBool,
}

struct State {
    /// The `-wal` file, opened at the first commit that wrote one.
    file: Option<fs::File>,
    /// The end of the last commit handed.
    at: Position,
    /// The frame the capture's last checkpoint reached in this generation.
    checkpointed: u32,
    /// The observer holds every frame of the WAL durably, as after the
    /// capture's checkpoint or with no WAL at all: a WAL that starts over,
    /// whatever its generation, loses nothing.
    restart_expected: bool,
    /// The frames past [`HARD`] at which the capture next restarts the WAL,
    /// once a restart waited in vain for a read; 0 when none did.
    restart_from: u32,
}

impl Capture {
    /// Hooks `writer`, then hands `observer` the commits its WAL holds past
    /// the observer's last position, before any write.
    pub(crate) fn open(
        path: &Path,
        writer: &Connection,
        observer: Arc<dyn Observe>,
        config: &Config,
    ) -> Result<Capture> {
        writer.wal_hook(Some(remember_the_end));
        let state =
            State { file: None, at: Position::default(), checkpointed: 0, restart_expected: true, restart_from: 0 };
        let (wal, broken) = (wal_of(path), AtomicBool::new(false));
        let (memory, busy_timeout) = (config.memory.clone(), config.busy_timeout);
        let capture = Capture { observer, wal, memory, busy_timeout, state: Mutex::new(state), broken };
        capture.recover(writer).map_err(|error| error.within(format!("{}: its WAL", path.display())))?;
        Ok(capture)
    }

    /// Refuses a write once a commit could not be handed, the file ahead of
    /// what its observer keeps until it opens again, or once the observer
    /// refuses writes.
    pub(crate) fn refuse_a_write(&self) -> Result<()> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "a commit is in the file and not in its log: it takes writes again once opened again",
            ));
        }
        self.observer.refuse()
    }

    /// Readies the hook for a commit this thread is about to make, which
    /// answers a caller or is the store's own.
    pub(crate) fn arm(&self, answering: bool) {
        WAL_END.set(None);
        ANSWERING.set(answering);
    }

    /// Hands the commit this thread just made to the observer, the writer
    /// still held, and checkpoints once the WAL is long. A commit that wrote
    /// nothing hands nothing.
    pub(crate) fn committed(&self, writer: &Connection) -> Result<()> {
        let Some(end) = WAL_END.take() else {
            return Ok(());
        };
        let handed = self.hand(end);
        ANSWERING.set(false);
        let broken = |error: Error| {
            self.broken.store(true, Ordering::SeqCst);
            Error::new(ErrorKind::OutcomeUnknown, "a commit is in the file and not in its log").with_source(error)
        };
        if !in_the_file(&handed) {
            return handed.map_err(broken);
        }
        self.checkpoint_when_long(writer, end, broken)?;
        handed
    }

    /// Makes what was handed durable before the file's last checkpoint,
    /// after which the WAL starts over.
    pub(crate) fn close(&self) -> Result<()> {
        self.observer.sync()?;
        let mut state = self.lock();
        state.file = None;
        state.restart_expected = true;
        Ok(())
    }

    fn hand(&self, end: u32) -> Result<()> {
        let mut state = self.lock();
        let State { file, at, checkpointed, restart_expected, restart_from } = &mut *state;
        let wal = open_once(file, &self.wal)?;
        let header = read_header(wal)?;
        let mut from = at.frame + 1;
        if !at.same_generation(&header.at(0)) {
            if !*restart_expected && !at.followed_by(&header.at(0)) {
                self.observer.gap(header.at(0))?;
            }
            from = 1;
            (*checkpointed, *restart_from) = (0, 0);
        }
        if end < from {
            return Ok(());
        }
        let _held = self.hold(&header, from, end);
        let frames = read_frames(wal, &header, from, end)?;
        let commit = commit_of(&header, end, &frames)?;
        let kept = self.observer.committed(&commit);
        if in_the_file(&kept) {
            *at = commit.position;
            *restart_expected = false;
        }
        kept
    }

    /// Checkpoints each time the WAL grew by as many frames as SQLite's own
    /// checkpoint waits for, so that a reader that keeps the WAL from
    /// starting over does not cost a sync every commit.
    /// A log that fails to sync no longer holds what it was handed, so the
    /// file is `broken`; a checkpoint that fails after it leaves the commit
    /// logged and is tried again at the next commit.
    ///
    /// Past [`HARD`] it restarts the WAL, waiting a moment for the reads
    /// begun before, which reads that never stop would keep from starting
    /// over for good, as the checkpoints of a file no node logs do.
    fn checkpoint_when_long(&self, writer: &Connection, end: u32, broken: impl FnOnce(Error) -> Error) -> Result<()> {
        let restart_from = {
            let state = self.lock();
            if end < state.checkpointed + CHECKPOINT_AT {
                return Ok(());
            }
            state.restart_from
        };
        self.observer.sync().map_err(broken)?;
        let restarting = end >= HARD && end >= restart_from;
        let checkpointed = if restarting {
            restart(writer, self.busy_timeout)
        } else {
            writer.query_row(CHECKPOINT, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        };
        let (busy, frames, copied): (i64, i64, i64) =
            checkpointed.map_err(|error| sql_error("a checkpoint of a commit already logged", error))?;
        let mut state = self.lock();
        state.checkpointed = end;
        if busy == 0 && frames == copied {
            state.restart_expected = true;
        }
        if restarting {
            state.restart_from = if busy == 0 { 0 } else { restart_again_at(end) };
        }
        Ok(())
    }

    /// Hands the commits a WAL left by a process that died holds past the
    /// observer's last position, or starts the observer where the file
    /// stands when it keeps nothing yet.
    /// A WAL that a long reader let grow is read a frame at a time, and each
    /// commit's frames once it is handed, so that recovery holds one commit
    /// rather than the whole WAL.
    fn recover(&self, writer: &Connection) -> Result<()> {
        let mut file = match fs::File::open(&self.wal) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io("read", error)),
        };
        let wal = match &mut file {
            Some(file) => WalCommits::scan(file)?,
            None => None,
        };
        let mut state = self.lock();
        match (self.observer.last(), wal) {
            (None, wal) => {
                let at = wal.as_ref().map_or_else(Position::default, |wal| wal.header.at(wal.end));
                let size = writer
                    .query_row("pragma page_count", [], |row| row.get(0))
                    .map_err(|error| sql_error("its pages", error))?;
                self.observer.begin(at, size)?;
                state.at = at;
                state.restart_expected = wal.is_none();
            }
            (Some(last), None) => {
                state.at = last;
                state.restart_expected = true;
            }
            (Some(last), Some(wal)) => {
                let from = if last.same_generation(&wal.header.at(0)) && last.frame <= wal.end {
                    last.frame + 1
                } else {
                    if !last.followed_by(&wal.header.at(0)) {
                        self.observer.gap(wal.header.at(0))?;
                    }
                    1
                };
                let file = file.as_mut().expect("a WAL that holds commits is open");
                for (start, end) in wal.commits_from(from) {
                    let _held = self.hold(&wal.header, start, end);
                    let frames = read_frames(file, &wal.header, start, end)?;
                    self.observer.committed(&commit_of(&wal.header, end, &frames)?)?;
                }
                state.at = wal.header.at(wal.end);
                state.restart_expected = false;
            }
        }
        self.observer.sync()
    }

    /// Holds the bytes of frames `from` to `end` of the store's memory while
    /// they are read; a commit already in its file is never refused for them.
    fn hold(&self, header: &Header, from: u32, end: u32) -> Option<crate::Reservation> {
        let bytes = u64::from(end.saturating_sub(from) + 1) * header.frame_size() as u64;
        self.memory.as_ref().map(|memory| memory.hold(bytes, HOLD_PATIENCE))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Hooks a writer no capture keeps, a copy's applier: each commit leaves
/// where the WAL ends for [`take_wal_end`], and SQLite's own checkpoints stop,
/// the writer's to run.
pub(crate) fn hook_wal(writer: &Connection) {
    writer.wal_hook(Some(remember_the_end));
}

/// Where the WAL ended after the commit this thread made last, once.
pub(crate) fn take_wal_end() -> Option<u32> {
    WAL_END.take()
}

/// The WAL beside `path` at `frame`: its generation as its header says it.
pub(crate) fn wal_at(path: &Path, frame: u32) -> Result<Position> {
    let wal = wal_of(path);
    let mut file = fs::File::open(&wal).map_err(|error| Error::io(wal.display().to_string(), error))?;
    Ok(read_header(&mut file)?.at(frame))
}

/// Where the WAL beside `path` stands: its generation and the frame its last
/// commit ends at; the default when it holds none.
pub(crate) fn wal_end(path: &Path) -> Result<Position> {
    Ok(commits_in(path)?.map_or_else(Position::default, |wal| wal.header.at(wal.end)))
}

/// Where each commit the WAL beside `path` holds past `mark` ends: those after
/// its frame in its generation, or every one of a later generation, which
/// began after `mark`.
pub(crate) fn commits_past(path: &Path, mark: Position) -> Result<Vec<Position>> {
    let Some(wal) = commits_in(path)? else {
        return Ok(Vec::new());
    };
    let after = if mark.same_generation(&wal.header.at(0)) { mark.frame } else { 0 };
    Ok(wal.commit_ends.iter().filter(|&&end| end > after).map(|&end| wal.header.at(end)).collect())
}

/// The commits the WAL beside `path` holds, as SQLite would recover them.
fn commits_in(path: &Path) -> Result<Option<WalCommits>> {
    let wal = wal_of(path);
    match fs::File::open(&wal) {
        Ok(mut file) => WalCommits::scan(&mut file).map_err(|error| error.within(wal.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(wal.display().to_string(), error)),
    }
}

/// The `-wal` file, opened the first time a commit reads it.
fn open_once<'a>(file: &'a mut Option<fs::File>, wal: &Path) -> Result<&'a mut fs::File> {
    if file.is_none() {
        *file = Some(fs::File::open(wal).map_err(|error| Error::io("open", error))?);
    }
    Ok(file.as_mut().expect("opened just above"))
}

/// The writer's WAL hook: it runs inside each commit, on the committing
/// thread, and only remembers where the WAL ends. Installing it is what
/// turns SQLite's own checkpoints off.
fn remember_the_end(_: &Wal, frames: c_int) -> rusqlite::Result<()> {
    WAL_END.set(u32::try_from(frames).ok());
    Ok(())
}

/// The WAL beside the file at `path`.
pub(crate) fn wal_of(path: &Path) -> PathBuf {
    let mut wal = OsString::from(path.as_os_str());
    wal.push("-wal");
    PathBuf::from(wal)
}

/// A WAL's header: the byte order of its checksums, its pages' size, its
/// generation and the checksum the first frame's starts from.
struct Header {
    big_endian: bool,
    page_size: u32,
    checkpoints: u32,
    salts: [u32; 2],
    checksum: [u32; 2],
}

impl Header {
    fn parse(bytes: &[u8]) -> Option<Header> {
        let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        if bytes.len() < HEADER || (word(0) & !1) != 0x377f_0682 {
            return None;
        }
        let header = Header {
            big_endian: word(0) & 1 == 1,
            page_size: word(8),
            checkpoints: word(12),
            salts: [word(16), word(20)],
            checksum: [word(24), word(28)],
        };
        let valid = header.page_size.is_power_of_two()
            && (512..=65_536).contains(&header.page_size)
            && checksum(header.big_endian, [0, 0], &bytes[..24]) == header.checksum;
        valid.then_some(header)
    }

    fn at(&self, frame: u32) -> Position {
        Position { checkpoints: self.checkpoints, salts: self.salts, frame }
    }

    fn frame_size(&self) -> usize {
        FRAME_HEADER + self.page_size as usize
    }
}

/// A frame's own fields: its page and, on the last frame of a commit, the
/// database's size in pages after it.
struct Frame<'a> {
    page: u32,
    size_after: u32,
    salts: [u32; 2],
    checksum: [u32; 2],
    data: &'a [u8],
}

impl<'a> Frame<'a> {
    fn parse(bytes: &'a [u8]) -> Frame<'a> {
        let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        Frame {
            page: word(0),
            size_after: word(4),
            salts: [word(8), word(12)],
            checksum: [word(16), word(20)],
            data: &bytes[FRAME_HEADER..],
        }
    }
}

/// The frames of a WAL that SQLite would recover: those whose salts are the
/// header's and whose checksums chain from it, up to the last that ends a
/// commit.
struct WalCommits {
    header: Header,
    /// The frame that ends each commit, in order.
    commit_ends: Vec<u32>,
    end: u32,
}

impl WalCommits {
    /// Reads `wal` a frame at a time, however long it grew: what it keeps is
    /// a frame's bytes and where each commit ends.
    fn scan(wal: &mut fs::File) -> Result<Option<WalCommits>> {
        let failed = |error| Error::io("read the WAL", error);
        wal.seek(SeekFrom::Start(0)).map_err(failed)?;
        let mut reader = io::BufReader::new(wal);
        let mut head = [0; HEADER];
        match reader.read_exact(&mut head) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(failed(error)),
        }
        let Some(header) = Header::parse(&head) else { return Ok(None) };
        let (mut sum, mut commit_ends, mut index) = (header.checksum, Vec::new(), 0u32);
        let mut frame = vec![0; header.frame_size()];
        loop {
            match reader.read_exact(&mut frame) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(failed(error)),
            }
            index += 1;
            let parsed = Frame::parse(&frame);
            sum = checksum(header.big_endian, sum, &frame[..8]);
            sum = checksum(header.big_endian, sum, parsed.data);
            if parsed.salts != header.salts || parsed.checksum != sum {
                break;
            }
            if parsed.size_after != 0 {
                commit_ends.push(index);
            }
        }
        let Some(&end) = commit_ends.last() else { return Ok(None) };
        Ok(Some(WalCommits { header, commit_ends, end }))
    }

    /// Each commit at or past `from`, as its first and last frames.
    fn commits_from(&self, from: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        let starts = std::iter::once(1).chain(self.commit_ends.iter().map(|end| end + 1));
        starts.zip(self.commit_ends.iter().copied()).filter(move |&(start, _)| start >= from)
    }
}

fn frame_offset(header: &Header, frame: u32) -> usize {
    HEADER + (frame as usize - 1) * header.frame_size()
}

/// SQLite's WAL checksum: two sums over the bytes as 32-bit words, each
/// pair of words adding into both.
fn checksum(big_endian: bool, [mut first, mut second]: [u32; 2], bytes: &[u8]) -> [u32; 2] {
    let word = |bytes: [u8; 4]| if big_endian { u32::from_be_bytes(bytes) } else { u32::from_le_bytes(bytes) };
    for &[a, b, c, d, e, f, g, h] in bytes.as_chunks::<8>().0 {
        first = first.wrapping_add(word([a, b, c, d])).wrapping_add(second);
        second = second.wrapping_add(word([e, f, g, h])).wrapping_add(first);
    }
    [first, second]
}

fn read_header(wal: &mut fs::File) -> Result<Header> {
    let mut bytes = [0; HEADER];
    wal.seek(SeekFrom::Start(0)).and_then(|_| wal.read_exact(&mut bytes)).map_err(|error| Error::io("read", error))?;
    Header::parse(&bytes).ok_or_else(|| Error::new(ErrorKind::Corrupt, "a WAL header that does not read"))
}

fn read_frames(wal: &mut fs::File, header: &Header, from: u32, end: u32) -> Result<Vec<u8>> {
    let mut frames = vec![0; (end - from + 1) as usize * header.frame_size()];
    wal.seek(SeekFrom::Start(frame_offset(header, from) as u64))
        .and_then(|_| wal.read_exact(&mut frames))
        .map_err(|error| Error::io("read", error))?;
    Ok(frames)
}

/// The commit `frames` hold, ending at frame `end`: each page as its last
/// frame left it.
fn commit_of<'a>(header: &Header, end: u32, frames: &'a [u8]) -> Result<Commit<'a>> {
    let mut pages = BTreeMap::new();
    let mut size = 0;
    for frame in frames.chunks_exact(header.frame_size()).map(Frame::parse) {
        if frame.salts != header.salts {
            return Err(Error::new(ErrorKind::Corrupt, "a commit's frame is of another generation of the WAL"));
        }
        pages.insert(frame.page, frame.data);
        size = frame.size_after;
    }
    if size == 0 {
        return Err(Error::new(ErrorKind::Corrupt, "a commit's last frame does not end a commit"));
    }
    Ok(Commit { position: header.at(end), page_size: header.page_size, size, pages: pages.into_iter().collect() })
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
