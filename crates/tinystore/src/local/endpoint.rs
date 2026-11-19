//! The endpoint a store opened with `cli` serves the command line on, while
//! its program runs: the socket or the named pipe a sidecar would listen on,
//! of the user's alone, and the `SERVE` that names it, with `"embedded"`.
//!
//! It runs no async runtime: a thread takes connections, and each connection
//! has one that hands what the client sends to a [`Pipe`] and one that writes
//! what the pipe owes. Every wait looks up each tick at whether the store
//! closes, so that a close wakes a thread blocked in an accept, a read or a
//! write on every system alike:
//!
//! ```text
//! close   stop taking connections       a waiting accept gives up within a tick
//!         GOAWAY to each                a REQUEST after it is answered unavailable, unrun
//!         up to 2 s for calls running   the streams under way finish and leave
//!         cut what is left              every read and write gives up within a tick
//!         remove SERVE, then the store closes and lets go of LOCK last
//! ```
//!
//! A connection is `Admin`, as a server's local one is, but `server.stop` and
//! `server.clock` are refused: the program decides when its store closes, and
//! its clock is its own.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{Holder, Serve};
use crate::Store;
use crate::pipe::{Connect, Pipe};
use crate::store::WeakStore;

/// The connections served at once: an operator's commands, not a program's
/// clients. The next is told `GOAWAY` `limit` at its HELLO.
pub(crate) const MOST: usize = 4;
/// How long a close lets the calls running finish before it cuts them off.
const LEAVING: Duration = Duration::from_secs(2);
/// How long a client may take to say HELLO before its connection ends.
const HELLO_TIME: Duration = Duration::from_secs(5);
/// How long a client that closed its side waits for the streams it began.
const DRAIN: Duration = Duration::from_secs(10);
/// How often a wait looks whether to give up: a close is seen this late.
const TICK: Duration = Duration::from_millis(20);
/// How often a Windows accept waiting for a client looks whether the store
/// closes: an idle store wakes this often, and its close waits this long.
#[cfg(windows)]
const ACCEPT_TICK: Duration = Duration::from_millis(100);
/// How long the endpoint waits before it takes connections again after one
/// failed, so that a lack of handles does not spin it.
const AGAIN: Duration = Duration::from_millis(10);
/// The bytes a connection reads from its client at once.
const READ_ROOM: usize = 64 << 10;
/// The bytes a connection's writer takes from its pipe at once.
const WRITE_ROOM: usize = 256 << 10;

/// The endpoint of a store opened with `cli`, served until it is stopped.
pub(crate) struct Endpoint {
    shared: Arc<Shared>,
    accepting: Option<JoinHandle<()>>,
    server: PathBuf,
    #[cfg(unix)]
    socket: PathBuf,
}

/// What the endpoint's threads look at.
#[derive(Default)]
struct Shared {
    /// The store closes: no connection more, and `GOAWAY` to each.
    closing: AtomicBool,
    /// The close waited long enough: every read and write gives up.
    cut: AtomicBool,
    /// The connections served now, of [`MOST`].
    held: AtomicUsize,
    connections: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    fn cut(&self) -> bool {
        self.cut.load(Ordering::Acquire)
    }
}

impl Endpoint {
    /// Listens for the command line on the store's socket or pipe, then
    /// publishes `SERVE`: a `SERVE` and a socket a process that died left
    /// are removed first, since the store holds `LOCK` and none listens on
    /// them.
    pub(crate) fn start(store: &Store) -> io::Result<Endpoint> {
        let server = super::server_dir(store.dir())?;
        super::unpublish(&server)?;
        let (listener, endpoint) = Listener::bind(store.dir(), &server)?;
        let serve = Serve::new(vec![endpoint], Holder::Embedded)?;
        let connect = Connect { prove: Some(serve.prove()), ..Connect::default() };
        super::publish(&server, &serve)?;
        let shared = Arc::new(Shared::default());
        #[cfg(unix)]
        let socket = listener.path.clone();
        let accepting = {
            let (shared, store) = (Arc::clone(&shared), store.downgrade());
            thread::Builder::new()
                .name("tinystore-cli".to_owned())
                .spawn(move || accept_all(listener, &store, &connect, &shared))
        };
        let mut endpoint = Endpoint {
            shared,
            accepting: None,
            server,
            #[cfg(unix)]
            socket,
        };
        // a thread that did not start leaves nothing listening, and no SERVE:
        // the endpoint dropped stops what it began
        endpoint.accepting = Some(accepting?);
        Ok(endpoint)
    }

    /// Stops as the module's table says, and removes `SERVE` last. A thread
    /// of the endpoint's own that stops it, as one that drops the store's
    /// last handle does, is not waited for: it would wait for itself.
    pub(crate) fn stop(&mut self) {
        if self.shared.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        // a Unix accept waits on no tick: a connection of the close's own wakes
        // it, and one that cannot be made leaves the thread to end when it
        // wakes, rather than the close waiting for it for ever
        #[cfg(unix)]
        let woken = std::os::unix::net::UnixStream::connect(&self.socket).is_ok();
        #[cfg(windows)]
        let woken = true;
        let accepting = self.accepting.take();
        if woken {
            joined(accepting);
        }
        let deadline = Instant::now() + LEAVING;
        while self.shared.held.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            thread::sleep(TICK);
        }
        self.shared.cut.store(true, Ordering::Release);
        let connections = std::mem::take(&mut *self.shared.connections.lock().unwrap_or_else(PoisonError::into_inner));
        for connection in connections {
            joined(Some(connection));
        }
        if let Err(error) = super::unpublish(&self.server) {
            tracing::warn!(target: "tinystore", %error, "a store's SERVE for the command line was not removed");
        }
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Waits for a thread to end, unless it is this one.
fn joined(handle: Option<JoinHandle<()>>) {
    if let Some(handle) = handle.filter(|handle| handle.thread().id() != thread::current().id()) {
        let _ = handle.join();
    }
}

/// Takes connections until the store closes, each served on threads of its
/// own; one that fails to connect fails alone.
fn accept_all(mut listener: Listener, store: &WeakStore, connect: &Connect, shared: &Arc<Shared>) {
    loop {
        let stream = match listener.accept(&|| shared.closing()) {
            Ok(Some(stream)) => stream,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(target: "tinystore", %error, "a command line client did not connect");
                thread::sleep(AGAIN);
                continue;
            }
        };
        // the close's own connection, which woke the accept, or a client as the store closes
        if shared.closing() {
            return;
        }
        let Some(store) = store.upgrade() else { return };
        serve(stream, store, connect, shared);
    }
}

/// Serves one client on a thread of its own, turned away at its HELLO when
/// [`MOST`] are served already.
fn serve(stream: Stream, store: Store, connect: &Connect, shared: &Arc<Shared>) {
    let mut connections = shared.connections.lock().unwrap_or_else(PoisonError::into_inner);
    connections.retain(|connection| !connection.is_finished());
    let (connect, held) = if shared.held.load(Ordering::Acquire) >= MOST {
        let why = format!("the program that holds this store takes {MOST} command line connections at once: try again");
        (Connect { turned_away: Some(why), ..connect.clone() }, None)
    } else {
        shared.held.fetch_add(1, Ordering::AcqRel);
        (connect.clone(), Some(Held(Arc::clone(shared))))
    };
    let shared = Arc::clone(shared);
    let started = thread::Builder::new().name("tinystore-cli-read".to_owned()).spawn(move || {
        let _held = held;
        connected(&stream, &store, connect, &shared);
    });
    match started {
        Ok(connection) => connections.push(connection),
        Err(error) => tracing::debug!(target: "tinystore", %error, "a command line client was not served"),
    }
}

/// A connection's place among the [`MOST`] served, given back as it ends.
struct Held(Arc<Shared>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.held.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a connection's two threads tell each other.
#[derive(Default)]
struct Ending {
    /// The reader is done and the streams it waited for finished: the
    /// writer writes what is owed and leaves.
    done: AtomicBool,
    /// The writer left, its client gone: the reader reads no more.
    gone: AtomicBool,
}

/// One client until it leaves, breaks the protocol, or the store closes:
/// what it sends goes to a pipe on the store, and what the pipe owes leaves
/// on a writer thread in the order it was made.
fn connected(stream: &Arc<StreamEnd>, store: &Store, connect: Connect, shared: &Arc<Shared>) {
    let pipe = match Pipe::connect(store, connect) {
        Ok(pipe) => Arc::new(pipe),
        Err(error) => return tracing::warn!(target: "tinystore", %error, "a command line connection did not open"),
    };
    let ending = Arc::new(Ending::default());
    let writer = {
        let (stream, pipe, ending, shared) =
            (Arc::clone(stream), Arc::clone(&pipe), Arc::clone(&ending), Arc::clone(shared));
        thread::Builder::new()
            .name("tinystore-cli-write".to_owned())
            .spawn(move || write_frames(&stream, &pipe, &ending, &shared))
    };
    let Ok(writer) = writer else { return };
    read_frames(stream, &pipe, &ending, shared);
    pipe.client_closed();
    drain(&pipe, &ending, shared);
    ending.done.store(true, Ordering::Release);
    let _ = writer.join();
    stream.close();
}

/// Hands the client's bytes to the pipe until it leaves, breaks a rule, the
/// writer left or the close cuts it off; a close tells it `GOAWAY` and reads
/// on while its streams finish. While the pipe is full it reads nothing, so
/// that a client that sends and never reads holds memory at the pipe's bound.
fn read_frames(stream: &StreamEnd, pipe: &Pipe, ending: &Ending, shared: &Shared) {
    let mut buffer = vec![0; READ_ROOM];
    let hello_by = Instant::now() + HELLO_TIME;
    let mut going_away = false;
    loop {
        if !going_away && shared.closing() {
            going_away = true;
            pipe.go_away();
        }
        let over = || {
            pipe.finished()
                || ending.gone.load(Ordering::Acquire)
                || shared.cut()
                || (going_away && pipe.streams() == 0)
                || (!pipe.welcomed() && Instant::now() >= hello_by)
        };
        if over() {
            return;
        }
        if pipe.full() {
            thread::sleep(TICK);
            continue;
        }
        match stream.read(&mut buffer, &|| over() || (!going_away && shared.closing())) {
            Ok(Some(0)) | Err(_) => return,
            Ok(Some(read)) => pipe.push(&buffer[..read]),
            Ok(None) => {}
        }
    }
}

/// Waits for the streams running once the client closed its side, so that a
/// client that closed it after its last call still gets the answer.
fn drain(pipe: &Pipe, ending: &Ending, shared: &Shared) {
    let deadline = Instant::now() + DRAIN;
    while pipe.streams() > 0
        && !pipe.finished()
        && !ending.gone.load(Ordering::Acquire)
        && !shared.cut()
        && Instant::now() < deadline
    {
        thread::sleep(TICK);
    }
}

/// Writes what the pipe owes as it becomes ready, until the reader is done
/// and the last of it left, the client is gone or the close cuts it off.
fn write_frames(stream: &StreamEnd, pipe: &Pipe, ending: &Ending, shared: &Shared) {
    // what the pipe owes is copied here a room at a time, rather than into
    // bytes of its own each time
    let mut room = vec![0; WRITE_ROOM];
    'writing: loop {
        // read before the frames are taken: every frame owed before the end
        // was said is taken by this pass
        let last = ending.done.load(Ordering::Acquire) || pipe.finished();
        let mut wait = TICK;
        loop {
            let taken = pipe.recv_into(wait, &mut room);
            wait = Duration::ZERO;
            if taken > 0 && write_all(stream, &room[..taken], shared).is_err() {
                break 'writing;
            }
            // a room filled may have left more, which no wait announces
            if taken < room.len() {
                break;
            }
        }
        if last || shared.cut() {
            break;
        }
    }
    ending.gone.store(true, Ordering::Release);
}

fn write_all(stream: &StreamEnd, mut bytes: &[u8], shared: &Shared) -> io::Result<()> {
    while !bytes.is_empty() {
        match stream.write(bytes, &|| shared.cut())? {
            Some(0) => return Err(io::ErrorKind::WriteZero.into()),
            Some(written) => bytes = &bytes[written..],
            None => return Err(io::Error::other("the store closed")),
        }
    }
    Ok(())
}

/// A connection's end, which its reader and its writer share.
type Stream = Arc<StreamEnd>;

#[cfg(unix)]
use unix::{Listener, StreamEnd};
#[cfg(windows)]
use windows::{Listener, StreamEnd};

#[cfg(unix)]
mod unix {
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::TICK;

    pub(super) struct Listener {
        listener: UnixListener,
        pub(super) path: PathBuf,
    }

    impl Listener {
        pub(super) fn bind(store: &Path, server: &Path) -> io::Result<(Listener, String)> {
            let socket = crate::local::bind_socket(store, server)?;
            Ok((Listener { listener: socket.listener, path: socket.path }, socket.endpoint))
        }

        /// The next client. An accept waits for one without a tick, since a
        /// close wakes it by connecting; `stop` is read once it returns.
        pub(super) fn accept(&mut self, stop: &dyn Fn() -> bool) -> io::Result<Option<Arc<StreamEnd>>> {
            let (stream, _) = self.listener.accept()?;
            if stop() {
                return Ok(None);
            }
            stream.set_read_timeout(Some(TICK))?;
            stream.set_write_timeout(Some(TICK))?;
            Ok(Some(Arc::new(StreamEnd(stream))))
        }
    }

    /// A client's socket, its reads and writes timed out each tick.
    pub(super) struct StreamEnd(UnixStream);

    impl StreamEnd {
        pub(super) fn read(&self, into: &mut [u8], stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
            loop {
                match (&self.0).read(into) {
                    Ok(read) => return Ok(Some(read)),
                    Err(error) if waited(&error) => {
                        if stop() {
                            return Ok(None);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
        }

        pub(super) fn write(&self, bytes: &[u8], stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
            loop {
                match (&self.0).write(bytes) {
                    Ok(written) => return Ok(Some(written)),
                    Err(error) if waited(&error) => {
                        if stop() {
                            return Ok(None);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
        }

        /// Tells the client the connection ended, though a clone of the
        /// socket may outlive this end.
        pub(super) fn close(&self) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Whether a call returned for its timeout, a tick passed with nothing.
    fn waited(error: &io::Error) -> bool {
        matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;
    use std::sync::Arc;

    use super::{ACCEPT_TICK, TICK};
    use crate::local::windows::PipeEnd;

    pub(super) struct Listener {
        name: String,
        waiting: Option<PipeEnd>,
    }

    impl Listener {
        /// Creates the store's pipe as the first instance of its name, so
        /// that a process squatting on it is found here rather than by the
        /// clients.
        pub(super) fn bind(store: &Path, _server: &Path) -> io::Result<(Listener, String)> {
            let (name, endpoint) = crate::local::pipe_names(store);
            let first = PipeEnd::new(crate::local::pipe_instance(&name, true)?);
            Ok((Listener { name, waiting: Some(first) }, endpoint))
        }

        /// The next client, on the instance made after the last one came, so
        /// that a client never finds none listening: none once `stop` says.
        pub(super) fn accept(&mut self, stop: &dyn Fn() -> bool) -> io::Result<Option<Arc<StreamEnd>>> {
            let pipe = match self.waiting.take() {
                Some(pipe) => pipe,
                None => PipeEnd::new(crate::local::pipe_instance(&self.name, false)?),
            };
            if !pipe.connect(ACCEPT_TICK, stop)? {
                return Ok(None);
            }
            match crate::local::pipe_instance(&self.name, false) {
                Ok(next) => self.waiting = Some(PipeEnd::new(next)),
                Err(error) => tracing::debug!(target: "tinystore", %error, "a pipe instance was not made"),
            }
            Ok(Some(Arc::new(StreamEnd(pipe))))
        }
    }

    /// A client's instance of the pipe.
    pub(super) struct StreamEnd(PipeEnd);

    impl StreamEnd {
        pub(super) fn read(&self, into: &mut [u8], stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
            self.0.read(into, TICK, stop)
        }

        pub(super) fn write(&self, bytes: &[u8], stop: &dyn Fn() -> bool) -> io::Result<Option<usize>> {
            self.0.write(bytes, TICK, stop)
        }

        /// Nothing before the handle closes: what the writer wrote stays in
        /// the pipe's buffer for the client to read, which a disconnect
        /// would throw away.
        pub(super) fn close(&self) {}
    }
}

// what the command line says to it, which needs the protocol's client
#[cfg(all(test, feature = "client"))]
#[path = "endpoint_tests.rs"]
mod tests;
