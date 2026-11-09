//! A database's drafts, the migrations it has not shipped: the copy of its
//! file kept from before the first of them, and the file taken back to it.
//!
//! Each runs on a connection of its own while the file's writer is held and
//! in no transaction: a vacuum attaches the file it writes, which no
//! connection of the store's may, and the file's connections read what the
//! copy wrote at their next transaction, as after [`super::vacuum`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use super::connection::sql_error;
use crate::{Error, Result};

/// Where a database keeps the copy from before its drafts, whether to take
/// it back there first, and the command a person runs for that, which the
/// refusal of an edited draft names.
#[derive(Debug)]
pub(crate) struct Drafts {
    pub(crate) copy: PathBuf,
    pub(crate) redo: bool,
    pub(crate) command: String,
}

const FLAGS: OpenFlags =
    OpenFlags::SQLITE_OPEN_READ_WRITE.union(OpenFlags::SQLITE_OPEN_NO_MUTEX).union(OpenFlags::SQLITE_OPEN_EXRESCODE);

/// Writes the file at `path` as it stands to `copy`, one in its place
/// removed first, since `vacuum into` writes only a file that is not there.
pub(super) fn keep(path: &Path, copy: &Path, busy_timeout: Duration) -> Result<()> {
    let what = || format!("copy {} to {} before its drafts", path.display(), copy.display());
    remove(copy).map_err(|error| error.within(what()))?;
    let into = copy.to_str().ok_or_else(|| Error::invalid(format!("{}: a path that is no UTF-8", copy.display())))?;
    let connection = own(path, busy_timeout).map_err(|error| sql_error(what(), error))?;
    connection.execute("vacuum into ?1", [into]).map(drop).map_err(|error| sql_error(what(), error))
}

/// Writes every page of `copy` over the file at `path`, in one transaction
/// of the file's, then removes the copy: the file is again what it was
/// before its drafts, the steps they applied and what was written since gone.
pub(super) fn take_back(path: &Path, copy: &Path, busy_timeout: Duration) -> Result<()> {
    let what = || format!("take {} back to {}", path.display(), copy.display());
    if !copy.is_file() {
        return Err(Error::invalid(format!(
            "{}: the copy of the database from before its drafts is gone, so nothing takes them back",
            copy.display()
        )));
    }
    let source = Connection::open_with_flags(copy, FLAGS).map_err(|error| sql_error(what(), error))?;
    let mut target = own(path, busy_timeout).map_err(|error| sql_error(what(), error))?;
    let backup = Backup::new(&source, &mut target).map_err(|error| sql_error(what(), error))?;
    match backup.step(-1).map_err(|error| sql_error(what(), error))? {
        StepResult::Done => {}
        // -1 copies every page in one step, the target's writer held by its file
        other => return Err(Error::internal(format!("{}: stopped short: {other:?}", what()))),
    }
    drop(backup);
    drop(source);
    remove(copy).map_err(|error| error.within(what()))
}

/// Removes the copy, which may not be there.
pub(super) fn remove(copy: &Path) -> Result<()> {
    match fs::remove_file(copy) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io(copy.display().to_string(), error)),
    }
}

fn own(path: &Path, busy_timeout: Duration) -> rusqlite::Result<Connection> {
    super::give_mutexes();
    let connection = Connection::open_with_flags(path, FLAGS)?;
    connection.busy_timeout(busy_timeout)?;
    Ok(connection)
}
