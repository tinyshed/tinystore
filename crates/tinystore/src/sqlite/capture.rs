use rusqlite::Connection;

use super::connection::{execute, sql_error};
use super::group::COMMIT;
use crate::Result;

#[derive(Clone, Default)]
pub(crate) struct Capture {
    #[cfg(feature = "capture-experiment")]
    state: Option<std::sync::Arc<observed::State>>,
}

impl Capture {
    pub(crate) fn open(path: &std::path::Path, connection: &Connection) -> Result<Self> {
        #[cfg(feature = "capture-experiment")]
        {
            observed::open(path, connection)
        }
        #[cfg(not(feature = "capture-experiment"))]
        {
            let _ = (path, connection);
            Ok(Self::default())
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        #[cfg(feature = "capture-experiment")]
        if let Some(state) = &self.state {
            state.check()?;
        }
        Ok(())
    }

    pub(crate) fn commit(&self, connection: &Connection) -> Result<()> {
        self.check()?;
        #[cfg(feature = "capture-experiment")]
        observed::FRAMES.with(|value| value.set(None));
        execute(connection, COMMIT).map_err(|e| sql_error("SQLite commit", e))?;
        #[cfg(feature = "capture-experiment")]
        if let Some(state) = &self.state
            && let Some(frames) = observed::FRAMES.with(|value| value.take())
        {
            state.capture(connection, frames)?;
        }
        Ok(())
    }

    pub(crate) fn close(&self, connection: &Connection) -> Result<()> {
        self.check()?;
        #[cfg(feature = "capture-experiment")]
        if let Some(state) = &self.state {
            state.observer.closing(&state.path, connection)?;
        }
        #[cfg(not(feature = "capture-experiment"))]
        let _ = connection;
        Ok(())
    }
}

#[cfg(feature = "capture-experiment")]
mod observed {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use rusqlite::Connection;
    use rusqlite::config::DbConfig;
    use rusqlite::hooks::Wal;

    use super::Capture;
    use crate::capture_experiment::Observer;
    use crate::sqlite::given::own;
    use crate::{Error, Result};

    thread_local! { pub(super) static FRAMES: Cell<Option<u32>> = const { Cell::new(None) }; }

    pub(super) struct State {
        pub(super) path: PathBuf,
        pub(super) observer: Arc<dyn Observer>,
        failed: AtomicBool,
    }

    pub(super) fn open(path: &Path, connection: &Connection) -> Result<Capture> {
        let Some(observer) = crate::capture_experiment::observer(path)? else {
            return Ok(Capture::default());
        };
        connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .map_err(|e| Error::invalid("disable checkpoint on close").with_source(e))?;
        connection.wal_hook(Some(committed));
        observer.opened(path, connection)?;
        Ok(Capture { state: Some(Arc::new(State { path: path.to_owned(), observer, failed: AtomicBool::new(false) })) })
    }

    fn committed(_wal: &Wal, frames: std::ffi::c_int) -> rusqlite::Result<()> {
        FRAMES.with(|value| value.set(Some(frames as u32)));
        Ok(())
    }

    impl State {
        pub(super) fn check(&self) -> Result<()> {
            if self.failed.load(Ordering::Acquire) {
                return Err(Error::new(
                    crate::ErrorKind::Unavailable,
                    "capture failed; reopen to recover before writing",
                ));
            }
            Ok(())
        }

        pub(super) fn capture(&self, connection: &Connection, frames: u32) -> Result<()> {
            let result = (|| {
                if self.observer.committed(&self.path, connection, frames)? {
                    // The observer has synced its complete prefix before requesting this.
                    connection
                        .query_row(own!("pragma wal_checkpoint(truncate)"), [], |_| Ok(()))
                        .map_err(|e| Error::new(crate::ErrorKind::Unavailable, "capture checkpoint").with_source(e))?;
                    self.observer.checkpointed(&self.path, connection)?;
                }
                Ok(())
            })();
            if result.is_err() {
                self.failed.store(true, Ordering::Release);
            }
            result
        }
    }
}
