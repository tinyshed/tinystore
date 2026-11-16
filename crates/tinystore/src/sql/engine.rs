use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::tx::{BOUND, HELD};
use crate::engine::{Claim, Engine, Host, SharedFile};
use crate::sqlite::{Config, Drafts, File, Migration};
use crate::sync::lock;
use crate::{Clock, Durability, Error, Memory, Result, Store};

/// The history a database's own migrations are kept under.
const HISTORY: &str = "migrations";

use crate::engine::database_file as file_of;
use crate::engine::{DATABASES, LOCAL};

/// Statements known to write that a database remembers; past them it forgets
/// them all and asks SQLite again.
const WRITES_KEPT: usize = 1024;

/// How often each database closes the readers it has left unused for a minute,
/// as kv and jobs do in their maintenance; a burst of reads otherwise kept
/// every reader it opened, and its memory, for good.
const SWEEP: Duration = Duration::from_secs(60);

/// The store's application databases, by name: one file and one writer each,
/// opened by the first handle and closed with the store.
pub(crate) struct Databases {
    open: Mutex<HashMap<String, Arc<Base>>>,
    /// Those connections that only read opened while no open that writes
    /// held them, each read through a file of its own that claims nothing and
    /// writes nothing: the program's own open still chooses its durability.
    read: Mutex<HashMap<String, Arc<Base>>>,
}

/// What an open of a database asks: its name, whether backups take it, the
/// migrations it must have applied and how far its commits go.
pub(crate) struct Opening<'a> {
    pub(crate) name: &'a str,
    pub(crate) backup: bool,
    pub(crate) migrations: Option<(Drafts, Vec<Migration>)>,
    pub(crate) durability: Option<Durability>,
}

/// One application database: `sql/<name>.db`, or `local/<name>.db` for one
/// no backup takes, its writer and its readers, and the buckets and queues
/// kept in its file.
pub(crate) struct Base {
    pub(crate) name: String,
    /// Whether backups take it: its file lies in `sql/`, and in `local/` when not.
    pub(crate) backup: bool,
    pub(crate) shared: Arc<SharedFile>,
    clock: Arc<dyn Clock>,
    /// The store's memory, which a call's rows hold while it holds them.
    pub(crate) memory: Arc<Memory>,
    /// Statements known to write, by their text. SQLite says whether one
    /// writes when it compiles it, and a read call sends those to the writer.
    writes: Mutex<HashSet<String>>,
    durability: Durability,
    /// Held by the database that writes its file; one read through a file
    /// of its own holds none.
    _claim: Option<Claim>,
}

impl Databases {
    /// The store's databases, opened with the first, their idle readers swept
    /// on the store's background thread.
    pub(crate) fn of(store: &Store) -> Result<Arc<Databases>> {
        store.engine(|store| {
            let databases = Arc::new(Databases { open: Mutex::new(HashMap::new()), read: Mutex::new(HashMap::new()) });
            let weak = Arc::downgrade(&databases);
            store.every("sql: idle readers", SWEEP, move || {
                if let Some(databases) = weak.upgrade() {
                    databases.sweep();
                }
                Ok(())
            })?;
            Ok(databases)
        })
    }

    /// Closes each database's readers unused for long enough.
    fn sweep(&self) {
        let mut open: Vec<Arc<Base>> = lock(&self.open).values().cloned().collect();
        open.extend(lock(&self.read).values().cloned());
        for base in open {
            base.file().sweep();
        }
    }

    /// The database `name`, opened when it is not, after `migrations` are
    /// applied and checked; one already open applies what it lacks of them.
    /// Its commits go as far as `durability` says, or the store's options
    /// where it says nothing; a file has one, so one already open refuses
    /// another. Its file lies where `backup` says, and a name whose file lies
    /// in the other folder, open or not, is refused.
    pub(crate) fn open(&self, store: &Store, asked: Opening<'_>) -> Result<Arc<Base>> {
        let Opening { name, backup, migrations, durability } = asked;
        let mut open = lock(&self.open);
        if let Some(base) = open.get(name) {
            let base = Arc::clone(base);
            drop(open);
            if base.backup != backup {
                return Err(Error::invalid(lies_elsewhere(name, backup)));
            }
            if let Some(asked) = durability.filter(|asked| *asked != base.durability) {
                let opened = base.durability;
                return Err(Error::invalid(format!("open with durability {opened}, and asked for {asked}")));
            }
            if let Some((drafts, migrations)) = migrations {
                base.file().migrate_checked(HISTORY, &migrations, Some(&drafts))?;
            }
            return Ok(base);
        }
        refuse_the_other_folder(store, name, backup)?;
        let claim = store.claim(&file_in(name, backup))?;
        let config = config(durability.or(store.durability()));
        let durability = config.durability;
        let file = store.open_file(claim.name(), config)?;
        if let Some((drafts, migrations)) = migrations {
            file.migrate_checked(HISTORY, &migrations, Some(&drafts))?;
        }
        let base = Arc::new(Base { _claim: Some(claim), backup, ..Base::opened(store, name, file, durability) });
        open.insert(name.to_owned(), Arc::clone(&base));
        Ok(base)
    }

    /// The database `name` as the store holds it, for a connection that may
    /// only read: the one an open that writes holds, or else one read through
    /// a file of its own, as a store beside its holder reads it, which claims
    /// nothing, binds no durability and applies no migration, so that the
    /// program's own open after it chooses them. A file the store does not
    /// hold is `NotFound` rather than made.
    #[cfg(feature = "wire")]
    pub(crate) fn open_as_it_is(&self, store: &Store, name: &str, backup: bool) -> Result<Arc<Base>> {
        let found = |base: &Arc<Base>| match base.backup == backup {
            true => Ok(Arc::clone(base)),
            false => Err(Error::invalid(lies_elsewhere(name, backup))),
        };
        if let Some(base) = lock(&self.open).get(name) {
            return found(base);
        }
        let mut read = lock(&self.read);
        if let Some(base) = read.get(name) {
            return found(base);
        }
        refuse_the_other_folder(store, name, backup)?;
        let config = config(store.durability());
        let durability = config.durability;
        let file = store.open_to_read(&file_in(name, backup), config)?;
        let base = Arc::new(Base { backup, ..Base::opened(store, name, file, durability) });
        read.insert(name.to_owned(), Arc::clone(&base));
        Ok(base)
    }
}

impl Engine for Databases {
    /// Closes every database; the first that fails is the error, and the
    /// others close either way.
    fn close(&self) -> Result<()> {
        let mut open: Vec<Arc<Base>> = lock(&self.open).drain().map(|(_, base)| base).collect();
        open.extend(lock(&self.read).drain().map(|(_, base)| base));
        let mut first_failure = None;
        for base in open {
            if let Err(error) = base.file().close() {
                first_failure.get_or_insert(error.within(base.describe()));
            }
        }
        first_failure.map_or(Ok(()), Err)
    }

    /// Names each database open that backups take, the buckets and queues it
    /// keeps with it; one in `local/` is left out, file and all.
    fn snapshot(&self) -> Result<crate::engine::Copying<'_>> {
        let open: Vec<Arc<Base>> = lock(&self.open).values().filter(|base| base.backup).cloned().collect();
        let mut copying = crate::engine::Copying::default();
        for base in &open {
            copying.files.extend(crate::engine::to_copy(base.file(), &file_of(&base.name), "sql")?.files);
        }
        Ok(copying)
    }
}

impl Base {
    /// The database `name` in `file`, its commits going as far as
    /// `durability` says, holding no claim yet.
    fn opened(store: &Store, name: &str, file: File, durability: Durability) -> Base {
        Base {
            name: name.to_owned(),
            backup: true,
            shared: Arc::new(SharedFile::new(file, format!("sql {name}"), store)),
            clock: store.clock(),
            memory: Arc::clone(store.memory()),
            writes: Mutex::new(HashSet::new()),
            durability,
            _claim: None,
        }
    }

    pub(crate) fn file(&self) -> &File {
        self.shared.file()
    }

    pub(crate) fn now(&self) -> SystemTime {
        self.clock.now()
    }

    /// Fails once a transaction that started at `started` has held the writer
    /// past its bound, which then rolls back.
    pub(crate) fn within_bound(&self, started: SystemTime) -> Result<()> {
        let held = self.now().duration_since(started).unwrap_or_default();
        if held <= BOUND {
            return Ok(());
        }
        let refused =
            format!("{}: a transaction held the writer {held:?}, past its {BOUND:?}, and rolls back", self.describe());
        Err(HELD.refuse(held.as_millis() as u64, refused))
    }

    pub(crate) fn writes(&self, text: &str) -> bool {
        lock(&self.writes).contains(text)
    }

    pub(crate) fn remember_write(&self, text: &str) {
        let mut writes = lock(&self.writes);
        if writes.len() >= WRITES_KEPT {
            writes.clear();
        }
        writes.insert(text.to_owned());
    }

    pub(crate) fn describe(&self) -> String {
        described(&self.name)
    }

    /// An error of `statement`, named by it and by the database.
    pub(super) fn failed(&self, statement: &super::statement::Sql, error: Error) -> Error {
        error.within(statement.describe()).within(self.describe())
    }
}

/// The file a database's name keeps, by its path in the store's directory:
/// `sql/<name>.db`, or `local/<name>.db` for one no backup takes.
pub(crate) fn file_in(name: &str, backup: bool) -> String {
    if backup { file_of(name) } else { format!("{LOCAL}/{name}.db") }
}

/// The folder a database's file lies in: `sql`, or `local` for one no backup
/// takes.
pub(crate) fn folder_of(backup: bool) -> &'static str {
    if backup { DATABASES } else { LOCAL }
}

/// Refuses a database whose file lies in the other folder than the one
/// `backup` asks: a name lives in one folder, and nothing moves it, so a
/// program that changed its mind is told rather than given a second, empty
/// database of the same name. The folder's listing tells, not the file's
/// name, since Windows answers for a device by its name alone.
fn refuse_the_other_folder(store: &Store, name: &str, backup: bool) -> Result<()> {
    match super::database::holds_file(store.dir(), folder_of(!backup), name) {
        true => Err(Error::invalid(lies_elsewhere(name, backup))),
        false => Ok(()),
    }
}

/// Why an open asked another folder than the database's own, naming both:
///
/// ```text
/// local/uploads.db, left out of backups, asked where the database lies in sql/uploads.db, which backups take
/// ```
fn lies_elsewhere(name: &str, backup: bool) -> String {
    let says = |backup: bool| if backup { "which backups take" } else { "left out of backups" };
    let (asked, held) = (file_in(name, backup), file_in(name, !backup));
    format!(
        "{asked}, {}, asked where the database lies in {held}, {}: a name lives in one folder, and nothing moves it",
        says(backup),
        says(!backup)
    )
}

/// The database `name` as an error names it: `sql app`.
pub(super) fn described(name: &str) -> String {
    format!("sql {name}")
}

/// A database's connections: every file's, their cache of prepared statements
/// 128 deep, with room for an application's.
fn config(durability: Option<Durability>) -> Config {
    Config { statements: 128, ..Config::committing(durability) }
}
