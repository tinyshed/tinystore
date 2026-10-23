use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use super::Route;
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
use super::kv::{self, Run};
#[cfg(feature = "locks")]
use super::locks;
#[cfg(feature = "metrics")]
use super::metrics;
use super::output::Output;
use super::protocol::method::{self, Access};
use super::protocol::{Empty, Failure, GoAway, Hello, ServerClock, Welcome};
#[cfg(feature = "records")]
use super::records;
#[cfg(feature = "search")]
use super::search;
#[cfg(feature = "sql")]
use super::sql;
use super::workers::Workers;
#[cfg(feature = "sql")]
use crate::Transaction;
use crate::clock::from_unix_millis;
use crate::engine::SharedFile;
use crate::kv::Handed;
use crate::pipe::{Capability, Connect};
use crate::sync::lock;
use crate::{CancellationToken, Clock, Result, Store, unix_millis};

/// The protocol this server speaks; a client of another is refused.
const PROTOCOL: u64 = 1;
const MAX_BODY: u64 = 4 << 20;
/// Streams a client may have open at once: as many writes as one grouped
/// commit carries, so an event loop with many writes in flight fills commits.
const IN_FLIGHT: u64 = 1024;
/// Bytes of REQUEST and DATA bodies a client may send before credit comes back.
const CONNECTION_CREDIT: u64 = 8 << 20;
const STREAM_CREDIT: u64 = 2 << 20;
/// What a client grants a stream when its HELLO does not say.
const CLIENT_STREAM_CREDIT: u64 = 2 << 20;
/// The largest HELLO read before a body is agreed.
const HELLO_MOST: usize = 1 << 16;
/// The bytes of a HELLO's challenge, which a local server's proof answers.
const CHALLENGE: usize = 16;
/// What a connection owes its client below which the subscriptions that
/// stopped for its output go on.
#[cfg(feature = "channels")]
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
    workers: Arc<Workers>,
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
    reads: Mutex<HashMap<u32, CancellationToken>>,
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
    /// What waits for the output to drain: subscriptions that stopped while
    /// the connection owed its client too much.
    #[cfg(feature = "channels")]
    waiting_output: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    ready: Condvar,
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
    pub(crate) fn new(store: Store, connect: Connect) -> Result<Session> {
        let workers = Workers::of(&store)?;
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
            reads: Mutex::new(HashMap::new()),
            asked: AtomicU64::new(0),
            open: Mutex::new(HashMap::new()),
            outstanding: AtomicU64::new(0),
            to_grant: AtomicU64::new(0),
            agreed: AtomicUsize::new(HELLO_MOST),
            reads_away: AtomicUsize::new(0),
            read_only: AtomicBool::new(false),
            #[cfg(feature = "channels")]
            waiting_output: Mutex::new(Vec::new()),
            ready: Condvar::new(),
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
        while !input.ended {
            let max_body = input.max_body.unwrap_or(HELLO_MOST);
            match input.reader.next(max_body) {
                Ok(Some(frame)) => self.shared.handle(&mut input, frame),
                Ok(None) => return,
                Err(refused) => self.shared.go_away(&mut input, "protocol", &refused.0),
            }
        }
    }

    /// The bytes ready now, leaving a wake already called to run: the host's
    /// read after a write, which a wake in flight answers anyway, so that a
    /// host writing often is not woken once a commit.
    pub(crate) fn take_ready(&self) -> Vec<u8> {
        let taken = self.shared.lock_output().take_all();
        self.shared.drained();
        taken
    }

    /// As `take_ready`, into the host's own bytes: as many as fit, and how
    /// many they were.
    pub(crate) fn take_ready_into(&self, into: &mut [u8]) -> usize {
        let taken = self.shared.lock_output().take_into(into);
        self.shared.drained();
        taken
    }

    /// The bytes the client is owed, waiting up to `wait` for the first; the
    /// next bytes ready wake the host again.
    pub(crate) fn take(&self, wait: Duration) -> Vec<u8> {
        let taken = self.awaited(wait).take_all();
        self.shared.drained();
        taken
    }

    /// As `take`, into the host's own bytes: as many as fit, and how many
    /// they were. What did not fit stays for the next read, and wakes nobody.
    pub(crate) fn take_into(&self, wait: Duration, into: &mut [u8]) -> usize {
        let taken = self.awaited(wait).take_into(into);
        self.shared.drained();
        taken
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
        #[cfg(feature = "jobs")]
        {
            self.shared.jobs.halt_works();
            for watch in self.shared.jobs.take_watches() {
                watch.close(Err(Failure::unavailable("the server is closing; watch again on another connection")));
            }
        }
        #[cfg(feature = "records")]
        self.shared.records.go_away();
        #[cfg(feature = "config")]
        self.shared.config.go_away();
        #[cfg(feature = "channels")]
        self.shared.channels.go_away();
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
    /// answer is dropped, the runs handed to the client are let go, and its
    /// workers end, the jobs they held failing their attempt. It waits for the
    /// workers to write that, so that a store closing with its last pipe finds
    /// them stopped rather than waits out their timeout.
    pub(crate) fn end(&self) {
        self.shared.lock_input().ended = true;
        let mut output = self.shared.lock_output();
        output.ended = true;
        output.clear();
        drop(output);
        self.shared.ready.notify_all();
        let runs = std::mem::take(&mut *self.shared.lock_runs());
        drop(runs);
        let reads: Vec<CancellationToken> = lock(&self.shared.reads).drain().map(|(_, read)| read).collect();
        reads.iter().for_each(CancellationToken::cancel);
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
    fn handle(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        match (input.max_body, frame.kind) {
            (None, Kind::Hello) => self.welcome(input, &frame.body),
            (None, kind) => self.go_away(input, "protocol", &format!("a {kind:?} before HELLO")),
            (Some(_), Kind::Request) => self.request(input, frame),
            (Some(_), Kind::Data) => self.data(input, frame),
            (Some(_), Kind::Cancel) => self.cancel(frame.stream),
            (Some(_), Kind::Ping) => self.send(&[Frame::new(Kind::Pong, 0, frame.body)]),
            (Some(_), Kind::Credit) => self.credit(&frame),
            (Some(_), Kind::GoAway) => self.finish(input),
            (Some(_), kind) => self.go_away(input, "protocol", &format!("a {kind:?} from a client")),
        }
    }

    /// Answers HELLO with what the connection agrees: the protocol, and the
    /// smallest of the bodies either side takes and the client's stream
    /// credit, so that one body always fits a stream's window.
    fn welcome(&self, input: &mut Input, body: &[u8]) {
        let hello = match Hello::decode(body) {
            Ok(hello) => hello,
            Err(failure) => return self.go_away(input, "protocol", &failure.message),
        };
        if hello.protocol != PROTOCOL {
            let refused = format!("protocol {}, where this server speaks {PROTOCOL}", hello.protocol);
            return self.go_away(input, "protocol", &refused);
        }
        if let Some(length) = hello.challenge.as_ref().map(Vec::len).filter(|length| *length != CHALLENGE) {
            let refused = format!("a challenge of {length} bytes, not {CHALLENGE}");
            return self.go_away(input, "protocol", &refused);
        }
        let capability = match &self.connect.admit {
            None => Capability::Admin,
            Some(admit) => match admit(hello.token.as_deref()) {
                Some(capability) => capability,
                None => return self.go_away(input, "unauthenticated", "no token this server knows"),
            },
        };
        input.capability = Some(capability);
        self.read_only.store(capability == Capability::Read, Ordering::Relaxed);
        let client_most = hello.max_body.unwrap_or(MAX_BODY);
        let stream_credit = hello.stream_credit.unwrap_or(CLIENT_STREAM_CREDIT);
        let max_body = MAX_BODY.min(client_most).min(stream_credit);
        input.max_body = Some(max_body as usize);
        self.agreed.store(max_body as usize, Ordering::Release);
        input.stream_credit = stream_credit;
        let proof = hello.challenge.zip(self.connect.prove.as_ref()).map(|(challenge, prove)| prove(&challenge));
        self.send(&[Frame::new(Kind::Welcome, 0, self.welcome_body(max_body, capability, proof))]);
        if let Some(greeted) = &self.connect.greeted {
            greeted(&hello.client);
        }
    }

    fn welcome_body(&self, max_body: u64, capability: Capability, proof: Option<Vec<u8>>) -> Vec<u8> {
        let capability = match capability {
            Capability::Admin => "admin",
            Capability::Data => "data",
            Capability::Read => "read",
        };
        let welcome = Welcome {
            protocol: PROTOCOL,
            server: env!("CARGO_PKG_VERSION").to_owned(),
            instance: instance(),
            capability: capability.to_owned(),
            max_body,
            in_flight: IN_FLIGHT,
            connection_credit: CONNECTION_CREDIT,
            stream_credit: STREAM_CREDIT,
            engines: engines(),
            now: unix_millis(self.store.now()),
            proof,
            durability: self.store.durability().map(|durability| durability.to_string()),
        };
        welcome.encode()
    }

    /// Answers a call as its method's route says: a point read before this
    /// returns, a write once its group commits, a run of once handed over,
    /// anything else on a worker.
    fn request(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        // A body past the connection's credit, a number in use, or one past
        // the streams WELCOME allowed, is the client's fault, and ends the
        // connection as the protocol says.
        let opened = self.receive(frame.body.len() as u64).and_then(|()| self.open_stream(frame.stream, &frame.body));
        if let Err(refused) = opened {
            return self.go_away(input, "protocol", &refused);
        }
        if input.going_away {
            let closing = Failure::unavailable("the server is closing; ask another connection");
            return self.answer(frame.stream, Err(closing));
        }
        if let Some(refused) = refused_to(input.capability, frame.method) {
            return self.answer(frame.stream, Err(Failure::permission(refused)));
        }
        if frame.method >> 8 == 0x00 {
            return self.server_call(frame.stream, frame.method, &frame.body);
        }
        self.route_call(input, frame);
    }

    /// Runs an admitted call where its method's route says.
    fn route_call(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        let max_body = input.max_body.unwrap_or(HELLO_MOST);
        let (method, stream, body) = (frame.method, frame.stream, frame.body);
        if self.store.probe().listening() {
            self.store.probe().count("tinystore_wire_requests_total", &[("method", method_name(method))], 1.0);
        }
        // A point read is answered where it is read, in less time than
        // another thread takes to wake, while its host waits for nothing
        // else. A crowd of them would each wait its turn on this thread,
        // which is all a host with one thread has: a crowd goes to the
        // workers, where sixteen run together, and so does a read that comes
        // while they still have reads of this connection, whose answers the
        // host comes back for anyway.
        let route = route(method);
        if route == Route::Inline && (input.crowd || self.reads_away.load(Ordering::Acquire) > 0) {
            return self.read_away(stream, method, body, max_body);
        }
        match route {
            Route::Inline => self.answer(stream, guarded(|| self.call(method, &body, max_body))),
            Route::Submit => self.submit(stream, method, body),
            Route::Handover => self.hand_over(stream, body),
            #[cfg(feature = "jobs")]
            Route::Exchange => self.exchange(input, stream, &body),
            #[cfg(feature = "jobs")]
            Route::Watch => self.watch(input, stream, &body),
            #[cfg(feature = "sql")]
            Route::Download => self.download(input, stream, method, body),
            #[cfg(feature = "sql")]
            Route::Transaction => self.transaction(input, stream, &body),
            #[cfg(feature = "blobs")]
            Route::Read => self.read(input, stream, body),
            #[cfg(feature = "blobs")]
            Route::Upload => self.upload(input, stream, &body),
            #[cfg(feature = "records")]
            Route::Tail => self.tail(input, stream, &body),
            #[cfg(feature = "records")]
            Route::Lines => self.lines(input, stream, &body),
            #[cfg(feature = "metrics")]
            Route::Parts => self.parts(input, stream, method, body),
            #[cfg(feature = "metrics")]
            Route::Instruments => self.instruments(input, stream, &body),
            #[cfg(feature = "config")]
            Route::Follow => self.follow(input, stream, &body),
            #[cfg(feature = "backup")]
            Route::Backup => self.backup(input, stream, &body),
            #[cfg(feature = "locks")]
            Route::Hold => self.hold(stream, &body),
            #[cfg(feature = "channels")]
            Route::Subscribe => self.subscribe(input, stream, &body),
            Route::Worker => {
                let (shared, reading) = (Arc::clone(self), self.reading(stream, method));
                self.workers.run(Box::new(move || {
                    let answered =
                        shared.read_under(stream, reading, || guarded(|| shared.call(method, &body, max_body)));
                    shared.answer(stream, answered);
                }));
            }
        }
    }

    /// Hands a write to its engine on a worker, which answers once its group
    /// commits.
    fn submit(self: &Arc<Self>, stream: u32, method: u16, body: Vec<u8>) {
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            let answering = Arc::clone(&shared);
            let done = move |answered| answering.answer(stream, answered);
            let queued = guarded(|| {
                match method >> 8 {
                    #[cfg(feature = "jobs")]
                    0x02 => jobs::submit(&shared.jobs, method, &body, done),
                    #[cfg(feature = "sql")]
                    0x03 => sql::submit(&shared.sql, method, &body, done),
                    _ => kv::submit(&shared.kv, method, &body, done),
                }
                Ok(())
            });
            if let Err(failure) = queued {
                shared.answer(stream, Err(failure));
            }
        }));
    }

    /// Asks for a once's key on a worker, the run handed to the client when
    /// it is its to run.
    fn hand_over(self: &Arc<Self>, stream: u32, body: Vec<u8>) {
        let asked = self.asked.fetch_add(1, Ordering::Relaxed);
        self.lock_runs().insert(stream, ClientRun::Asking(asked));
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || shared.run_once(stream, asked, body, None)));
    }

    /// Answers a point read on a worker, counted as away until it is.
    fn read_away(self: &Arc<Self>, stream: u32, method: u16, body: Vec<u8>, max_body: usize) {
        self.reads_away.fetch_add(1, Ordering::AcqRel);
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            let answered = guarded(|| shared.call(method, &body, max_body));
            shared.reads_away.fetch_sub(1, Ordering::AcqRel);
            shared.answer(stream, answered);
        }));
    }

    /// Registers a subscription: the stream's RESPONSE once the store has it,
    /// then its batches as DATA within the client's credit.
    #[cfg(feature = "channels")]
    fn subscribe(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| channels::subscribe(&self.store, &self.channels, stream, body, link)) {
            Ok(subscribed) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                subscribed.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Asks for the key a hold names; the stream's RESPONSE goes once the key
    /// is given, from whichever thread lets it go.
    #[cfg(feature = "locks")]
    fn hold(self: &Arc<Self>, stream: u32, body: &[u8]) {
        let answering = Arc::downgrade(self);
        let answer: locks::Answering = Box::new(move |answered| {
            if let Some(shared) = answering.upgrade() {
                shared.answer(stream, answered);
            }
        });
        if let Err(failure) = guarded(|| locks::hold(&self.store, &self.locks, stream, body, answer)) {
            self.locks.cancel(stream);
            self.answer(stream, Err(failure));
        }
    }

    /// Starts a client's worker: the stream's `RESPONSE`, then the jobs it
    /// hands the client and the client's answers, as DATA both ways.
    #[cfg(feature = "jobs")]
    fn exchange(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        match guarded(|| jobs::work(&self.jobs, stream, body, self.link(input, stream))) {
            Ok(work) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                work.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's watch of a job: the stream's `RESPONSE`, then a DATA
    /// each time the job changes, until it ends.
    #[cfg(feature = "jobs")]
    fn watch(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        match guarded(|| jobs::watch(&self.jobs, stream, body, self.link(input, stream))) {
            Ok(watch) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                watch.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// What a stream the server sends DATA on needs of the session.
    #[cfg(feature = "jobs")]
    fn link(self: &Arc<Self>, input: &Input, stream: u32) -> jobs::Link {
        let (sending, finishing, spawning) = (Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self));
        let send: jobs::Sender = Arc::new(move |body| {
            if let Some(shared) = sending.upgrade() {
                shared.send(&[Frame::new(Kind::Data, stream, body)]);
            }
        });
        let finish: jobs::Finisher = Arc::new(move |last| {
            if let Some(shared) = finishing.upgrade() {
                shared.end_stream(stream, last);
            }
        });
        let spawn: jobs::Spawn = Arc::new(move |task| {
            if let Some(shared) = spawning.upgrade() {
                shared.workers.run(task);
            }
        });
        let max_body = input.max_body.unwrap_or(HELLO_MOST);
        jobs::Link { send, finish, spawn, credit: input.stream_credit, max_body }
    }

    /// Answers a query on a worker: its rows in the RESPONSE when they fit
    /// one message, else a RESPONSE and their parts as DATA within the
    /// client's credit.
    #[cfg(feature = "sql")]
    fn download(self: &Arc<Self>, input: &Input, stream: u32, called: u16, body: Vec<u8>) {
        let link = self.sql_link(input, stream);
        let (shared, reading) = (Arc::clone(self), self.reading(stream, called));
        self.workers.run(Box::new(move || {
            let (answering, sending) = (Arc::clone(&shared), link.clone());
            let done = move |queried: std::result::Result<sql::Queried, Failure>| match queried {
                Ok(sql::Queried::Whole(rows)) => answering.answer(stream, Ok(rows)),
                Ok(sql::Queried::Parts(parts, held)) => {
                    answering.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                    sql::download(&answering.sql, stream, (parts, held), &sending);
                }
                Err(failure) => answering.answer(stream, Err(failure)),
            };
            let queued = shared.read_under(stream, reading, || {
                guarded(|| {
                    let read_only = shared.read_only.load(Ordering::Relaxed);
                    sql::query(&shared.sql, (called, &body), &link, read_only, done);
                    Ok(())
                })
            });
            if let Err(failure) = queued {
                shared.answer(stream, Err(failure));
            }
        }));
    }

    /// Opens a client's transaction: the stream's RESPONSE, then, in its turn
    /// on its database's line, a worker that holds the writer while the
    /// client's calls come as DATA.
    #[cfg(feature = "sql")]
    fn transaction(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.sql_link(input, stream);
        match guarded(|| sql::transaction(&self.sql, stream, body, &link)) {
            Ok((txing, running)) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                let (shared, line) = (Arc::clone(self), running.line());
                let turn = Box::new(move || {
                    let nested = |tx: &Transaction<'_>, method, body: &[u8], max_body| {
                        shared.call_in(tx, method, body, max_body)
                    };
                    txing.run(running, &shared.sql, stream, &nested);
                });
                self.workers.run_in_line(&line, turn);
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Answers a read of a file's bytes on a worker: its first piece in the
    /// RESPONSE, and the rest as pieces within the client's credit.
    #[cfg(feature = "blobs")]
    fn read(self: &Arc<Self>, input: &Input, stream: u32, body: Vec<u8>) {
        let link = self.stream_link(input, stream);
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || match guarded(|| blobs::read(&shared.blobs, stream, &body, &link)) {
            Ok(blobs::Reading::Whole(response)) => shared.answer(stream, Ok(response)),
            Ok(blobs::Reading::Begun(response, getting)) => {
                shared.queue_moved(Frame::new(Kind::Response, stream, response), None);
                getting.pump();
            }
            Err(failure) => shared.answer(stream, Err(failure)),
        }));
    }

    /// Begins an upload: the stream's RESPONSE, then the client's pieces as
    /// DATA, its last publishing the file.
    #[cfg(feature = "blobs")]
    fn upload(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| blobs::upload(&self.blobs, stream, body, &link)) {
            Ok(()) => self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's tail: the stream's RESPONSE, then a DATA each time
    /// events arrive that its query finds.
    #[cfg(feature = "records")]
    fn tail(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| records::tail(&self.store, &self.records, stream, body, link)) {
            Ok(tailing) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                tailing.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Begins a client's writer of lines: the stream's RESPONSE, then its
    /// pieces as DATA, the last answered with what the writer dropped.
    #[cfg(feature = "records")]
    fn lines(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| records::lines(&self.records, stream, body, link)) {
            Ok(()) => self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Answers a read, the newest samples or an aggregate on a worker: one
    /// part in the RESPONSE, or the RESPONSE and then the parts within the
    /// client's credit, half of what it takes at once each.
    #[cfg(feature = "metrics")]
    fn parts(self: &Arc<Self>, input: &Input, stream: u32, called: u16, body: Vec<u8>) {
        let link = self.stream_link(input, stream);
        let (shared, reading) = (Arc::clone(self), self.reading(stream, called));
        self.workers.run(Box::new(move || {
            let bound = super::part_bound(link.max_body, link.credit, 0);
            let parted =
                shared.read_under(stream, reading, || guarded(|| metrics::parts(&shared.store, called, &body, bound)));
            match parted {
                Ok(mut parts) if parts.len() == 1 => shared.answer(stream, Ok(parts.pop_front().unwrap_or_default())),
                Ok(parts) => {
                    shared.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                    metrics::download(&shared.metrics, stream, parts, link);
                }
                Err(failure) => shared.answer(stream, Err(failure)),
            }
        }));
    }

    /// Opens a client's instruments' stream: the RESPONSE says what the
    /// store knows of its session, then its batches come as DATA, each
    /// answered with one.
    #[cfg(feature = "metrics")]
    fn instruments(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| metrics::instruments(&self.store, &self.metrics, stream, body, link)) {
            Ok(response) => self.send(&[Frame::new(Kind::Response, stream, response)]),
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a client's follow of a config: the stream's RESPONSE, then a
    /// DATA with its value now and one each time it changes.
    #[cfg(feature = "config")]
    fn follow(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| config::follow(&self.config, stream, body, link)) {
            Ok(following) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                following.begin();
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// Starts a backup of the store: the stream's RESPONSE, then its zip in
    /// pieces within the client's credit, then what it holds.
    #[cfg(feature = "backup")]
    fn backup(self: &Arc<Self>, input: &Input, stream: u32, body: &[u8]) {
        let link = self.stream_link(input, stream);
        match guarded(|| backup::asked(&self.backups, stream, body, link)) {
            Ok(sending) => {
                self.send(&[Frame::new(Kind::Response, stream, Empty {}.encode())]);
                sending.begin(&self.store, &self.backups);
            }
            Err(failure) => self.answer(stream, Err(failure)),
        }
    }

    /// What a stream of a file's bytes, a tail's batches, a writer's lines, a
    /// read's parts, a client's instruments, a config's values or a backup's
    /// zip needs of the session.
    #[cfg(any(
        feature = "blobs",
        feature = "records",
        feature = "metrics",
        feature = "config",
        feature = "backup",
        feature = "channels"
    ))]
    fn stream_link(self: &Arc<Self>, input: &Input, stream: u32) -> super::Link {
        let (sending, finishing, granting, spawning) =
            (Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self), Arc::downgrade(self));
        super::Link {
            send: Arc::new(move |body| {
                if let Some(shared) = sending.upgrade() {
                    shared.queue_moved(Frame::new(Kind::Data, stream, body), None);
                }
            }),
            finish: Arc::new(move |last| {
                if let Some(shared) = finishing.upgrade() {
                    shared.end_stream(stream, last);
                }
            }),
            grant: Arc::new(move |bytes: u32| {
                if let Some(shared) = granting.upgrade() {
                    shared.send(&[Frame::new(Kind::Credit, stream, bytes.to_le_bytes().to_vec())]);
                }
            }),
            spawn: Arc::new(move |task| {
                if let Some(shared) = spawning.upgrade() {
                    shared.workers.run(task);
                }
            }),
            credit: input.stream_credit,
            max_body: input.max_body.unwrap_or(HELLO_MOST),
            #[cfg(feature = "channels")]
            owed: {
                let owing = Arc::downgrade(self);
                Arc::new(move || owing.upgrade().map_or(0, |shared| shared.owed()))
            },
            #[cfg(feature = "channels")]
            when_drained: {
                let draining = Arc::downgrade(self);
                Arc::new(move |task| {
                    if let Some(shared) = draining.upgrade() {
                        shared.when_drained(task);
                    }
                })
            },
        }
    }

    /// What an sql stream that sends DATA needs of the session.
    #[cfg(feature = "sql")]
    fn sql_link(self: &Arc<Self>, input: &Input, stream: u32) -> sql::Link {
        let (sending, finishing) = (Arc::downgrade(self), Arc::downgrade(self));
        sql::Link {
            send: Arc::new(move |body| {
                if let Some(shared) = sending.upgrade() {
                    shared.send(&[Frame::new(Kind::Data, stream, body)]);
                }
            }),
            finish: Arc::new(move |last| {
                if let Some(shared) = finishing.upgrade() {
                    shared.end_stream(stream, last);
                }
            }),
            credit: input.stream_credit,
            max_body: input.max_body.unwrap_or(HELLO_MOST),
        }
    }

    /// Starts a client's run of a once key: its kept answer ends the stream, a
    /// run handed over waits for the client's last DATA, and a run held
    /// elsewhere asks again once it ends, unless it ends with an answer it
    /// could not keep, which is this client's. A run cancelled meanwhile goes
    /// back.
    fn run_once(self: &Arc<Self>, stream: u32, asked: u64, body: Vec<u8>, unkept: Option<Arc<crate::kv::Raw>>) {
        if !still_asking(&self.lock_runs(), stream, asked) {
            return;
        }
        let started = match unkept {
            Some(raw) => Ok(kv::unkept(&raw)),
            None => {
                let again = {
                    let (shared, body) = (Arc::clone(self), body.clone());
                    Box::new(move |unkept| {
                        let asking = Arc::clone(&shared);
                        shared.workers.run(Box::new(move || asking.run_once(stream, asked, body, unkept)));
                    })
                };
                catch_unwind(AssertUnwindSafe(|| kv::run(&self.kv, &body, again)))
                    .unwrap_or_else(|_| Err(Failure::internal("a call panicked")))
            }
        };
        // the answer leaves under the lock, so that a CANCEL either finds the
        // run or comes after this stream's frames
        let mut runs = self.lock_runs();
        if !still_asking(&runs, stream, asked) {
            drop(runs);
            drop(started);
            return;
        }
        match started {
            Ok(Run::Answered(answer)) => {
                runs.remove(&stream);
                self.answer(stream, Ok(answer));
            }
            Ok(Run::Handed(handed, answer)) => {
                runs.insert(stream, ClientRun::Handed(*handed));
                self.send(&[Frame::new(Kind::Response, stream, answer)]);
            }
            Ok(Run::Waiting) => {}
            Err(failure) => {
                runs.remove(&stream);
                self.answer(stream, Err(failure));
            }
        }
    }

    /// A client's DATA: an item of its worker's stream, or the last of a run
    /// handed over, its answer to keep. A DATA of a stream no longer in use is
    /// dropped, its credit still given back.
    fn data(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        let bytes = frame.body.len() as u64;
        if let Err(refused) = self.receive(bytes).and_then(|()| self.spend_stream_credit(frame.stream, bytes)) {
            return self.go_away(input, "protocol", &refused);
        }
        #[cfg(feature = "blobs")]
        if let Some(uploading) = self.blobs.uploading(frame.stream) {
            let last = frame.flags & frame::END != 0;
            return uploading.piece(frame.body, last);
        }
        #[cfg(feature = "records")]
        if self.records.piece(frame.stream, &frame.body, frame.flags & frame::END != 0) {
            return;
        }
        #[cfg(feature = "metrics")]
        if self.metrics.batch(frame.stream, &frame.body, frame.flags & frame::END != 0) {
            return;
        }
        #[cfg(feature = "sql")]
        if let Some(tx) = self.sql.tx(frame.stream) {
            // a call is read once it is queued: its credit goes back at once
            if let Ok(read) = u32::try_from(frame.body.len()) {
                self.send(&[Frame::new(Kind::Credit, frame.stream, read.to_le_bytes().to_vec())]);
            }
            return tx.item(&frame.body, frame.flags & frame::END != 0);
        }
        #[cfg(feature = "jobs")]
        if let Some(work) = self.jobs.work(frame.stream) {
            return self.work_data(&work, &frame);
        }
        if frame.flags & frame::END == 0 {
            return;
        }
        let handed = {
            let mut runs = self.lock_runs();
            match runs.remove(&frame.stream) {
                Some(ClientRun::Handed(handed)) => handed,
                Some(asking) => {
                    runs.insert(frame.stream, asking);
                    return;
                }
                None => return,
            }
        };
        let (shared, stream, body) = (Arc::clone(self), frame.stream, frame.body);
        self.workers.run(Box::new(move || {
            let kept = guarded(|| kv::kept(handed, &body));
            shared.end_stream(stream, kept);
        }));
    }

    /// A client's item on its worker's stream: an answer, a progress or a
    /// stop. Its END ends the worker, and the server's DATA·END follows once
    /// what was answered is written; an item that is not one fails the stream.
    #[cfg(feature = "jobs")]
    fn work_data(self: &Arc<Self>, work: &jobs::Work, frame: &Frame) {
        if let Some(granted) = work.release(frame.body.len() as u64, STREAM_CREDIT) {
            self.send(&[Frame::new(Kind::Credit, frame.stream, granted.to_le_bytes().to_vec())]);
        }
        let ended = frame.flags & frame::END != 0;
        let item = if ended && frame.body.is_empty() { Ok(()) } else { guarded(|| work.item(&frame.body)) };
        match (item, ended) {
            (Ok(()), false) => {}
            (Ok(()), true) => self.end_work(frame.stream, Ok(Empty {}.encode())),
            (Err(failure), _) => self.end_work(frame.stream, Err(failure)),
        }
    }

    /// Ends a client's worker off this thread, and its stream with `last` once
    /// the worker wrote what was answered.
    #[cfg(feature = "jobs")]
    fn end_work(self: &Arc<Self>, stream: u32, last: std::result::Result<Vec<u8>, Failure>) {
        let Some(work) = self.jobs.take_work(stream) else {
            return;
        };
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            work.end();
            shared.end_stream(stream, last);
        }));
    }

    /// A client's grant of DATA on a stream: a worker's or a watch's sends
    /// what waited for it. No other stream sends DATA a client grants credit
    /// for: a call answers in one message, within the body agreed.
    #[cfg_attr(
        not(any(
            feature = "jobs",
            feature = "sql",
            feature = "blobs",
            feature = "records",
            feature = "metrics",
            feature = "config",
            feature = "backup",
            feature = "channels"
        )),
        expect(
            unused_variables,
            reason = "only the streams of jobs, sql, blobs, records, metrics, config, backup and channels take credit"
        )
    )]
    fn credit(&self, frame: &Frame) {
        let Ok(granted) = <[u8; 4]>::try_from(frame.body.as_slice()) else {
            return;
        };
        let granted = u32::from_le_bytes(granted);
        #[cfg(feature = "jobs")]
        self.jobs.grant(frame.stream, granted);
        #[cfg(feature = "sql")]
        self.sql.grant(frame.stream, granted);
        #[cfg(feature = "blobs")]
        self.blobs.grant(frame.stream, granted);
        #[cfg(feature = "records")]
        self.records.grant(frame.stream, granted);
        #[cfg(feature = "metrics")]
        self.metrics.grant(frame.stream, granted);
        #[cfg(feature = "config")]
        self.config.grant(frame.stream, granted);
        #[cfg(feature = "backup")]
        self.backups.grant(frame.stream, granted);
        #[cfg(feature = "channels")]
        self.channels.grant(frame.stream, granted);
    }

    /// Lets go of a client's run of once, answering `cancelled`. A read under
    /// way stops, its statement interrupted, and answers `cancelled`; a call
    /// that writes runs to its end, short as a call is. A worker cancelled
    /// ends as its END would end it, the jobs it held failing their attempt,
    /// and a watch cancelled ends.
    fn cancel(self: &Arc<Self>, stream: u32) {
        let read = lock(&self.reads).get(&stream).cloned();
        if let Some(read) = read {
            read.cancel();
        }
        #[cfg(feature = "sql")]
        if self.sql.cancel(stream) {
            return;
        }
        #[cfg(feature = "blobs")]
        if self.blobs.cancel(stream) {
            return;
        }
        #[cfg(feature = "records")]
        if self.records.cancel(stream) {
            return;
        }
        #[cfg(feature = "metrics")]
        if self.metrics.cancel(stream) {
            return;
        }
        #[cfg(feature = "config")]
        if self.config.cancel(stream) {
            return;
        }
        #[cfg(feature = "backup")]
        if self.backups.cancel(stream) {
            return;
        }
        #[cfg(feature = "channels")]
        if self.channels.cancel(stream) {
            return;
        }
        #[cfg(feature = "locks")]
        if self.locks.cancel(stream) {
            return self.answer(stream, Err(Failure::cancelled("the client cancelled its hold")));
        }
        #[cfg(feature = "jobs")]
        if self.jobs.work(stream).is_some() {
            return self.end_work(stream, Err(Failure::cancelled("the client cancelled its worker")));
        }
        #[cfg(feature = "jobs")]
        if let Some(watch) = self.jobs.take_watch(stream) {
            return watch.close(Err(Failure::cancelled("the client cancelled its watch")));
        }
        let mut runs = self.lock_runs();
        let cancelled = Failure::cancelled("the client cancelled the run");
        match runs.remove(&stream) {
            Some(ClientRun::Asking(_)) => self.answer(stream, Err(cancelled)),
            Some(ClientRun::Handed(handed)) => {
                self.end_stream(stream, Err(cancelled));
                drop(runs);
                drop(handed);
            }
            None => {}
        }
    }

    /// The token of a read a worker runs for `stream`, which a CANCEL of the
    /// stream cancels; none for a call that writes, which runs to its end.
    fn reading(&self, stream: u32, method: u16) -> Option<CancellationToken> {
        if method::access(method) != Some(Access::Read) {
            return None;
        }
        let token = CancellationToken::new();
        lock(&self.reads).insert(stream, token.clone());
        Some(token)
    }

    /// Runs `read` under its stream's token, then lets the token go, unless a
    /// REQUEST that named the stream's number again has put its own there.
    fn read_under<T>(&self, stream: u32, token: Option<CancellationToken>, read: impl FnOnce() -> T) -> T {
        let Some(token) = token else { return read() };
        let answered = token.run(read);
        let mut reads = lock(&self.reads);
        if reads.get(&stream).is_some_and(|kept| kept.same(&token)) {
            reads.remove(&stream);
        }
        answered
    }

    /// The server's own methods, answered before this returns: a stop once its
    /// answer has left, so that the client hears it before the GOAWAY.
    fn server_call(&self, stream: u32, called: u16, body: &[u8]) {
        match called {
            method::SERVER_STOP => match self.stopping(body) {
                Ok(stop) => {
                    self.answer(stream, Ok(Empty {}.encode()));
                    stop();
                }
                Err(failure) => self.answer(stream, Err(failure)),
            },
            method::SERVER_CLOCK => self.answer(stream, guarded(|| self.clock(body))),
            other => self.answer(stream, Err(Failure::unimplemented(format!("method {other:#06x}")))),
        }
    }

    fn stopping(&self, body: &[u8]) -> std::result::Result<crate::pipe::Stop, Failure> {
        Empty::decode(body)?;
        let refused = || Failure::permission("this store's program decides when its server stops");
        self.connect.stop.clone().ok_or_else(refused)
    }

    /// Reads a private server's clock, set to a time or moved forward first.
    fn clock(&self, body: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
        let asked = ServerClock::decode(body)?;
        let Some(clock) = &self.connect.clock else {
            return Err(Failure::invalid("a clock call to a server that runs on the system's clock"));
        };
        match (asked.at, asked.advance) {
            (Some(_), Some(_)) => return Err(Failure::invalid("a clock set and moved in one call")),
            (Some(at), None) => clock.set(from_unix_millis(at))?,
            (None, Some(advance)) => clock.advance(Duration::from_millis(advance)),
            (None, None) => return Ok(ServerClock { at: Some(unix_millis(clock.now())), advance: None }.encode()),
        }
        self.store.clock_moved();
        Ok(ServerClock { at: Some(unix_millis(clock.now())), advance: None }.encode())
    }

    /// Ends a stream with its RESPONSE.
    fn answer(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let response = match self.within_agreed(answered) {
            Ok(body) => Frame::new(Kind::Response, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Response, stream, failure.encode()).failing(),
        };
        self.send_last(response);
    }

    /// Ends a stream whose RESPONSE went out already, with its last DATA.
    fn end_stream(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let data = match self.within_agreed(answered) {
            Ok(body) => Frame::new(Kind::Data, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Data, stream, failure.encode()).failing(),
        };
        self.send_last(data);
    }

    fn call(&self, method: u16, body: &[u8], max_body: usize) -> std::result::Result<Vec<u8>, Failure> {
        let started = self.store.probe().start();
        let answered = self.call_engine(method, body, max_body);
        if let Some(started) = started {
            self.store.probe().time("tinystore_wire_call_ms", &[("method", method_name(method))], started.elapsed());
        }
        answered
    }

    fn call_engine(&self, method: u16, body: &[u8], max_body: usize) -> std::result::Result<Vec<u8>, Failure> {
        let lent = |handle| self.lent(handle);
        match method >> 8 {
            0x01 => {
                let read_only = self.read_only.load(Ordering::Relaxed);
                kv::call(&self.store, &self.kv, &lent, method, body, (max_body, read_only))
            }
            #[cfg(feature = "jobs")]
            0x02 => jobs::call(&self.store, &self.jobs, &lent, method, body, max_body),
            #[cfg(feature = "sql")]
            0x03 => sql::call(&self.store, &self.sql, method, body, self.read_only.load(Ordering::Relaxed)),
            #[cfg(feature = "blobs")]
            0x04 => blobs::call(&self.store, &self.blobs, method, body, max_body),
            #[cfg(feature = "records")]
            0x05 => records::call(&self.store, &self.records, method, body, self.read_only.load(Ordering::Relaxed)),
            #[cfg(feature = "metrics")]
            0x06 => metrics::call(&self.store, method, body, max_body),
            #[cfg(feature = "config")]
            0x07 => config::call(&self.store, &self.config, method, body, self.read_only.load(Ordering::Relaxed)),
            #[cfg(feature = "locks")]
            0x09 => locks::call(&self.store, method, body),
            #[cfg(feature = "channels")]
            0x0a => channels::call(&self.store, method, body),
            #[cfg(feature = "search")]
            0x0b => {
                let read_only = self.read_only.load(Ordering::Relaxed);
                search::call(&self.store, &self.search, &lent, method, body, read_only)
            }
            _ => Err(Failure::unimplemented(format!("method {method:#06x}"))),
        }
    }

    /// The file of a database this connection opened, for a bucket or a
    /// queue opened in it.
    fn lent(&self, handle: u64) -> std::result::Result<Arc<SharedFile>, Failure> {
        #[cfg(feature = "sql")]
        return self.sql.shared(handle);
        #[cfg(not(feature = "sql"))]
        Err(Failure::invalid(format!("database {handle}: this server keeps no databases")))
    }

    /// Answers a call of kv or jobs made inside a database's transaction.
    #[cfg(feature = "sql")]
    fn call_in(
        &self,
        tx: &Transaction<'_>,
        method: u16,
        body: &[u8],
        max_body: usize,
    ) -> std::result::Result<Vec<u8>, Failure> {
        match method >> 8 {
            0x01 => kv::call_in(&self.kv, tx, method, body, max_body),
            #[cfg(feature = "jobs")]
            0x02 => jobs::call_in(&self.jobs, tx, method, body, max_body),
            #[cfg(feature = "channels")]
            0x0a => channels::call_in(&self.store, tx, method, body),
            _ => Err(Failure::invalid(format!("method {method:#06x} does not run inside a transaction"))),
        }
    }

    fn go_away(&self, input: &mut Input, code: &str, message: &str) {
        let body = GoAway { code: code.to_owned(), message: message.to_owned() }.encode();
        self.send(&[Frame::new(Kind::GoAway, 0, body)]);
        self.finish(input);
    }

    /// Reads nothing more; what is owed already stays for the host to take.
    fn finish(&self, input: &mut Input) {
        input.ended = true;
        self.lock_output().ended = true;
        self.ready.notify_all();
    }

    fn send(&self, frames: &[Frame]) {
        self.queue(frames, None);
    }

    /// Runs what waited for the output once it owes less than its low mark.
    fn drained(&self) {
        #[cfg(feature = "channels")]
        {
            let mut waiting = self.waiting_output.lock().unwrap_or_else(PoisonError::into_inner);
            if waiting.is_empty() || self.owed() > OUTPUT_LOW {
                return;
            }
            for task in std::mem::take(&mut *waiting) {
                self.workers.run(task);
            }
        }
    }

    /// The bytes queued for the client and not yet taken by the host.
    #[cfg(feature = "channels")]
    fn owed(&self) -> usize {
        self.lock_output().owed()
    }

    /// Runs `task` once the output drains; at once when it has already.
    #[cfg(feature = "channels")]
    fn when_drained(&self, task: Box<dyn FnOnce() + Send>) {
        self.waiting_output.lock().unwrap_or_else(PoisonError::into_inner).push(task);
        self.drained();
    }

    /// Sends a stream's last frame.
    fn send_last(&self, frame: Frame) {
        let stream = frame.stream;
        self.queue_moved(frame, Some(stream));
    }

    /// Takes a stream number into use, refusing one in use already or one
    /// past the streams in flight WELCOME allowed.
    fn open_stream(&self, stream: u32, body: &[u8]) -> std::result::Result<(), String> {
        let mut open = self.lock_open();
        if open.contains_key(&stream) {
            return Err(format!("a REQUEST on stream {stream}, which is in use"));
        }
        if open.len() >= IN_FLIGHT as usize {
            return Err(format!("a REQUEST past the {IN_FLIGHT} streams in flight WELCOME allowed"));
        }
        open.insert(stream, OpenStream { left: STREAM_CREDIT, held: body.len() as u64 });
        Ok(())
    }

    /// Counts a body against the connection's credit.
    fn receive(&self, bytes: u64) -> std::result::Result<(), String> {
        let outstanding = self.outstanding.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if outstanding > CONNECTION_CREDIT {
            return Err(format!("{outstanding} bytes sent past the connection's credit of {CONNECTION_CREDIT}"));
        }
        Ok(())
    }

    /// Counts a DATA body against its stream's credit; one past it is the
    /// client's fault. A DATA of a stream no longer in use is dropped, as the
    /// protocol says, and counts against nothing.
    fn spend_stream_credit(&self, stream: u32, bytes: u64) -> std::result::Result<(), String> {
        let mut open = self.lock_open();
        let Some(opened) = open.get_mut(&stream) else {
            drop(open);
            self.let_go(bytes);
            self.grant_when_due();
            return Ok(());
        };
        opened.left =
            opened.left.checked_sub(bytes).ok_or_else(|| format!("a DATA on stream {stream} past its credit"))?;
        opened.held += bytes;
        Ok(())
    }

    /// Bytes the server let go of, which the next grant gives back.
    fn let_go(&self, bytes: u64) {
        self.to_grant.fetch_add(bytes, Ordering::AcqRel);
    }

    /// Grants the connection's credit back once half its window gathered.
    fn grant_when_due(&self) {
        let gathered = self.to_grant.load(Ordering::Acquire);
        if gathered < CONNECTION_CREDIT / 2 {
            return;
        }
        let granted = self.to_grant.swap(0, Ordering::AcqRel);
        if granted == 0 {
            return;
        }
        self.outstanding.fetch_sub(granted, Ordering::AcqRel);
        let granted = u32::try_from(granted).unwrap_or(u32::MAX);
        self.send(&[Frame::new(Kind::Credit, 0, granted.to_le_bytes().to_vec())]);
    }

    /// A body as it may go out: one past the agreed body is `limit`, which a
    /// client takes, rather than a frame it would end the connection over.
    fn within_agreed(&self, answered: std::result::Result<Vec<u8>, Failure>) -> std::result::Result<Vec<u8>, Failure> {
        let agreed = self.agreed.load(Ordering::Acquire);
        match answered {
            Ok(body) if body.len() > agreed => Err(Failure {
                code: "limit".to_owned(),
                message: format!("an answer of {} bytes, past the {agreed} a body holds", body.len()),
                what: None,
            }),
            answered => answered,
        }
    }

    fn lock_open(&self) -> MutexGuard<'_, HashMap<u32, OpenStream>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues frames for the host to take, `ended` of them their streams'
    /// last. Those streams leave the count under the output's lock, after
    /// their frames are queued: whoever read a stream's last frame finds
    /// `streams` without it, and whoever finds no stream finds every last
    /// frame queued. Counted after the lock instead, a client that had read
    /// its answer could still see its stream in flight.
    fn queue(&self, frames: &[Frame], ended: Option<u32>) {
        let mut output = self.lock_output();
        let dropped = output.ended && frames.iter().all(|frame| frame.kind != Kind::GoAway);
        if !dropped {
            for frame in frames {
                output.copy(frame);
            }
        }
        self.queued(output, dropped, frames, ended);
    }

    /// Queues one frame as `queue` does, a long body moved behind what is
    /// owed rather than copied: a file's mebibyte is written once into the
    /// message that carries it, and once more into the host's bytes.
    fn queue_moved(&self, frame: Frame, ended: Option<u32>) {
        debug_assert!(frame.kind != Kind::Credit, "a grant is counted where it is queued");
        let mut output = self.lock_output();
        let dropped = output.ended && frame.kind != Kind::GoAway;
        if !dropped {
            output.push(frame);
        }
        self.queued(output, dropped, &[], ended);
    }

    /// What follows the frames' queueing, under the output's lock: their
    /// streams' grants and ends counted, and the host woken.
    fn queued(&self, mut output: MutexGuard<'_, Output>, dropped: bool, frames: &[Frame], ended: Option<u32>) {
        let mut let_go = 0;
        {
            let mut open = self.lock_open();
            // a grant of the server's on a stream is room its client may send
            // into, and says the server let go of as many of its bytes
            for frame in frames.iter().filter(|frame| frame.kind == Kind::Credit && frame.stream != 0) {
                if let (Some(opened), Ok(granted)) = (open.get_mut(&frame.stream), <[u8; 4]>::try_from(&frame.body[..]))
                {
                    let granted = u64::from(u32::from_le_bytes(granted));
                    opened.left = opened.left.saturating_add(granted);
                    let released = granted.min(opened.held);
                    opened.held -= released;
                    let_go += released;
                }
            }
            if let Some(opened) = ended.and_then(|stream| open.remove(&stream)) {
                let_go += opened.held;
            }
        }
        if let_go > 0 {
            self.let_go(let_go);
        }
        if dropped {
            return;
        }
        let wake = !std::mem::replace(&mut output.woken, true);
        let awaited = output.awaited;
        drop(output);
        if awaited {
            self.ready.notify_all();
        }
        if let (true, Some(wake)) = (wake, &self.connect.wake) {
            wake();
        }
        // the output's lock is let go: a grant goes out as a frame of its own
        if let_go > 0 {
            self.grant_when_due();
        }
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

/// Whether the run asked as `asked` is still the one on its stream.
fn still_asking(runs: &HashMap<u32, ClientRun>, stream: u32, asked: u64) -> bool {
    matches!(runs.get(&stream), Some(ClientRun::Asking(asking)) if *asking == asked)
}

/// Why a connection admitted as `capability` may not call `method`, if it may not.
fn refused_to(capability: Option<Capability>, method: u16) -> Option<&'static str> {
    match method::access(method) {
        Some(Access::Admin) if capability != Some(Capability::Admin) => Some("the server's own calls are an admin's"),
        Some(Access::Write) if capability == Some(Capability::Read) => Some("a read connection writes nothing"),
        _ => None,
    }
}

/// How a method runs: kv's and jobs' say theirs; every other engine's runs on
/// a worker.
fn route(method: u16) -> Route {
    match method >> 8 {
        0x01 => kv::route(method),
        #[cfg(feature = "jobs")]
        0x02 => jobs::route(method),
        #[cfg(feature = "sql")]
        0x03 => sql::route(method),
        #[cfg(feature = "blobs")]
        0x04 => blobs::route(method),
        #[cfg(feature = "records")]
        0x05 => records::route(method),
        #[cfg(feature = "metrics")]
        0x06 => metrics::route(method),
        #[cfg(feature = "config")]
        0x07 => config::route(method),
        #[cfg(feature = "backup")]
        0x08 => Route::Backup,
        #[cfg(feature = "locks")]
        0x09 => locks::route(method),
        #[cfg(feature = "channels")]
        0x0a => channels::route(method),
        _ => Route::Worker,
    }
}

/// The engines this server serves.
fn engines() -> Vec<String> {
    let jobs = cfg!(feature = "jobs").then_some("jobs");
    let sql = cfg!(feature = "sql").then_some("sql");
    let blobs = cfg!(feature = "blobs").then_some("blobs");
    let records = cfg!(feature = "records").then_some("records");
    let metrics = cfg!(feature = "metrics").then_some("metrics");
    let config = cfg!(feature = "config").then_some("config");
    let backup = cfg!(feature = "backup").then_some("backup");
    let locks = cfg!(feature = "locks").then_some("locks");
    let channels = cfg!(feature = "channels").then_some("channels");
    let search = cfg!(feature = "search").then_some("search");
    let engines = [Some("kv"), config, locks, channels, jobs, sql, blobs, records, metrics, search, backup];
    engines.into_iter().flatten().map(str::to_owned).collect()
}

/// A call that panics answers `internal` rather than leaving its stream open.
fn guarded<T>(call: impl FnOnce() -> std::result::Result<T, Failure>) -> std::result::Result<T, Failure> {
    catch_unwind(AssertUnwindSafe(call)).unwrap_or_else(|_| Err(Failure::internal("a call panicked")))
}

/// Sixteen bytes that differ for each session: the process's random hasher
/// keys, over the time and an address.
fn instance() -> Vec<u8> {
    let state = RandomState::new();
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    let mut bytes = Vec::with_capacity(16);
    for half in 0..2u8 {
        let mut hasher = state.build_hasher();
        hasher.write_u8(half);
        hasher.write_u128(now);
        hasher.write_usize(&bytes as *const _ as usize);
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes
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
fn method_name(method: u16) -> &'static str {
    super::protocol::METHODS.iter().find(|(_, id, _)| *id == method).map_or("unknown", |(name, ..)| name)
}
