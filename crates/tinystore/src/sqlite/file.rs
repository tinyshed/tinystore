use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use rusqlite::Connection;

use super::capture::{Capture, Logged, Observe, in_the_file, logged, own_work};
use super::connection::{self, Role, execute, sql_error};
use super::given::own;
use super::group::{BEGIN, COMMIT, Done, Group, ROLLBACK, Work};
use super::migrate::{self, Migration};
use super::readers::Readers;
use super::{Committed, Config, Left, Settle, Settlers, Tx};
use crate::{Error, ErrorKind, Result};

const FOREIGN_KEYS_OFF: &str = own!("pragma foreign_keys = off");
const FOREIGN_KEYS_ON: &str = own!("pragma foreign_keys = on");

thread_local! {
    /// The files whose writer this thread holds in a transaction, by address.
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// One SQLite file: one writer whose commits are grouped, and a pool of
/// readers. Every engine's file is one of these.
pub(crate) struct File {
    path: PathBuf,
    /// Shared with the file's log, which holds it to read where the file's
    /// commits stand while none is under way.
    writer: Arc<Mutex<Connection>>,
    group: Group,
    readers: Arc<Readers>,
    closed: AtomicBool,
    settlers: Arc<Settlers>,
    capture: Logged,
    /// The file is a copy its home writes: what would write here runs on a
    /// reader, whose `query_only` refuses a write.
    copy: Arc<AtomicBool>,
    config: Arc<Config>,
}

/// What a node holds of an engine's file to change its mode as the file's
/// home moves, weakly, so that the file closes with its engine.
#[cfg(feature = "replication")]
pub(crate) struct Switch {
    path: PathBuf,
    config: Arc<Config>,
    writer: Weak<Mutex<Connection>>,
    capture: Weak<std::sync::RwLock<Option<Arc<Capture>>>>,
    copy: Weak<AtomicBool>,
    readers: Weak<Readers>,
}

impl File {
    pub(crate) fn open(path: impl Into<PathBuf>, config: Config) -> Result<Self> {
        Self::open_observed(path, config, None)
    }

    /// Opens a copy of a file another node writes: its engines read it, a
    /// migration only checks that its home applied the steps, and a write is
    /// refused as `Invalid`.
    ///
    /// Its writer, which nothing of a copy writes through, is in memory: the
    /// copy's file is then open only through its readers, which a snapshot
    /// taken again closes while it takes the file's place.
    pub(crate) fn open_copy(path: impl Into<PathBuf>, config: Config) -> Result<Self> {
        // a copy's applier checkpoints its WAL, once its position is synced
        let file = Self::open_observed(path, Config { keep_wal: true, ..config }, None)?;
        let writer = no_writer(&file.describe())?;
        Ok(Self { copy: Arc::new(AtomicBool::new(true)), writer: Arc::new(Mutex::new(writer)), ..file })
    }

    /// What a node holds of the file to change its mode, and to pause its
    /// readers, as its home moves.
    #[cfg(feature = "replication")]
    pub(crate) fn switch(&self) -> Switch {
        Switch {
            path: self.path.clone(),
            config: Arc::clone(&self.config),
            writer: Arc::downgrade(&self.writer),
            capture: Arc::downgrade(&self.capture),
            copy: Arc::downgrade(&self.copy),
            readers: Arc::downgrade(&self.readers),
        }
    }

    #[cfg(all(test, feature = "replication"))]
    pub(crate) fn become_home(&self, observer: Arc<dyn Observe>) -> Result<()> {
        self.switch().become_home(observer)
    }

    #[cfg(all(test, feature = "replication"))]
    pub(crate) fn become_copy(&self) -> Result<()> {
        self.switch().become_copy()
    }

    pub(crate) fn is_copy(&self) -> bool {
        self.copy.load(Ordering::SeqCst)
    }

    /// Opens the file with each commit handed to `observer`, which is first
    /// handed what the WAL holds past the observer's last position.
    pub(crate) fn open_observed(
        path: impl Into<PathBuf>,
        config: Config,
        observer: Option<Arc<dyn Observe>>,
    ) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Error::io(path.display().to_string(), error))?;
        }
        let config = Arc::new(Config { keep_wal: config.keep_wal || observer.is_some(), ..config });
        let writer = connection::open(&path, &config, Role::Writer)?;
        let capture = observer
            .as_ref()
            .map(|observer| Capture::open(&path, &writer, Arc::clone(observer)).map(Arc::new))
            .transpose()?;
        let writer = Arc::new(Mutex::new(writer));
        if let Some(observer) = &observer {
            observer.writer(Arc::downgrade(&writer));
        }
        let settlers = Arc::new(Settlers::default());
        let capture: Logged = Arc::new(std::sync::RwLock::new(capture));
        Ok(Self {
            group: Group::new(config.group, Arc::clone(&settlers), Arc::clone(&capture)),
            readers: Arc::new(Readers::new(path.clone(), Arc::clone(&config))),
            writer,
            path,
            closed: AtomicBool::new(false),
            settlers,
            capture,
            copy: Arc::new(AtomicBool::new(false)),
            config,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Has every transaction of the file settle `settle` before it commits,
    /// from the next one on, until the engine is dropped.
    pub(crate) fn settle_with(&self, settle: Weak<dyn Settle>) {
        self.settlers.add(settle);
    }

    /// Runs `write` in a commit it may share with the writes queued beside it,
    /// and hands back its value once that commit is as durable as the file's
    /// configuration says.
    ///
    /// `write` runs on whichever caller leads the commit, so it owns what it
    /// writes; it is built before it waits, and no application code runs while
    /// the group does. `bytes` is what it adds to the commit.
    pub(crate) fn write<T, F>(&self, bytes: usize, write: F) -> Result<T>
    where
        F: FnOnce(&Tx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.refuse_when_closed("a write")?;
        self.refuse_inside_a_transaction("a write", "would wait for the writer the transaction holds")?;
        if self.is_copy() {
            return self.as_a_copy(write);
        }
        let slot = Arc::new(Mutex::new(None));
        let filled = Arc::clone(&slot);
        let work: Work = Box::new(move |tx| {
            let value = write(tx)?;
            *lock(&filled) = Some(value);
            Ok(())
        });
        self.group.write(&self.writer, bytes, work).map_err(|error| error.within(self.describe()))?;
        lock(&slot)
            .take()
            .ok_or_else(|| Error::internal(format!("{}: a write committed without its value", self.describe())))
    }

    /// `write` without waiting: it queues the write and returns, and `done` is
    /// called with the value, or the error, on the thread that commits it. For
    /// hosts whose one thread holds many writes in flight; `done` must not
    /// block, since the commits after it wait for it.
    pub(crate) fn submit<T, F, D>(&self, bytes: usize, write: F, done: D)
    where
        F: FnOnce(&Tx<'_>) -> Result<T> + Send + 'static,
        T: Send + 'static,
        D: FnOnce(Result<T>) + Send + 'static,
    {
        let refused = self.refuse_when_closed("a write").and_then(|()| {
            self.refuse_inside_a_transaction("a write", "would wait for the writer the transaction holds")
        });
        if let Err(error) = refused {
            return done(Err(error));
        }
        if self.is_copy() {
            return done(self.as_a_copy(write));
        }
        let slot = Arc::new(Mutex::new(None));
        let filled = Arc::clone(&slot);
        let work: Work = Box::new(move |tx| {
            let value = write(tx)?;
            *lock(&filled) = Some(value);
            Ok(())
        });
        let describe = self.describe();
        let answered: Done = Box::new(move |answer| {
            let value = answer.map_err(|error| error.within(&describe)).and_then(|()| {
                let missing = || Error::internal(format!("{describe}: a write committed without its value"));
                lock(&slot).take().ok_or_else(missing)
            });
            done(value);
        });
        self.group.submit(&self.writer, bytes, work, answered);
    }

    /// Runs `work` in a transaction that holds the writer alone, for reads that
    /// decide what to write. It commits when `work` succeeds and rolls back
    /// when it fails or panics, and it pays its own sync.
    ///
    /// A write to this file from inside `work`, other than through its `Tx`,
    /// would wait for the writer `work` holds: it fails `Invalid` instead.
    pub(crate) fn transaction<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        self.refuse_when_closed("a transaction")?;
        self.refuse_inside_a_transaction("a transaction", "would wait for the writer the transaction holds")?;
        if self.is_copy() {
            return self.as_a_copy(work);
        }
        let connection = lock(&self.writer);
        self.transaction_on(&connection, work)
    }

    /// Runs what would write the file, on a copy, in one snapshot of a reader:
    /// what only reads, as finding a name already made, answers, and what
    /// writes fails, its home's to do.
    fn as_a_copy<T, E: From<Error>>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E> {
        let mut outcome = None;
        self.readers
            .read(|connection| {
                let (committed, settled_by_none) = (Committed::default(), Settlers::default());
                outcome = Some(work(&Tx::new(connection, &committed, &settled_by_none)));
                Ok(())
            })
            .map_err(|error| error.within(self.describe()))?;
        outcome.expect("the read ran")
    }

    fn transaction_on<T, E: From<Error>>(
        &self,
        connection: &Connection,
        work: impl FnOnce(&Tx<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.refuse_when_unlogged()?;
        execute(connection, BEGIN).map_err(|error| sql_error(format!("{}: a transaction", self.describe()), error))?;
        let _held = Held::enter(self.address());
        let mut open = Open { connection, finished: false };
        let committed = Committed::default();
        let tx = Tx::new(connection, &committed, &self.settlers);
        tx.settle(Left::Before)?;
        let value = work(&tx)?;
        tx.settle(Left::Here)?;
        open.finished = true;
        let handed = self.commit(connection);
        if in_the_file(&handed) {
            committed.run();
        }
        handed.map(|()| value).map_err(E::from)
    }

    /// Commits the transaction on `connection`; a commit that fails may or may
    /// not be in the file.
    fn commit(&self, connection: &Connection) -> Result<()> {
        let capture = logged(&self.capture);
        if let Some(capture) = &capture {
            capture.arm(!super::capture::is_own_work());
        }
        execute(connection, COMMIT).map_err(|error| {
            if !connection.is_autocommit() {
                let _ = execute(connection, ROLLBACK);
            }
            Error::new(ErrorKind::OutcomeUnknown, format!("{}: a transaction's commit", self.describe()))
                .with_source(error)
        })?;
        match &capture {
            Some(capture) => capture.committed(connection).map_err(|error| error.within(self.describe())),
            None => Ok(()),
        }
    }

    /// Refuses a transaction once a commit of the file could not be logged.
    fn refuse_when_unlogged(&self) -> Result<()> {
        match logged(&self.capture) {
            Some(capture) => capture.refuse_a_write().map_err(|error| error.within(self.describe())),
            None => Ok(()),
        }
    }

    /// Marks this thread as inside a transaction of the file that has not
    /// begun yet, as the store's is until it takes its first handle in: a call
    /// on the file around the transaction is refused from here on.
    pub(crate) fn mark(&self) -> Result<Mark> {
        self.refuse_when_closed("a transaction")?;
        self.refuse_inside_a_transaction("a transaction", "would wait for the writer the transaction holds")?;
        Ok(Mark(Held::enter(self.address())))
    }

    /// Begins the transaction `mark` announced, which holds the writer alone
    /// until it commits, and rolls back when it is dropped before.
    pub(crate) fn begin(&self, _mark: &Mark) -> Result<Begun<'_>> {
        self.refuse_when_closed("a transaction")?;
        if self.is_copy() {
            return Err(a_copy_takes_no_transaction(&self.describe()));
        }
        let connection = lock(&self.writer);
        self.refuse_when_unlogged()?;
        execute(&connection, BEGIN).map_err(|error| sql_error(format!("{}: a transaction", self.describe()), error))?;
        let begun = Begun { file: self, connection, hooks: Committed::default(), committed: false };
        begun.tx().settle(Left::Before)?;
        Ok(begun)
    }

    /// Whether this thread holds the file's writer in a transaction.
    pub(crate) fn held_here(&self) -> bool {
        HELD.with(|held| held.borrow().contains(&self.address()))
    }

    /// Runs `read` on a reader, in one snapshot of the file. A read from inside
    /// a transaction of the file would not see what the transaction wrote: it
    /// fails `Invalid` instead.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.refuse_when_closed("a read")?;
        self.refuse_inside_a_transaction("a read", "would not see what the transaction wrote")?;
        self.readers.read(read)
    }

    /// Copies the file into `target` as one moment of it, its writer and
    /// readers going on meanwhile.
    pub(crate) fn copy_into(&self, target: &Path) -> Result<()> {
        self.refuse_when_closed("a copy")?;
        connection::copy(&self.path, target)
    }

    pub(crate) fn migrate(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        if self.is_copy() {
            return self.check_its_home_migrated(history, migrations);
        }
        if self.migrated(history, migrations)? {
            return Ok(());
        }
        own_work(|| self.transaction(|tx| migrate::apply(tx, history, migrations)))
            .map_err(|error| error.within(self.describe()))
    }

    /// Whether the file holds every step of `history` already: a reader then
    /// answers, and no writer is taken, so that a file which takes no writes,
    /// as an old home's, still opens.
    fn migrated(&self, history: &str, migrations: &[Migration]) -> Result<bool> {
        match self.read(|connection| migrate::check_applied_by_home(connection, history, migrations)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::Unavailable => Ok(false),
            Err(error) => Err(error.within(self.describe())),
        }
    }

    /// A copy's migrations are its home's: they arrive as pages, and the copy
    /// only checks that its home applied these steps.
    fn check_its_home_migrated(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        self.read(|connection| migrate::check_applied_by_home(connection, history, migrations))
            .map_err(|error| error.within(self.describe()))
    }

    /// Applies migrations as `migrate` does, with foreign keys off while they
    /// run and checked before the commit, as SQLite's procedure for changing a
    /// table asks: with them on, rebuilding a parent table deletes its
    /// children through `on delete cascade`, and nothing says so.
    pub(crate) fn migrate_checked(&self, history: &str, migrations: &[Migration]) -> Result<()> {
        self.refuse_when_closed("a migration")?;
        self.refuse_inside_a_transaction("a migration", "would wait for the writer the transaction holds")?;
        if self.is_copy() {
            return self.check_its_home_migrated(history, migrations);
        }
        if self.migrated(history, migrations)? {
            return Ok(());
        }
        let connection = lock(&self.writer);
        let what = || format!("{}: {history}: foreign keys", self.describe());
        // foreign_keys cannot change inside a transaction, so it changes around one.
        execute(&connection, FOREIGN_KEYS_OFF).map_err(|error| sql_error(what(), error))?;
        let applied = own_work(|| {
            self.transaction_on(&connection, |tx| {
                migrate::apply(tx, history, migrations)?;
                migrate::check_foreign_keys(tx, history)
            })
        });
        let restored = execute(&connection, FOREIGN_KEYS_ON).map_err(|error| sql_error(what(), error));
        applied.map_err(|error| error.within(self.describe())).and(restored)
    }

    /// Closes the readers left unused for too long; the store runs it now and
    /// then.
    pub(crate) fn sweep(&self) -> usize {
        let closed = self.readers.sweep();
        if closed > 0 {
            super::memory::give_back();
        }
        closed
    }

    pub(crate) fn readers_open(&self) -> usize {
        self.readers.open()
    }

    #[cfg(test)]
    pub(crate) fn readers_waiting(&self) -> usize {
        self.readers.waiting()
    }

    pub(crate) fn commits(&self) -> u64 {
        self.group.commits()
    }

    /// Refuses new work, answers the writes still queued, closes the readers
    /// and truncates the WAL. A commit under way finishes first.
    pub(crate) fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.group.close();
        self.readers.close();
        let connection = lock(&self.writer);
        if let Some(capture) = logged(&self.capture) {
            capture.close().map_err(|error| error.within(format!("{}: its log", self.describe())))?;
        }
        // A truncated WAL leaves the directory as small as its data.
        connection
            .query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))
            .map_err(|error| sql_error(format!("{}: its last checkpoint", self.describe()), error))
    }

    fn refuse_when_closed(&self, what: &str) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::closed(format!("{}: {what}", self.describe())));
        }
        Ok(())
    }

    fn refuse_inside_a_transaction(&self, what: &str, why: &str) -> Result<()> {
        if !self.held_here() {
            return Ok(());
        }
        Err(Error::invalid(format!(
            "{}: {what} from inside a transaction of the file {why}: make it through the transaction",
            self.describe()
        )))
    }

    fn address(&self) -> usize {
        self as *const File as usize
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// This thread inside a transaction of a file, from before it begins.
pub(crate) struct Mark(
    #[cfg_attr(
        any(feature = "kv", feature = "jobs"),
        expect(dead_code, reason = "held for its drop, which lets the thread go")
    )]
    Held,
);

#[cfg(feature = "replication")]
impl Switch {
    /// Whether the file is still open.
    pub(crate) fn is_open(&self) -> bool {
        self.copy.strong_count() > 0
    }

    pub(crate) fn readers(&self) -> Option<Arc<Readers>> {
        self.readers.upgrade()
    }

    /// Makes a copy this node's own while it is open: a writer on the file,
    /// each commit handed to `observer`, the file's log, from now on. The copy's
    /// applier and readers are closed before, and what the applier left in the
    /// WAL is checkpointed first, so that the log takes no frame of it for a
    /// commit of its own.
    pub(crate) fn become_home(&self, observer: Arc<dyn Observe>) -> Result<()> {
        let (Some(held), Some(capture_cell), Some(copy)) =
            (self.writer.upgrade(), self.capture.upgrade(), self.copy.upgrade())
        else {
            return Err(Error::closed(format!("{}: its change of home", self.path.display())));
        };
        let describe = self.path.display().to_string();
        let config = Config { keep_wal: true, ..(*self.config).clone() };
        let writer = connection::open(&self.path, &config, Role::Writer)?;
        writer
            .query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))
            .map_err(|error| sql_error(format!("{describe}: the copy's last checkpoint"), error))?;
        let capture = Capture::open(&self.path, &writer, Arc::clone(&observer))?;
        let mut writing = lock(&held);
        *writing = writer;
        observer.writer(Arc::downgrade(&held));
        *capture_cell.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(capture));
        copy.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Makes this node's file a copy its new home writes, while it is open:
    /// what its log holds made durable, its WAL checkpointed, its writer
    /// closed. Its writes were refused before, so that none waits for a writer
    /// that is gone; one that still reached the group fails on the writer left.
    pub(crate) fn become_copy(&self) -> Result<()> {
        let (Some(held), Some(capture_cell), Some(copy)) =
            (self.writer.upgrade(), self.capture.upgrade(), self.copy.upgrade())
        else {
            return Err(Error::closed(format!("{}: its change of home", self.path.display())));
        };
        let describe = self.path.display().to_string();
        let mut writing = lock(&held);
        // The capture leaves its cell only once closed: one that fails to close
        // keeps the file a home whose commits are logged.
        if let Some(capture) = logged(&capture_cell) {
            capture.close().map_err(|error| error.within(format!("{describe}: its log")))?;
        }
        capture_cell.write().unwrap_or_else(PoisonError::into_inner).take();
        writing
            .query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))
            .map_err(|error| sql_error(format!("{describe}: its last checkpoint as a home"), error))?;
        copy.store(true, Ordering::SeqCst);
        *writing = no_writer(&describe)?;
        Ok(())
    }
}

/// The writer of a copy, which nothing of a copy writes through: a database
/// in memory that refuses a write, so that one which reached the group as the
/// file became a copy fails rather than vanish.
fn no_writer(describe: &str) -> Result<Connection> {
    let writer = Connection::open_in_memory().map_err(|error| sql_error(describe, error))?;
    writer.pragma_update(None, "query_only", true).map_err(|error| sql_error(describe, error))?;
    Ok(writer)
}

/// A transaction begun on a file by [`File::begin`].
pub(crate) struct Begun<'f> {
    file: &'f File,
    connection: MutexGuard<'f, Connection>,
    hooks: Committed,
    committed: bool,
}

impl Begun<'_> {
    pub(crate) fn file(&self) -> &File {
        self.file
    }

    /// The writer inside the transaction.
    pub(crate) fn tx(&self) -> Tx<'_> {
        Tx::new(&self.connection, &self.hooks, &self.file.settlers)
    }

    /// Settles what the calls left, commits, then runs what they asked to
    /// run once it did, the writer still held.
    pub(crate) fn commit(mut self) -> Result<()> {
        self.tx().settle(Left::Here)?;
        self.committed = true;
        let handed = self.file.commit(&self.connection);
        if in_the_file(&handed) {
            self.hooks.run();
        }
        handed
    }
}

impl Drop for Begun<'_> {
    fn drop(&mut self) {
        if !self.committed && !self.connection.is_autocommit() {
            let _ = execute(&self.connection, ROLLBACK);
        }
    }
}

/// Why a transaction of a copy is refused: its code reads and decides on the
/// writer, which only the file's home holds, and code is not sent where a
/// write passes on.
pub(crate) fn a_copy_takes_no_transaction(owner: &str) -> Error {
    Error::new(
        ErrorKind::Unavailable,
        format!("{owner}: a transaction holds its file's writer, which a copy has not: run it at the file's home"),
    )
}

/// Whether this thread holds any file's writer in a transaction, or is marked
/// inside one that has not begun yet: what waits there holds a writer too.
pub(crate) fn in_a_transaction() -> bool {
    HELD.with(|held| !held.borrow().is_empty())
}

/// A file's writer this thread holds, from its transaction's start to its end.
struct Held(usize);

impl Held {
    fn enter(file: usize) -> Held {
        HELD.with(|held| held.borrow_mut().push(file));
        Held(file)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(at) = held.iter().rposition(|&file| file == self.0) {
                held.remove(at);
            }
        });
    }
}

/// A transaction of its own, rolled back unless it finished.
struct Open<'a> {
    connection: &'a Connection,
    finished: bool,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        if !self.finished && !self.connection.is_autocommit() {
            let _ = execute(self.connection, ROLLBACK);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
