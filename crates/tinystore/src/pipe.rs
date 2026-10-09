//! A connection in memory: the wire's frames to a store in this process, with
//! no socket between. The FFI hands its bytes here, so Bun, Node, Python and
//! Go reach the core through the same session a server gives a socket.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crate::wire::Session;
pub use crate::wire::Wake;
use crate::{Options, Result, Store};

/// The stores this process holds through pipes, by directory, and how many
/// pipes each has: one store a directory, closed with its last pipe.
fn stores() -> MutexGuard<'static, HashMap<PathBuf, (Store, usize)>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, (Store, usize)>>> = OnceLock::new();
    STORES.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

/// One connection in memory to the store in a directory.
pub struct Pipe {
    session: Session,
    dir: PathBuf,
}

impl Pipe {
    /// Opens the store in `dir`, or joins it when this process holds it through
    /// another pipe. `wake` is called when frames become ready after the last
    /// were taken; a host that reads with a blocking [`Pipe::recv`] passes none.
    pub fn open(dir: impl AsRef<Path>, wake: Option<Wake>) -> Result<Pipe> {
        let dir = std::path::absolute(dir.as_ref()).map_err(|error| crate::Error::io("pipe: its directory", error))?;
        let store = join(&dir)?;
        match Session::new(store, wake) {
            Ok(session) => Ok(Pipe { session, dir }),
            Err(error) => {
                leave(&dir);
                Err(error)
            }
        }
    }

    /// Hands frames to the store and returns the frames ready at once.
    pub fn send(&self, frames: &[u8]) -> Vec<u8> {
        self.session.receive(frames);
        self.session.take(Duration::ZERO)
    }

    /// The frames ready since, waiting up to `wait` for the first.
    pub fn recv(&self, wait: Duration) -> Vec<u8> {
        self.session.take(wait)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.session.end();
        leave(&self.dir);
    }
}

impl fmt::Debug for Pipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pipe({})", self.dir.display())
    }
}

fn join(dir: &Path) -> Result<Store> {
    let mut stores = stores();
    if let Some((store, pipes)) = stores.get_mut(dir) {
        *pipes += 1;
        return Ok(store.clone());
    }
    let store = Store::open(dir, Options::default())?;
    stores.insert(dir.to_path_buf(), (store.clone(), 1));
    Ok(store)
}

/// Lets go of a pipe's store, closing it with its last pipe.
fn leave(dir: &Path) {
    let mut stores = stores();
    let Some((store, pipes)) = stores.get_mut(dir) else {
        return;
    };
    *pipes -= 1;
    if *pipes > 0 {
        return;
    }
    let store = store.clone();
    stores.remove(dir);
    drop(stores);
    if let Err(error) = store.close() {
        tracing::warn!(target: "tinystore", %error, "a pipe's store did not close cleanly");
    }
}

#[cfg(test)]
#[path = "pipe_tests.rs"]
mod tests;
