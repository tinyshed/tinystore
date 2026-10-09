use std::hash::{BuildHasher, Hasher, RandomState};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::frame::{self, Frame, Kind};
use super::kv;
use super::message::{Answer, Failure, Fields};
use super::msgpack::Value;
use super::workers::Workers;
use crate::{Result, Store, unix_millis};

const PROTOCOL: u64 = 1;
const MAX_BODY: u64 = 4 << 20;
const IN_FLIGHT: u64 = 64;
/// Bytes of REQUEST bodies a client may send before credit comes back.
const CONNECTION_CREDIT: u64 = 8 << 20;
const STREAM_CREDIT: u64 = 2 << 20;
/// What a client grants a stream when its HELLO does not say.
const CLIENT_STREAM_CREDIT: u64 = 2 << 20;
/// The largest HELLO read before a body is agreed.
const HELLO_MOST: usize = 1 << 16;

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
    ready: Condvar,
    wake: Option<Wake>,
}

#[derive(Default)]
struct Input {
    reader: frame::Reader,
    /// The largest body both sides agreed, once WELCOME went out.
    max_body: Option<usize>,
    /// REQUEST bytes read since credit last went back.
    released: u64,
    ended: bool,
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    woken: bool,
    ended: bool,
}

impl Session {
    pub(crate) fn new(store: Store, wake: Option<Wake>) -> Result<Session> {
        let workers = Workers::of(&store)?;
        let shared = Shared {
            store,
            workers,
            kv: kv::Handles::default(),
            input: Mutex::new(Input::default()),
            output: Mutex::new(Output::default()),
            ready: Condvar::new(),
            wake,
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

    /// The bytes the client is owed, waiting up to `wait` for the first.
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

    /// Ends the session: calls under way finish, and what they answer is
    /// dropped.
    pub(crate) fn end(&self) {
        self.shared.lock_input().ended = true;
        let mut output = self.shared.lock_output();
        output.ended = true;
        output.bytes.clear();
        drop(output);
        self.shared.ready.notify_all();
    }
}

impl Shared {
    fn handle(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        match (input.max_body, frame.kind) {
            (None, Kind::Hello) => self.welcome(input, &frame.body),
            (None, kind) => self.go_away(input, "protocol", &format!("a {kind:?} before HELLO")),
            (Some(_), Kind::Request) => self.request(input, frame),
            (Some(_), Kind::Ping) => self.send(&[Frame::new(Kind::Pong, 0, frame.body)]),
            // calls are short and end on their own; downloads arrive with scan
            (Some(_), Kind::Cancel | Kind::Credit) => {}
            (Some(_), Kind::GoAway) => self.finish(input),
            (Some(_), kind) => self.go_away(input, "protocol", &format!("a {kind:?} from a client")),
        }
    }

    /// Answers HELLO with what the connection agrees: the older protocol of
    /// the two, and the smallest body either side takes.
    fn welcome(&self, input: &mut Input, body: &[u8]) {
        let hello = match Fields::decode(body, &[1, 2, 3, 4, 5, 6]) {
            Ok(hello) => hello,
            Err(failure) => return self.go_away(input, "protocol", &failure.message),
        };
        let agreed = |hello: &Fields| -> std::result::Result<u64, Failure> {
            let protocol = hello.uint(1, "protocol")?.unwrap_or(0);
            if protocol < PROTOCOL {
                return Err(Failure::invalid(format!("protocol {protocol}, older than {PROTOCOL}")));
            }
            let client_most = hello.uint(4, "max body")?.unwrap_or(MAX_BODY);
            let stream_credit = hello.uint(5, "stream credit")?.unwrap_or(CLIENT_STREAM_CREDIT);
            Ok(MAX_BODY.min(client_most).min(stream_credit))
        };
        let max_body = match agreed(&hello) {
            Ok(max_body) => max_body,
            Err(failure) => return self.go_away(input, "protocol", &failure.message),
        };
        input.max_body = Some(max_body as usize);
        self.send(&[Frame::new(Kind::Welcome, 0, self.welcome_body(max_body))]);
    }

    fn welcome_body(&self, max_body: u64) -> Vec<u8> {
        Answer::default()
            .put(1, Value::Uint(PROTOCOL))
            .put(2, Value::Str(env!("CARGO_PKG_VERSION").to_owned()))
            .put(3, Value::Bin(instance()))
            .put(4, Value::Str("admin".to_owned()))
            .put(5, Value::Uint(max_body))
            .put(6, Value::Uint(IN_FLIGHT))
            .put(7, Value::Uint(CONNECTION_CREDIT))
            .put(8, Value::Uint(STREAM_CREDIT))
            .put(9, Value::Array(vec![Value::Str("kv".to_owned())]))
            .put(10, int(unix_millis(self.store.now())))
            .encode()
    }

    /// Runs a call on the store's workers and answers it on its stream.
    fn request(self: &Arc<Self>, input: &mut Input, frame: Frame) {
        self.release(input, frame.body.len() as u64);
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            let answered =
                catch_unwind(AssertUnwindSafe(|| shared.call(frame.method, &frame.body))).unwrap_or_else(|_| {
                    Err(Failure { code: "internal", message: "a call panicked".to_owned(), what: Vec::new() })
                });
            let response = match answered {
                Ok(body) => Frame::new(Kind::Response, frame.stream, body).ending(),
                Err(failure) => Frame::new(Kind::Response, frame.stream, failure.encode()).failing(),
            };
            shared.send(&[response]);
        }));
    }

    fn call(&self, method: u16, body: &[u8]) -> std::result::Result<Vec<u8>, Failure> {
        match method >> 8 {
            0x01 => kv::call(&self.store, &self.kv, method, body),
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
        let body = Answer::default().put(1, Value::Str(code.to_owned())).put(2, Value::Str(message.to_owned()));
        self.send(&[Frame::new(Kind::GoAway, 0, body.encode())]);
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
        if let (true, Some(wake)) = (wake, &self.wake) {
            wake();
        }
    }

    fn lock_input(&self) -> MutexGuard<'_, Input> {
        self.input.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_output(&self) -> MutexGuard<'_, Output> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An integer as the profile writes it: unsigned when it is not negative.
pub(crate) fn int(value: i64) -> Value {
    u64::try_from(value).map_or(Value::Int(value), Value::Uint)
}

/// Sixteen bytes that differ for each session: the process's random hasher
/// keys, over the time and an address.
fn instance() -> Vec<u8> {
    let state = RandomState::new();
    let mut bytes = Vec::with_capacity(16);
    for half in 0..2u8 {
        let mut hasher = state.build_hasher();
        hasher.write_u8(half);
        hasher.write_u128(Instant::now().elapsed().as_nanos());
        hasher.write_usize(&bytes as *const _ as usize);
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes
}
