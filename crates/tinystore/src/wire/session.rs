use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use super::codec::Message;
use super::frame::{self, Frame, Kind};
use super::kv::{self, Route, Run};
use super::protocol::{Empty, Failure, GoAway, Hello, ServerClock, Welcome, method};
use super::workers::Workers;
use crate::clock::from_unix_millis;
use crate::kv::Handed;
use crate::pipe::{Capability, Connect};
use crate::{Clock, Result, Store, unix_millis};

/// The protocol this server speaks, the second: a client of the first is
/// refused, since nothing of it was released.
const PROTOCOL: u64 = 2;
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
    input: Mutex<Input>,
    output: Mutex<Output>,
    /// Clients' runs of once, by stream, from the REQUEST to the last frame.
    runs: Mutex<HashMap<u32, ClientRun>>,
    /// Numbers each run's asking, so that a wait that ends after its stream
    /// was cancelled, and its number named again, finds another run there.
    asked: AtomicU64,
    /// Streams whose final frame has not left, which a closing server waits
    /// for.
    in_flight: AtomicUsize,
    ready: Condvar,
    connect: Connect,
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
    /// REQUEST and DATA bytes read since credit last went back.
    released: u64,
    /// GOAWAY went out: a REQUEST after it is answered `unavailable`, unrun.
    going_away: bool,
    /// What the client may do, once its HELLO was admitted.
    capability: Option<Capability>,
    ended: bool,
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    woken: bool,
    ended: bool,
}

impl Session {
    pub(crate) fn new(store: Store, connect: Connect) -> Result<Session> {
        let workers = Workers::of(&store)?;
        let shared = Shared {
            store,
            workers,
            kv: kv::Handles::default(),
            input: Mutex::new(Input::default()),
            output: Mutex::new(Output::default()),
            runs: Mutex::new(HashMap::new()),
            asked: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            ready: Condvar::new(),
            connect,
        };
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
        std::mem::take(&mut self.shared.lock_output().bytes)
    }

    /// The bytes the client is owed, waiting up to `wait` for the first; the
    /// next bytes ready wake the host again.
    pub(crate) fn take(&self, wait: Duration) -> Vec<u8> {
        let deadline = Instant::now() + wait;
        let mut output = self.shared.lock_output();
        while output.bytes.is_empty() && !output.ended {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            output = self.shared.ready.wait_timeout(output, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        output.woken = false;
        std::mem::take(&mut output.bytes)
    }

    /// Tells the client the server is closing; the streams running finish.
    pub(crate) fn go_away(&self) {
        let mut input = self.shared.lock_input();
        if input.ended || input.going_away {
            return;
        }
        input.going_away = true;
        let body = GoAway { code: "unavailable".to_owned(), message: "the server is closing".to_owned() }.encode();
        self.shared.send(&[Frame::new(Kind::GoAway, 0, body)]);
    }

    pub(crate) fn streams(&self) -> usize {
        self.shared.in_flight.load(Ordering::Acquire)
    }

    pub(crate) fn welcomed(&self) -> bool {
        self.shared.lock_input().max_body.is_some()
    }

    /// Whether the session reads nothing more and owes nothing more.
    pub(crate) fn finished(&self) -> bool {
        let output = self.shared.lock_output();
        output.ended && output.bytes.is_empty()
    }

    /// Ends the session: calls under way finish, what they answer is dropped,
    /// and the runs handed to the client are let go.
    pub(crate) fn end(&self) {
        self.shared.lock_input().ended = true;
        let mut output = self.shared.lock_output();
        output.ended = true;
        output.bytes.clear();
        drop(output);
        self.shared.ready.notify_all();
        let runs = std::mem::take(&mut *self.shared.lock_runs());
        drop(runs);
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
            // calls answer in one message, within the body agreed, and send no DATA a client grants credit for
            (Some(_), Kind::Credit) => {}
            (Some(_), Kind::GoAway) => self.finish(input),
            (Some(_), kind) => self.go_away(input, "protocol", &format!("a {kind:?} from a client")),
        }
    }

    /// Answers HELLO with what the connection agrees: protocol 2, and the
    /// smallest body either side takes.
    fn welcome(&self, input: &mut Input, body: &[u8]) {
        let hello = match Hello::decode(body) {
            Ok(hello) => hello,
            Err(failure) => return self.go_away(input, "protocol", &failure.message),
        };
        if hello.protocol < PROTOCOL {
            let refused = format!("protocol {}, older than {PROTOCOL}, the one this server speaks", hello.protocol);
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
        let client_most = hello.max_body.unwrap_or(MAX_BODY);
        let stream_credit = hello.stream_credit.unwrap_or(CLIENT_STREAM_CREDIT);
        let max_body = MAX_BODY.min(client_most).min(stream_credit);
        input.max_body = Some(max_body as usize);
        let proof = hello.challenge.zip(self.connect.prove.as_ref()).map(|(challenge, prove)| prove(&challenge));
        self.send(&[Frame::new(Kind::Welcome, 0, self.welcome_body(max_body, capability, proof))]);
    }

    fn welcome_body(&self, max_body: u64, capability: Capability, proof: Option<Vec<u8>>) -> Vec<u8> {
        let capability = match capability {
            Capability::Admin => "admin",
            Capability::Data => "data",
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
            engines: vec!["kv".to_owned()],
            now: unix_millis(self.store.now()),
            proof,
        };
        welcome.encode()
    }

    /// Answers a call as its method's route says: a point read before this
    /// returns, a write once its group commits, a run of once handed over,
    /// anything else on a worker.
    fn request(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        self.release(input, frame.body.len() as u64);
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let max_body = input.max_body.unwrap_or(HELLO_MOST);
        let (method, stream, body) = (frame.method, frame.stream, frame.body);
        if input.going_away {
            return self.answer(stream, Err(Failure::unavailable("the server is closing; ask another connection")));
        }
        if method >> 8 == 0x00 {
            if input.capability != Some(Capability::Admin) {
                return self.answer(stream, Err(Failure::permission("the server's own calls are an admin's")));
            }
            return self.server_call(stream, method, &body);
        }
        match route(method) {
            Route::Inline => self.answer(stream, guarded(|| self.call(method, &body, max_body))),
            Route::Submit => {
                let shared = Arc::clone(self);
                self.workers.run(Box::new(move || {
                    let answering = Arc::clone(&shared);
                    let done = move |answered| answering.answer(stream, answered);
                    let queued = guarded(|| {
                        kv::submit(&shared.kv, method, &body, done);
                        Ok(Vec::new())
                    });
                    if let Err(failure) = queued {
                        shared.answer(stream, Err(failure));
                    }
                }));
            }
            Route::Handover => {
                let asked = self.asked.fetch_add(1, Ordering::Relaxed);
                self.lock_runs().insert(stream, ClientRun::Asking(asked));
                let shared = Arc::clone(self);
                self.workers.run(Box::new(move || shared.run_once(stream, asked, body)));
            }
            Route::Worker => {
                let shared = Arc::clone(self);
                self.workers.run(Box::new(move || {
                    shared.answer(stream, guarded(|| shared.call(method, &body, max_body)));
                }));
            }
        }
    }

    /// Starts a client's run of a once key: its kept answer ends the stream, a
    /// run handed over waits for the client's last DATA, and a run held
    /// elsewhere asks again once it ends. A run cancelled meanwhile goes back.
    fn run_once(self: &Arc<Self>, stream: u32, asked: u64, body: Vec<u8>) {
        if !still_asking(&self.lock_runs(), stream, asked) {
            return;
        }
        let again = {
            let (shared, body) = (Arc::clone(self), body.clone());
            Box::new(move || {
                let asking = Arc::clone(&shared);
                shared.workers.run(Box::new(move || asking.run_once(stream, asked, body)));
            })
        };
        let started = catch_unwind(AssertUnwindSafe(|| kv::run(&self.kv, &body, again)))
            .unwrap_or_else(|_| Err(Failure::internal("a call panicked")));
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
                runs.insert(stream, ClientRun::Handed(handed));
                self.send(&[Frame::new(Kind::Response, stream, answer)]);
            }
            Ok(Run::Waiting) => {}
            Err(failure) => {
                runs.remove(&stream);
                self.answer(stream, Err(failure));
            }
        }
    }

    /// A client's DATA: the last of a run handed over, its answer to keep. A
    /// DATA of a stream no longer in use is dropped, its credit still given
    /// back.
    fn data(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        self.release(input, frame.body.len() as u64);
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

    /// Lets go of a client's run of once, answering `cancelled`; a call
    /// cancelled runs to its end, short as a call is.
    fn cancel(&self, stream: u32) {
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
            (None, None) => {}
        }
        Ok(ServerClock { at: Some(unix_millis(clock.now())), advance: None }.encode())
    }

    /// Ends a stream with its RESPONSE.
    fn answer(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let response = match answered {
            Ok(body) => Frame::new(Kind::Response, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Response, stream, failure.encode()).failing(),
        };
        self.send(&[response]);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }

    /// Ends a stream whose RESPONSE went out already, with its last DATA.
    fn end_stream(&self, stream: u32, answered: std::result::Result<Vec<u8>, Failure>) {
        let data = match answered {
            Ok(body) => Frame::new(Kind::Data, stream, body).ending(),
            Err(failure) => Frame::new(Kind::Data, stream, failure.encode()).failing(),
        };
        self.send(&[data]);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }

    fn call(&self, method: u16, body: &[u8], max_body: usize) -> std::result::Result<Vec<u8>, Failure> {
        match method >> 8 {
            0x01 => kv::call(&self.store, &self.kv, method, body, max_body),
            _ => Err(Failure::unimplemented(format!("method {method:#06x}"))),
        }
    }

    /// Gives the connection's credit back once half its window was read.
    fn release(&self, input: &mut Input, bytes: u64) {
        input.released += bytes;
        if input.released >= CONNECTION_CREDIT / 2 {
            let granted = std::mem::take(&mut input.released) as u32;
            self.send(&[Frame::new(Kind::Credit, 0, granted.to_le_bytes().to_vec())]);
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
        let mut output = self.lock_output();
        if output.ended && frames.iter().all(|frame| frame.kind != Kind::GoAway) {
            return;
        }
        for frame in frames {
            frame.encode_into(&mut output.bytes);
        }
        let wake = !std::mem::replace(&mut output.woken, true);
        drop(output);
        self.ready.notify_all();
        if let (true, Some(wake)) = (wake, &self.connect.wake) {
            wake();
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

/// How a method runs: kv's say theirs; every other engine's runs on a worker.
fn route(method: u16) -> Route {
    match method >> 8 {
        0x01 => kv::route(method),
        _ => Route::Worker,
    }
}

/// A call that panics answers `internal` rather than leaving its stream open.
fn guarded(call: impl FnOnce() -> std::result::Result<Vec<u8>, Failure>) -> std::result::Result<Vec<u8>, Failure> {
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
