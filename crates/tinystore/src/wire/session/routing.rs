//! Where an admitted call runs, as its method's route says: answered where
//! it is read, queued for a commit, handed over, or run on a worker.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{ClientRun, HELLO_MOST, Input, OUTPUT_LOW, OUTPUT_MOST, Shared, guarded, method_name};
use crate::clock::from_unix_millis;
use crate::pipe::Capability;
use crate::sync::lock;
use crate::wire::Route;
#[cfg(feature = "blobs")]
use crate::wire::blobs;
#[cfg(feature = "channels")]
use crate::wire::channels;
use crate::wire::codec::Message;
#[cfg(feature = "config")]
use crate::wire::config;
use crate::wire::frame::{Frame, Kind};
#[cfg(feature = "jobs")]
use crate::wire::jobs;
use crate::wire::kv::{self, Run};
#[cfg(feature = "locks")]
use crate::wire::locks;
#[cfg(feature = "metrics")]
use crate::wire::metrics;
use crate::wire::protocol::method::{self, Access};
use crate::wire::protocol::{Empty, Failure, ServerClock};
#[cfg(feature = "records")]
use crate::wire::records;
#[cfg(feature = "search")]
use crate::wire::search;
#[cfg(feature = "sql")]
use crate::wire::sql;
use crate::{Clock, unix_millis};

impl Shared {
    /// Answers a call as its method's route says: a point read before this
    /// returns, a write once its group commits, a run of once handed over,
    /// anything else on a worker.
    pub(super) fn request(self: &Arc<Self>, input: &mut Input, frame: Frame) {
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
        self.count_request(frame.method);
        // A point read is answered where it is read, in less time than
        // another thread takes to wake, while its host waits for nothing
        // else. A crowd of them would each wait its turn on this thread,
        // which is all a host with one thread has: a crowd goes to the
        // workers, where sixteen run together, and so does a read that comes
        // while they still have reads of this connection, whose answers the
        // host comes back for anyway.
        let route = route(frame.method);
        if route == Route::Inline && (input.crowd || self.reads_away.load(Ordering::Acquire) > 0) {
            let max_body = input.max_body.unwrap_or(HELLO_MOST);
            return self.read_away(frame.stream, frame.method, frame.body, max_body);
        }
        self.run_route(input, route, frame);
    }

    fn count_request(&self, method: u16) {
        if self.store.probe().listening() {
            self.store.probe().count("tinystore_wire_requests_total", &[("method", method_name(method))], 1.0);
        }
    }

    fn run_route(self: &Arc<Self>, input: &mut Input, route: Route, frame: Frame) {
        let max_body = input.max_body.unwrap_or(HELLO_MOST);
        let (method, stream, body) = (frame.method, frame.stream, frame.body);
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
            #[cfg(feature = "metrics")]
            Route::Gather => self.gather(input, stream, &body),
            #[cfg(feature = "config")]
            Route::Follow => self.follow(input, stream, &body),
            #[cfg(feature = "backup")]
            Route::Backup => self.backup(input, stream, &body),
            #[cfg(feature = "locks")]
            Route::Hold => self.hold(stream, &body),
            #[cfg(feature = "channels")]
            Route::Subscribe => self.subscribe(input, stream, &body),
            Route::Worker => self.run_on_worker(stream, method, body, max_body),
        }
    }

    /// Answers a call on a worker: a read stops at its stream's CANCEL, and
    /// a write runs to its end, its answer past the agreed body
    /// `outcome_unknown`.
    fn run_on_worker(self: &Arc<Self>, stream: u32, method: u16, body: Vec<u8>, max_body: usize) {
        let (shared, reading) = (Arc::clone(self), self.reading(stream, method));
        let writes = method::access(method) == Some(Access::Write);
        self.run_call(Box::new(move || {
            let answered = shared.read_under(stream, reading, || guarded(|| shared.call(method, &body, max_body)));
            shared.answer(stream, if writes { shared.written_within_agreed(answered) } else { answered });
        }));
    }

    /// Hands a write to its engine on a worker, which answers once its group
    /// commits.
    fn submit(self: &Arc<Self>, stream: u32, method: u16, body: Vec<u8>) {
        let shared = Arc::clone(self);
        self.run_call(Box::new(move || {
            let answering = Arc::clone(&shared);
            let done = move |answered| answering.answer(stream, answering.written_within_agreed(answered));
            let queued = guarded(|| {
                match method >> 8 {
                    #[cfg(feature = "jobs")]
                    0x02 => jobs::submit(&shared.jobs, method, &body, done),
                    #[cfg(feature = "sql")]
                    0x03 => sql::submit(&shared.sql, (method, &body), &|called, body| shared.lend(called, body), done),
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
        self.run_call(Box::new(move || shared.run_once(stream, asked, body, None)));
    }

    /// Answers a point read on a worker, counted as away until it is.
    fn read_away(self: &Arc<Self>, stream: u32, method: u16, body: Vec<u8>, max_body: usize) {
        self.reads_away.fetch_add(1, Ordering::AcqRel);
        let shared = Arc::clone(self);
        self.run_call(Box::new(move || {
            let answered = guarded(|| shared.call(method, &body, max_body));
            shared.reads_away.fetch_sub(1, Ordering::AcqRel);
            shared.answer(stream, answered);
        }));
    }

    /// Runs a call on a worker once the connection owes its client within
    /// its bound. The frames of one write are all read before a worker
    /// answers any, so the bound looked at where frames are read holds none
    /// of them: each call looks again where it starts, and one past the
    /// bound waits, as a subscription does, until the host takes what is
    /// owed. What the workers had started still answers: a call on each,
    /// and the writes they queued for a commit.
    pub(super) fn run_call(self: &Arc<Self>, call: Box<dyn FnOnce() + Send>) {
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || shared.start_call(call)));
    }

    /// Runs `call` here, or once the host took what is owed. Between the two
    /// marks a call waits behind those already waiting, which the calls that
    /// keep coming would otherwise hold off for as long as they come.
    fn start_call(self: &Arc<Self>, call: Box<dyn FnOnce() + Send>) {
        let owing = self.owing.load(Ordering::SeqCst);
        if owing <= OUTPUT_LOW || (owing <= OUTPUT_MOST && lock(&self.waiting_output).is_empty()) {
            return call();
        }
        let shared = Arc::clone(self);
        self.when_drained(Box::new(move || shared.start_call(call)));
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
            0x04 => blobs::call(&self.store, &self.blobs, &lent, method, body, max_body),
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
pub(super) fn route(method: u16) -> Route {
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
