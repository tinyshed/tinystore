//! The frames a client sends, each answered once it is whole: the handshake
//! that agrees what the connection takes, and the DATA of its streams handed
//! to whoever takes them.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use super::{
    CONNECTION_CREDIT, ClientRun, HELLO_MOST, IN_FLIGHT, Input, OUTPUT_LOW, OUTPUT_MOST, STREAM_CREDIT, Shared, guarded,
};
use crate::pipe::Capability;
use crate::unix_millis;
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
#[cfg(feature = "jobs")]
use crate::wire::jobs;
use crate::wire::kv;
#[cfg(feature = "jobs")]
use crate::wire::protocol::{Empty, Failure};
use crate::wire::protocol::{GoAway, Hello, Welcome};

/// The protocol this server speaks; a client of another is refused.
const PROTOCOL: u64 = 1;
const MAX_BODY: u64 = 4 << 20;
/// What a client grants a stream when its HELLO does not say.
const CLIENT_STREAM_CREDIT: u64 = 2 << 20;
/// The bytes of a HELLO's challenge, which a local server's proof answers.
const CHALLENGE: usize = 16;

impl Shared {
    /// Answers the frames the reader holds whole, until the connection owes
    /// its client past its bound: the rest wait in the reader, read once the
    /// host takes what is owed.
    pub(super) fn read_frames(self: &Arc<Self>, input: &mut Input) {
        while !input.ended {
            if input.max_body.is_some() && self.owes_past_bound() {
                return;
            }
            let max_body = input.max_body.unwrap_or(HELLO_MOST);
            match input.reader.next(max_body) {
                Ok(Some(frame)) => self.handle(input, frame),
                Ok(None) => return,
                Err(refused) => self.go_away(input, "protocol", &refused.0),
            }
        }
    }

    /// Whether the connection owes its client past its bound, which holds the
    /// frames that come. A take that lowers what is owed looks at `held`
    /// after it counted, and this looks at what is owed again after it set
    /// `held`: one of the two sees the other, so frames are never held with
    /// nobody to read them on.
    fn owes_past_bound(&self) -> bool {
        if self.owing.load(Ordering::SeqCst) > OUTPUT_MOST {
            self.held.store(true, Ordering::SeqCst);
            if self.owing.load(Ordering::SeqCst) > OUTPUT_MOST {
                return true;
            }
        }
        if self.held.load(Ordering::Relaxed) {
            self.held.store(false, Ordering::SeqCst);
        }
        false
    }

    /// Reads the frames held for the host's take, once the connection owes
    /// little.
    pub(super) fn read_held(self: &Arc<Self>) {
        if !self.held.load(Ordering::SeqCst) || self.owing.load(Ordering::SeqCst) > OUTPUT_LOW {
            return;
        }
        let mut input = self.lock_input();
        self.read_frames(&mut input);
    }

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

    pub(super) fn go_away(&self, input: &mut Input, code: &str, message: &str) {
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
        if let Some(gathering) = self.metrics.gathering(frame.stream) {
            return gathering.piece(frame.body, frame.flags & frame::END != 0);
        }
        #[cfg(feature = "metrics")]
        if self.metrics.batch(frame.stream, &frame.body, frame.flags & frame::END != 0) {
            return;
        }
        #[cfg(feature = "sql")]
        if let Some(tx) = self.sql.tx(frame.stream) {
            // its credit goes back once the transaction answered the call
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
        // The stream's grant waits for half its window, which nine workers
        // sharing the connection's window may never reach: the connection
        // gets its bytes back as soon as the item is read.
        self.let_go_of(frame.stream, frame.body.len() as u64);
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
    pub(super) fn end_work(self: &Arc<Self>, stream: u32, last: std::result::Result<Vec<u8>, Failure>) {
        let Some(work) = self.jobs.take_work(stream) else {
            return;
        };
        let shared = Arc::clone(self);
        self.workers.run(Box::new(move || {
            work.end();
            shared.end_stream(stream, last);
        }));
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
