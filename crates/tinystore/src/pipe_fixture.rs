//! A client's side of a pipe, frames out and frames back as an SDK speaks
//! them, for the tests of every engine's methods.

use std::path::Path;
use std::time::Duration;

use super::Pipe;
use crate::wire::codec::Message;
use crate::wire::frame::{self, Frame, Kind};
use crate::wire::protocol::{Failure, GoAway, Handle, Hello, KvBucketOpen, KvCall, Welcome, method};

pub(crate) struct Client {
    pub(crate) pipe: Pipe,
    pub(crate) reader: frame::Reader,
    /// Frames read while waiting for another stream's.
    early: Vec<Frame>,
    pub(crate) next_stream: u32,
}

impl Client {
    pub(crate) fn connect(dir: &Path) -> Client {
        Client::over(Pipe::open(dir, &[], None).unwrap())
    }

    /// A client of `store` admitted as a server admits a token of `capability`.
    pub(crate) fn admitted(store: &crate::Store, capability: super::Capability) -> Client {
        let admit: super::Admit = std::sync::Arc::new(move |_: Option<&str>| Some(capability));
        let connect = super::Connect { admit: Some(admit), ..super::Connect::default() };
        let mut client = Client::over(Pipe::connect(store, connect).unwrap());
        let welcome = client.greet(Hello { protocol: 2, token: Some("a token".to_owned()), ..Hello::default() });
        assert!(welcome.is_ok(), "{welcome:?}");
        client
    }

    pub(crate) fn over(pipe: Pipe) -> Client {
        Client { pipe, reader: frame::Reader::default(), early: Vec::new(), next_stream: 0 }
    }

    pub(crate) fn open(dir: &Path) -> Client {
        let mut client = Client::connect(dir);
        let welcome = client.hello(2).unwrap();
        assert_eq!(welcome.protocol, 2);
        client
    }

    /// Says HELLO in `protocol`, and returns the WELCOME or the GOAWAY.
    pub(crate) fn hello(&mut self, protocol: u64) -> Result<Welcome, GoAway> {
        self.greet(Hello { protocol, client: "test/0".to_owned(), ..Hello::default() })
    }

    pub(crate) fn greet(&mut self, hello: Hello) -> Result<Welcome, GoAway> {
        self.write(Frame::new(Kind::Hello, 0, hello.encode()));
        let answer = self.read();
        match answer.kind {
            Kind::Welcome => Ok(Welcome::decode(&answer.body).unwrap()),
            Kind::GoAway => Err(GoAway::decode(&answer.body).unwrap()),
            other => panic!("a {other:?} for HELLO"),
        }
    }

    pub(crate) fn write(&mut self, frame: Frame) {
        let mut bytes = Vec::new();
        frame.encode_into(&mut bytes);
        let ready = self.pipe.send(&bytes);
        self.reader.push(&ready);
    }

    pub(crate) fn read(&mut self) -> Frame {
        loop {
            if let Some(frame) = self.reader.next(1 << 22).unwrap() {
                return frame;
            }
            let bytes = self.pipe.recv(Duration::from_secs(10));
            assert!(!bytes.is_empty(), "the pipe answered nothing within ten seconds");
            self.reader.push(&bytes);
        }
    }

    /// The next frame on `stream`, keeping those of other streams for later.
    pub(crate) fn next_on(&mut self, stream: u32) -> Frame {
        if let Some(at) = self.early.iter().position(|frame| frame.stream == stream) {
            return self.early.remove(at);
        }
        loop {
            let frame = self.read();
            match frame.kind {
                Kind::Credit => {}
                _ if frame.stream == stream => return frame,
                _ => self.early.push(frame),
            }
        }
    }

    /// The credit the server gives back next on `stream`, for the DATA the
    /// client sent on it; the frames of other kinds kept for later.
    #[cfg(feature = "blobs")]
    pub(crate) fn credit_on(&mut self, stream: u32) -> u64 {
        loop {
            let frame = self.read();
            match frame.kind {
                Kind::Credit if frame.stream == stream => {
                    return u64::from(u32::from_le_bytes(frame.body[..4].try_into().unwrap()));
                }
                Kind::Credit => {}
                _ => self.early.push(frame),
            }
        }
    }

    /// Whether no frame of `stream` arrives within `wait`, the frames that do
    /// arrive kept for later.
    #[cfg(any(feature = "jobs", feature = "sql"))]
    pub(crate) fn silent_on(&mut self, stream: u32, wait: Duration) -> bool {
        let bytes = self.pipe.recv(wait);
        self.reader.push(&bytes);
        while let Some(frame) = self.reader.next(1 << 22).unwrap() {
            if frame.kind != Kind::Credit {
                self.early.push(frame);
            }
        }
        !self.early.iter().any(|frame| frame.stream == stream)
    }

    /// Sends a call and returns its stream.
    pub(crate) fn start(&mut self, called: u16, request: &impl Message) -> u32 {
        self.next_stream += 1;
        let frame = Frame { method: called, ..Frame::new(Kind::Request, self.next_stream, request.encode()) };
        self.write(frame.ending());
        self.next_stream
    }

    /// Sends a REQUEST that leaves its stream open for the client's DATA, and
    /// returns its stream.
    pub(crate) fn open_stream(&mut self, called: u16, request: &impl Message) -> u32 {
        self.next_stream += 1;
        let body = request.encode();
        self.write(Frame { method: called, ..Frame::new(Kind::Request, self.next_stream, body) });
        self.next_stream
    }

    /// Asks for a run of once: its REQUEST leaves the stream open for the
    /// answer the client sends back when the run is handed to it.
    pub(crate) fn run(&mut self, request: &KvCall) -> u32 {
        self.open_stream(method::KV_ONCE_RUN, request)
    }

    /// Calls a method and returns its answer, or its error.
    pub(crate) fn call<A: Message>(&mut self, called: u16, request: &impl Message) -> Result<A, Failure> {
        let stream = self.start(called, request);
        let answer = self.next_on(stream);
        assert_eq!(answer.kind, Kind::Response);
        assert_ne!(answer.flags & frame::END, 0, "a call's answer ends its stream");
        answered(&answer)
    }

    pub(crate) fn bucket(&mut self, name: &str) -> u64 {
        let open = KvBucketOpen { name: name.to_owned(), ..KvBucketOpen::default() };
        self.call::<Handle>(method::KV_BUCKET_OPEN, &open).unwrap().handle
    }
}

pub(crate) fn answered<A: Message>(frame: &Frame) -> Result<A, Failure> {
    match frame.flags & frame::ERROR {
        0 => Ok(A::decode(&frame.body).unwrap()),
        _ => Err(Failure::decode(&frame.body).unwrap()),
    }
}
