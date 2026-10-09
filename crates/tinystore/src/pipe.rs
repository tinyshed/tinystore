//! A connection in memory: the wire's frames to a store in this process, with
//! no socket between. The FFI hands its bytes here, so Bun, Node, Python and
//! Go reach the core through the same session a server gives a socket, and a
//! server gives each of its clients one.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crate::wire::Session;
pub use crate::wire::Wake;
use crate::{Options, Result, Store, TestClock};

/// The stores this process holds through pipes, by directory, and how many
/// pipes each has: one store a directory, closed with its last pipe.
fn stores() -> MutexGuard<'static, HashMap<PathBuf, (Store, usize)>> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, (Store, usize)>>> = OnceLock::new();
    STORES.get_or_init(Mutex::default).lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers a HELLO's challenge: the HMAC-SHA256 of it, keyed with `SERVE`'s
/// secret, which only the directory's owner can read.
pub type Prove = Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

/// What `server.stop` does, called once its answer has left.
pub type Stop = Arc<dyn Fn() + Send + Sync>;

/// What a connection to a store needs of its host beyond the frames.
#[derive(Clone, Default)]
pub struct Connect {
    /// Called when frames become ready after the host took the last ones; a
    /// host that reads with a blocking [`Pipe::recv`] passes none.
    pub wake: Option<Wake>,
    /// A local server's proof; a connection without it, embedded or remote,
    /// gives none.
    pub prove: Option<Prove>,
    /// A connection without it refuses `server.stop`: the program that holds
    /// the store decides when its server stops.
    pub stop: Option<Stop>,
    /// A private server's clock, which `server.clock` reads and moves; a
    /// connection without it refuses the call.
    pub clock: Option<Arc<TestClock>>,
}

impl fmt::Debug for Connect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connect")
            .field("wake", &self.wake.is_some())
            .field("prove", &self.prove.is_some())
            .field("stop", &self.stop.is_some())
            .field("clock", &self.clock)
            .finish()
    }
}

/// One connection in memory to a store.
pub struct Pipe {
    session: Session,
    /// The directory of a store the process registry holds for its pipes;
    /// none for a store the program holds itself.
    dir: Option<PathBuf>,
}

impl Pipe {
    /// Opens the store in `dir`, or joins it when this process holds it through
    /// another pipe. `wake` is called when frames become ready after the last
    /// were taken; a host that reads with a blocking [`Pipe::recv`] passes none.
    pub fn open(dir: impl AsRef<Path>, wake: Option<Wake>) -> Result<Pipe> {
        let dir = std::path::absolute(dir.as_ref()).map_err(|error| crate::Error::io("pipe: its directory", error))?;
        let store = join(&dir)?;
        match Session::new(store, Connect { wake, ..Connect::default() }) {
            Ok(session) => Ok(Pipe { session, dir: Some(dir) }),
            Err(error) => {
                leave(&dir);
                Err(error)
            }
        }
    }

    /// A connection to a store this program holds, as its server gives each
    /// client; the store stays open when the pipe closes.
    pub fn connect(store: &Store, connect: Connect) -> Result<Pipe> {
        Ok(Pipe { session: Session::new(store.clone(), connect)?, dir: None })
    }

    /// Hands frames to the store and returns the frames ready at once.
    pub fn send(&self, frames: &[u8]) -> Vec<u8> {
        self.session.receive(frames);
        self.session.take_ready()
    }

    /// Hands frames to the store and leaves what they answer to
    /// [`Pipe::recv`]: for a host whose one writer takes every frame, so that
    /// frames leave in the order they were made.
    pub fn push(&self, frames: &[u8]) {
        self.session.receive(frames);
    }

    /// The frames ready since, waiting up to `wait` for the first.
    pub fn recv(&self, wait: Duration) -> Vec<u8> {
        self.session.take(wait)
    }

    /// Tells the client the server is closing: a `GOAWAY`, after which a
    /// `REQUEST` is answered `unavailable` unrun, while the streams running
    /// finish.
    pub fn go_away(&self) {
        self.session.go_away();
    }

    /// The streams whose final frame has not left.
    pub fn streams(&self) -> usize {
        self.session.streams()
    }

    /// Whether the session reads nothing more and owes nothing more: the
    /// client said `GOAWAY` or broke a rule, and what was owed was taken.
    pub fn finished(&self) -> bool {
        self.session.finished()
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.session.end();
        if let Some(dir) = &self.dir {
            leave(dir);
        }
    }
}

impl fmt::Debug for Pipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.dir {
            Some(dir) => write!(f, "Pipe({})", dir.display()),
            None => f.write_str("Pipe"),
        }
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
