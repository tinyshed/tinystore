//! A connection's protocol, apart from the bytes' transport: the session a
//! host drives, and the state its parts share.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

#[cfg(feature = "backup")]
use super::backup;
#[cfg(feature = "blobs")]
use super::blobs;
#[cfg(feature = "channels")]
use super::channels;
use super::codec::Message;
#[cfg(feature = "config")]
use super::config;
use super::frame::{self, Frame, Kind};
#[cfg(feature = "jobs")]
use super::jobs;
use super::kv;
#[cfg(feature = "locks")]
use super::locks;
#[cfg(feature = "metrics")]
use super::metrics;
use super::output::Output;
use super::protocol::{Failure, GoAway};
#[cfg(feature = "records")]
use super::records;
#[cfg(feature = "search")]
use super::search;
#[cfg(feature = "sql")]
use super::sql;
use crate::cancel::Tokens;
use crate::kv::Handed;
use crate::pipe::{Capability, Connect};
use crate::pool::Pool;
use crate::sync::lock;
use crate::{Result, Store};

mod cancel;
mod credit;
mod intake;
mod lending;
mod routing;
#[cfg(any(
    feature = "jobs",
    feature = "sql",
    feature = "blobs",
    feature = "records",
    feature = "metrics",
    feature = "config",
    feature = "backup",
    feature = "channels"
))]
mod streams;

/// Streams a client may have open at once: as many writes as one grouped
/// commit carries, so an event loop with many writes in flight fills commits.
const IN_FLIGHT: u64 = 1024;
/// Bytes of REQUEST and DATA bodies a client may send before credit comes back.
pub(super) const CONNECTION_CREDIT: u64 = 8 << 20;
const STREAM_CREDIT: u64 = 2 << 20;
/// The largest HELLO read before a body is agreed.
const HELLO_MOST: usize = 1 << 16;
/// What a connection may owe its client before it reads no frame more of
/// it and its workers start no call more: a PING's PONG counts against no
/// credit, and a call's answer leaves its stream's credit once queued, so a
/// client that sends and never reads would otherwise grow the server's
/// memory at the rate it sends.
pub(super) const OUTPUT_MOST: usize = 8 << 20;
/// What a connection owes its client below which it reads the frames that
/// waited, and the calls and subscriptions that stopped for its output go on.
const OUTPUT_LOW: usize = 1 << 20;

/// Requests read from a connection at once that send its point reads to the
/// workers. Five in flight are answered as fast where they are read, on a
/// host whose threads wake slowly; six are not.
const CROWD: usize = 6;

/// What a session calls when frames become ready after the host took the last
/// ones: once, until the host takes again. It must return quickly and not call
/// into the session.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// One connection's protocol, apart from the bytes' transport: the frames a
/// client sends go in, the frames it is owed come out. A socket's server and
/// the FFI's pipe drive the same session.
pub(crate) struct Session {
    shared: Arc<Shared>,
}

struct Shared {
    store: Store,
    /// The store's pool, where the calls that do not answer inline run.
    workers: Arc<Pool>,
    kv: kv::Handles,
    #[cfg(feature = "config")]
    config: config::Configs,
    #[cfg(feature = "locks")]
    locks: Arc<locks::Holds>,
    #[cfg(feature = "channels")]
    channels: channels::Subscriptions,
    #[cfg(feature = "jobs")]
    jobs: jobs::Handles,
    #[cfg(feature = "sql")]
    sql: sql::Handles,
    #[cfg(feature = "search")]
    search: search::Handles,
    #[cfg(feature = "blobs")]
    blobs: blobs::Handles,
    #[cfg(feature = "records")]
    records: records::Handles,
    #[cfg(feature = "metrics")]
    metrics: metrics::Handles,
    #[cfg(feature = "backup")]
    backups: backup::Backups,
    input: Mutex<Input>,
    output: Mutex<Output>,
    /// Clients' runs of once, by stream, from the REQUEST to the last frame.
    runs: Mutex<HashMap<u32, ClientRun>>,
    /// The tokens of the reads the workers run, by stream, which a CANCEL of
    /// the stream or the connection's end cancels.
    reads: Tokens<u32>,
    /// Numbers each run's asking, so that a wait that ends after its stream
    /// was cancelled, and its number named again, finds another run there.
    asked: AtomicU64,
    /// Streams whose final frame has not left, which a closing server waits
    /// for, each with its credit and the connection's bytes it holds.
    open: Mutex<HashMap<u32, OpenStream>>,
    /// REQUEST and DATA bytes the client sent and the server has not granted
    /// back: past the connection's credit is the client's fault.
    outstanding: AtomicU64,
    /// Bytes the server let go of and has not granted back yet: a grant goes
    /// once half the window has gathered.
    to_grant: AtomicU64,
    /// The largest body both sides agreed, once WELCOME went out: no frame of
    /// the server's passes it.
    agreed: AtomicUsize,
    /// Point reads the workers have of this connection, not yet answered.
    reads_away: AtomicUsize,
    /// The connection's HELLO admitted it to read only.
    read_only: AtomicBool,
    /// What the output owes, as of its last change: what the reading of
    /// frames looks at without the output's lock.
    owing: AtomicUsize,
    /// The output owed past [`OUTPUT_MOST`] when a frame came: the frames
    /// from it on wait in the reader until the host takes what is owed.
    held: AtomicBool,
    /// What waits for the output to drain: calls and subscriptions that
    /// stopped while the connection owed its client too much.
    waiting_output: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    ready: Condvar,
    /// The host's wake, called under this lock, which the session's end
    /// takes to let it go: a host frees its wake once the pipe closed, and a
    /// thread that queued a frame just before must not call it after.
    wake: Mutex<Option<Wake>>,
    connect: Connect,
}

/// A stream in use: the DATA its client may still send on it, and the bytes of
/// its REQUEST and DATA the connection's credit gets back once the server lets
/// go of them, as it grants the stream credit or ends the stream.
struct OpenStream {
    left: u64,
    held: u64,
}

/// Where a client's run of once stands.
enum ClientRun {
    /// Asking for its key, or waiting for the run that holds it.
    Asking(u64),
    /// Handed to the client, until its last DATA, a CANCEL or the session's end.
    Handed(Handed),
}

#[derive(Default)]
struct Input {
    reader: frame::Reader,
    /// The largest body both sides agreed, once WELCOME went out.
    max_body: Option<usize>,
    /// The DATA bytes the client takes on a stream before it grants more.
    stream_credit: u64,
    /// GOAWAY went out: a REQUEST after it is answered `unavailable`, unrun.
    going_away: bool,
    /// What the client may do, once its HELLO was admitted.
    capability: Option<Capability>,
    ended: bool,
    /// Whether the bytes last taken held a crowd of requests.
    crowd: bool,
}

impl Session {
    pub(crate) fn new(store: Store, mut connect: Connect) -> Result<Session> {
        let workers = Pool::of(&store)?;
        let wake = Mutex::new(connect.wake.take());
        let shared = Shared {
            store,
            workers,
            kv: kv::Handles::default(),
            #[cfg(feature = "config")]
            config: config::Configs::default(),
            #[cfg(feature = "locks")]
            locks: Arc::default(),
            #[cfg(feature = "channels")]
            channels: channels::Subscriptions::default(),
            #[cfg(feature = "jobs")]
            jobs: jobs::Handles::default(),
            #[cfg(feature = "sql")]
            sql: sql::Handles::default(),
            #[cfg(feature = "search")]
            search: search::Handles::default(),
            #[cfg(feature = "blobs")]
            blobs: blobs::Handles::default(),
            #[cfg(feature = "records")]
            records: records::Handles::default(),
            #[cfg(feature = "metrics")]
            metrics: metrics::Handles::default(),
            #[cfg(feature = "backup")]
            backups: backup::Backups::default(),
            input: Mutex::new(Input::default()),
            output: Mutex::new(Output::default()),
            runs: Mutex::new(HashMap::new()),
            reads: Tokens::default(),
            asked: AtomicU64::new(0),
            open: Mutex::new(HashMap::new()),
            outstanding: AtomicU64::new(0),
            to_grant: AtomicU64::new(0),
            agreed: AtomicUsize::new(HELLO_MOST),
            reads_away: AtomicUsize::new(0),
            read_only: AtomicBool::new(false),
            owing: AtomicUsize::new(0),
            held: AtomicBool::new(false),
            waiting_output: Mutex::new(Vec::new()),
            ready: Condvar::new(),
            wake,
            connect,
        };
        shared.store.probe().shift("tinystore_wire_connections", &[], 1.0);
        Ok(Session { shared: Arc::new(shared) })
    }

    /// Takes bytes the client sent, in pieces of any size. Each frame they
    /// finish is answered: a call on the store's workers, everything else at
    /// once. A frame that breaks a rule ends the session with GOAWAY.
    pub(crate) fn receive(&self, bytes: &[u8]) {
        let mut input = self.shared.lock_input();
        if input.ended {
            return;
        }
        input.reader.push(bytes);
        input.crowd = input.reader.holds(CROWD);
        self.shared.read_frames(&mut input);
    }

    /// Whether the connection owes its client so much that it reads no frame
    /// more until the host takes some: what it is handed meanwhile waits.
    pub(crate) fn full(&self) -> bool {
        self.shared.held.load(Ordering::SeqCst)
    }

    /// The bytes ready now, leaving a wake already called to run: the host's
    /// read after a write, which a wake in flight answers anyway, so that a
    /// host writing often is not woken once a commit.
    pub(crate) fn take_ready(&self) -> Vec<u8> {
        let mut output = self.shared.lock_output();
        let taken = output.take_all();
        self.took(output);
        taken
    }

    /// As `take_ready`, into the host's own bytes: as many as fit, and how
    /// many they were.
    pub(crate) fn take_ready_into(&self, into: &mut [u8]) -> usize {
        let mut output = self.shared.lock_output();
        let taken = output.take_into(into);
        self.took(output);
        taken
    }

    /// The bytes the client is owed, waiting up to `wait` for the first; the
    /// next bytes ready wake the host again.
    pub(crate) fn take(&self, wait: Duration) -> Vec<u8> {
        let mut output = self.awaited(wait);
        let taken = output.take_all();
        self.took(output);
        taken
    }

    /// As `take`, into the host's own bytes: as many as fit, and how many
    /// they were. What did not fit stays for the next read, and wakes nobody.
    pub(crate) fn take_into(&self, wait: Duration, into: &mut [u8]) -> usize {
        let mut output = self.awaited(wait);
        let taken = output.take_into(into);
        self.took(output);
        taken
    }

    /// What follows the host's take: what the output owes now counted, then,
    /// its lock let go, what waited for it run, and the frames held for it
    /// read.
    fn took(&self, output: MutexGuard<'_, Output>) {
        self.shared.owing.store(output.owed(), Ordering::SeqCst);
        drop(output);
        self.shared.drained();
        self.shared.read_held();
    }

    /// The output once it owes bytes or `wait` has passed, the host counted
    /// as having read: the next bytes ready wake it again.
    fn awaited(&self, wait: Duration) -> MutexGuard<'_, Output> {
        let deadline = Instant::now() + wait;
        let mut output = self.shared.lock_output();
        while output.owes_nothing() && !output.ended {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            output.awaited = true;
            output = self.shared.ready.wait_timeout(output, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        output.awaited = false;
        output.woken = false;
        output
    }

    /// Tells the client the server is closing; the streams running finish, and
    /// its workers take no job more but answer those they hold.
    pub(crate) fn go_away(&self) {
        let mut input = self.shared.lock_input();
        if input.ended || input.going_away {
            return;
        }
        input.going_away = true;
        let body = GoAway { code: "unavailable".to_owned(), message: "the server is closing".to_owned() }.encode();
        self.shared.send(&[Frame::new(Kind::GoAway, 0, body)]);
        self.end_open("the server is closing");
    }

    /// The client closed its side: no frame comes from it any more, so the
    /// streams only its cancel would end, its subscriptions, follows, tails
    /// and watches, end, and its workers take no job more. The calls running
    /// finish and their answers still leave, for a client that reads on, as
    /// a private child's parent does once it closed stdin.
    pub(crate) fn client_closed(&self) {
        self.end_open("the client closed its side");
    }

    /// Ends the streams that run until their client cancels them, saying why.
    fn end_open(&self, why: &str) {
        #[cfg(feature = "jobs")]
        {
            self.shared.jobs.halt_works();
            for watch in self.shared.jobs.take_watches() {
                watch.close(Err(Failure::unavailable(format!("{why}; watch again on another connection"))));
            }
        }
        #[cfg(feature = "records")]
        self.shared.records.go_away(why);
        #[cfg(feature = "config")]
        self.shared.config.go_away(why);
        #[cfg(feature = "channels")]
        self.shared.channels.go_away(why);
        let _ = why;
    }

    pub(crate) fn streams(&self) -> usize {
        self.shared.lock_open().len()
    }

    pub(crate) fn welcomed(&self) -> bool {
        self.shared.lock_input().max_body.is_some()
    }

    /// Whether the session reads nothing more and owes nothing more.
    pub(crate) fn finished(&self) -> bool {
        let output = self.shared.lock_output();
        output.ended && output.owes_nothing()
    }

    /// Ends the session: reads under way stop, other calls finish, what they
    /// answer is dropped, the calls waiting for the client to read are
    /// dropped unrun, the runs handed to the client are let go, and its
    /// workers end, the jobs they held failing their attempt. It waits for the
    /// workers to write that, so that a store closing with its last pipe finds
    /// them stopped rather than waits out their timeout.
    pub(crate) fn end(&self) {
        self.shared.lock_input().ended = true;
        let mut output = self.shared.lock_output();
        output.ended = true;
        output.clear();
        self.shared.owing.store(0, Ordering::SeqCst);
        drop(output);
        self.shared.ready.notify_all();
        // waits for a wake under way: once this returns none runs or will
        lock(&self.shared.wake).take();
        // a call that waits holds the session, which holds what waits
        let waiting = std::mem::take(&mut *lock(&self.shared.waiting_output));
        drop(waiting);
        let runs = std::mem::take(&mut *self.shared.lock_runs());
        drop(runs);
        self.shared.reads.cancel_all();
        #[cfg(feature = "jobs")]
        {
            for watch in self.shared.jobs.take_watches() {
                watch.end();
            }
            for work in self.shared.jobs.take_works() {
                work.end();
            }
        }
        #[cfg(feature = "sql")]
        self.shared.sql.end();
        #[cfg(feature = "blobs")]
        self.shared.blobs.end();
        #[cfg(feature = "records")]
        self.shared.records.end();
        #[cfg(feature = "metrics")]
        self.shared.metrics.end();
        #[cfg(feature = "config")]
        self.shared.config.end();
        #[cfg(feature = "locks")]
        self.shared.locks.end();
        #[cfg(feature = "channels")]
        self.shared.channels.end();
        #[cfg(feature = "backup")]
        self.shared.backups.end_all("the connection is gone");
    }
}

impl Shared {
    fn lock_open(&self) -> MutexGuard<'_, HashMap<u32, OpenStream>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_input(&self) -> MutexGuard<'_, Input> {
        self.input.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_output(&self) -> MutexGuard<'_, Output> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_runs(&self) -> MutexGuard<'_, HashMap<u32, ClientRun>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A call that panics answers `internal` rather than leaving its stream open.
fn guarded<T>(call: impl FnOnce() -> std::result::Result<T, Failure>) -> std::result::Result<T, Failure> {
    catch_unwind(AssertUnwindSafe(call)).unwrap_or_else(|_| Err(Failure::internal("a call panicked")))
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

impl Drop for Shared {
    fn drop(&mut self) {
        self.store.probe().shift("tinystore_wire_connections", &[], -1.0);
    }
}

/// A method's name as the protocol spells it, `kv.get`.
pub(super) fn method_name(method: u16) -> &'static str {
    super::protocol::METHODS.iter().find(|(_, id, _)| *id == method).map_or("unknown", |(name, ..)| name)
}
