//! The client's side of the protocol, for the command line, which writes
//! through the server that holds a store: the handshake, and each call's
//! request and answer, over a byte stream the caller dialled. Requests and
//! answers are the protocol's own messages, so its bytes keep one writer and
//! one parser. It reads and writes on the caller's thread and starts none:
//! the caller dials, checks the server's proof and bounds the wait. It
//! promises nothing to a program, and the documentation leaves it out.
//!
//! ```text
//! HELLO    {1: 1, 2: "tinystore-cli/0.2.0", 6: challenge}
//! WELCOME  {2: "0.2.0", 5: max body, 7: connection credit, 11: proof, …}
//! REQUEST  stream 1, END, metrics.ingest {1: [series]}
//! RESPONSE stream 1, END, {}        or END·ERROR, {1: code, 2: message}
//! ```

use std::fmt;
use std::io::{self, Read, Write};

use super::codec::Message;
use super::frame::{self, Frame, Kind};
use super::message::kind_of;
use super::protocol::{self as wire, Empty, Failure, GoAway, Hello, method};
use crate::{Error, ErrorKind, Result};

/// The protocol this client speaks.
const PROTOCOL: u64 = 1;
/// The largest body read before the WELCOME agrees one.
const FIRST_BODY: usize = 1 << 20;
/// The bytes read from the stream at once.
const READ: usize = 64 << 10;
/// The most bytes a DATA of another program's output, a text or a file
/// carries: a transfer shares the connection a frame of 64 KiB at a time.
#[cfg(any(feature = "records", feature = "metrics", feature = "blobs"))]
const PIECE: usize = 64 << 10;
/// What a text's exposition carries beside its text and its labels, at most.
#[cfg(feature = "metrics")]
const EXPOSITION_FIELDS: usize = 64;
/// What a label of an exposition carries beside its name and its value.
#[cfg(feature = "metrics")]
const LABEL_FIELDS: usize = 10;

#[cfg(feature = "metrics")]
pub use crate::metrics::prometheus::PrometheusStream;
#[cfg(feature = "channels")]
pub use channels::Heard;
#[cfg(feature = "blobs")]
pub use files::FileInfo;
#[cfg(feature = "jobs")]
pub use jobs::NewJob;
pub use kv::KvValue;

#[cfg(feature = "channels")]
#[path = "client_channels.rs"]
mod channels;
#[cfg(feature = "blobs")]
#[path = "client_files.rs"]
mod files;
#[cfg(feature = "jobs")]
#[path = "client_jobs.rs"]
mod jobs;
#[path = "client_kv.rs"]
mod kv;

/// An event to append: its name, and what it carries.
#[cfg(feature = "records")]
#[derive(Debug, Default)]
pub struct Event {
    pub name: String,
    /// A JSON object.
    pub fields: Option<String>,
    /// As slog counts: -4 debug, 0 info, 4 warn, 8 error.
    pub level: Option<i64>,
    /// When it happened: when it is appended, unless given.
    pub at: Option<std::time::SystemTime>,
    /// What names it in its stream: an id the stream holds writes nothing.
    pub id: Option<String>,
}

/// What the server says of itself as it takes the connection.
#[derive(Debug)]
pub struct Welcome {
    /// Its version.
    pub server: String,
    /// The HMAC-SHA256 of the challenge, keyed with `SERVE`'s secret: the
    /// caller checks it before its first call.
    pub proof: Option<Vec<u8>>,
    /// `full` or `os` when the store was opened with one.
    pub durability: Option<String>,
}

/// A connection the server took, a call at a time.
pub struct Client<S> {
    stream: S,
    frames: frame::Reader,
    read: Vec<u8>,
    max_body: usize,
    /// The REQUEST and DATA bytes the server takes before it gives credit back.
    credit: u64,
    /// The DATA bytes a stream may send before the server grants more.
    stream_credit: u64,
    last_stream: u32,
}

impl<S> fmt::Debug for Client<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("max_body", &self.max_body).field("credit", &self.credit).finish()
    }
}

impl<S: Read + Write> Client<S> {
    /// Says HELLO as `name`, with the challenge a local server proves itself
    /// with, and gives the connection once the server welcomes it; a GOAWAY
    /// is its error.
    pub fn hello(stream: S, name: &str, challenge: Option<[u8; 16]>) -> Result<(Client<S>, Welcome)> {
        let mut client = Client {
            stream,
            frames: frame::Reader::default(),
            read: vec![0; READ],
            max_body: FIRST_BODY,
            credit: 0,
            stream_credit: 0,
            last_stream: 0,
        };
        let hello = Hello {
            protocol: PROTOCOL,
            client: name.to_owned(),
            challenge: challenge.map(Vec::from),
            ..Hello::default()
        };
        client.send(&Frame::new(Kind::Hello, 0, hello.encode())).map_err(unreachable)?;
        let answer = client.receive().map_err(unreachable)?;
        match answer.kind {
            Kind::Welcome => {
                let welcome = wire::Welcome::decode(&answer.body).map_err(|failure| error_of(failure, ""))?;
                client.max_body = usize::try_from(welcome.max_body).unwrap_or(usize::MAX);
                client.credit = welcome.connection_credit;
                client.stream_credit = welcome.stream_credit;
                Ok((client, Welcome { server: welcome.server, proof: welcome.proof, durability: welcome.durability }))
            }
            Kind::GoAway => Err(gone(&answer.body)),
            other => Err(Error::corrupt(format!("a {other:?} frame where the server's WELCOME belongs"))),
        }
    }

    /// Asks the server to stop once its streams finish. A server that closes
    /// the connection on its way out has done what was asked.
    pub fn stop(&mut self) -> Result<()> {
        match self.call(method::SERVER_STOP, &Empty {}) {
            Err(error) if error.kind() == ErrorKind::OutcomeUnknown => Ok(()),
            answered => answered.map(drop),
        }
    }

    /// Writes the series, every one or none, as `Metrics::ingest` does.
    #[cfg(feature = "metrics")]
    pub fn ingest(&mut self, series: &[crate::metrics::Series]) -> Result<()> {
        let series = series.iter().map(new_series).collect();
        self.call(method::METRICS_INGEST, &wire::MetricsIngest { series }).map(drop)
    }

    /// Writes the series of a text an exporter serves, every one or none, as
    /// `Prometheus::ingest` does: each with `labels` beside its own, a sample
    /// the text gives no time at `at`, each name kept `keep` when given. A
    /// text past what one message carries goes up in pieces.
    #[cfg(feature = "metrics")]
    pub fn ingest_prometheus(
        &mut self,
        text: &[u8],
        labels: &crate::metrics::Labels,
        at: std::time::SystemTime,
        keep: Option<std::time::Duration>,
    ) -> Result<()> {
        let (at, keep) = (Some(crate::unix_millis(at)), keep.map(millis_of));
        let fields: usize = labels.iter().map(|(name, value)| name.len() + value.len() + LABEL_FIELDS).sum();
        if text.len() + EXPOSITION_FIELDS + fields > self.max_body {
            let upload = wire::MetricsUpload { size: text.len() as u64, labels: labels.clone(), at, keep };
            let mut pieces = text.chunks(PIECE);
            let mut next = || Ok(pieces.next().map(|piece| wire::MetricsPiece { bytes: piece.to_vec() }.encode()));
            return self.exchange(method::METRICS_UPLOAD_PROMETHEUS, &upload, &mut next).map(drop);
        }
        let exposition = wire::MetricsExposition { text: text.to_vec(), labels: labels.clone(), at, keep };
        self.call(method::METRICS_INGEST_PROMETHEUS, &exposition).map(drop)
    }

    /// Gives `name` the unit and the help described, as `Metrics::describe`
    /// does.
    #[cfg(feature = "metrics")]
    pub fn describe(&mut self, name: &str, description: &crate::metrics::Description) -> Result<()> {
        let described = wire::MetricsDescription {
            name: name.to_owned(),
            unit: description.unit.clone(),
            help: description.help.clone(),
        };
        self.call(method::METRICS_DESCRIBE, &described).map(drop)
    }

    /// Deletes the series of `name` whose labels are `labels`, exactly, as
    /// `Metrics::delete` does: whether the store held it.
    #[cfg(feature = "metrics")]
    pub fn delete_series(&mut self, name: &str, labels: &crate::metrics::Labels) -> Result<bool> {
        let named = wire::MetricsSeriesNamed { name: name.to_owned(), labels: labels.clone() };
        Ok(self.answer::<wire::MetricsDeleted>(method::METRICS_DELETE, &named)?.found)
    }

    /// Appends the events to `stream`, every one or none, as
    /// `Events::append_all` does: how many were written as nothing, the
    /// stream holding their ids.
    #[cfg(feature = "records")]
    pub fn append(&mut self, stream: &str, events: &[Event]) -> Result<u64> {
        let handle = self.events(stream)?;
        let events = events.iter().map(new_event).collect();
        Ok(self.answer::<wire::RecordsAppended>(method::RECORDS_APPEND, &wire::RecordsAppend { handle, events })?.held)
    }

    /// Writes another program's output to `stream` as `Events::lines` does,
    /// read from `output` until it ends: a line an event, a stack trace
    /// joined into one. What the writer dropped comes back; `output`'s own
    /// failure is said as `output_failed` says it.
    #[cfg(feature = "records")]
    pub fn append_lines(
        &mut self,
        stream: &str,
        output: &mut impl Read,
        output_failed: impl Fn(io::Error) -> Error,
    ) -> Result<u64> {
        let handle = self.events(stream)?;
        let mut piece = vec![0; PIECE];
        let answer = self.exchange(method::RECORDS_LINES, &wire::Handle { handle }, &mut || {
            let read = output.read(&mut piece).map_err(&output_failed)?;
            Ok((read > 0).then(|| wire::RecordsPiece { bytes: piece[..read].to_vec() }.encode()))
        })?;
        Ok(decoded::<wire::RecordsDropped>(&answer)?.dropped)
    }

    /// The handle of the stream named `stream`, made with its first event.
    #[cfg(feature = "records")]
    fn events(&mut self, stream: &str) -> Result<u64> {
        self.handle(method::RECORDS_OPEN, &wire::RecordsOpen { stream: stream.to_owned(), keep: None })
    }

    /// Sends one call and reads its answer as an `M`.
    fn answer<M: Message>(&mut self, called: u16, request: &impl Message) -> Result<M> {
        decoded(&self.call(called, request)?)
    }

    /// Sends a call that opens something, and gives its handle.
    fn handle(&mut self, called: u16, request: &impl Message) -> Result<u64> {
        Ok(self.answer::<wire::Handle>(called, request)?.handle)
    }

    /// Sends one call and waits for its answer. A connection lost before the
    /// request went is `unavailable`; after, `outcome_unknown`, since the
    /// server may have run it.
    fn call(&mut self, called: u16, request: &impl Message) -> Result<Vec<u8>> {
        let stream = self.request(called, request, true)?;
        loop {
            let frame = self.receive().map_err(unanswered)?;
            match frame.kind {
                Kind::Response if frame.stream == stream => return ended(frame),
                _ => self.take(frame).map_err(unanswered)?,
            }
        }
    }

    /// Opens a stream with `request`, sends what `next` gives as its DATA,
    /// each within the credit the server grants, ends it, and gives the
    /// server's last message on it: an upload's answer, or why it stopped.
    #[cfg(any(feature = "records", feature = "metrics", feature = "blobs"))]
    fn exchange(
        &mut self,
        called: u16,
        request: &impl Message,
        next: &mut dyn FnMut() -> Result<Option<Vec<u8>>>,
    ) -> Result<Vec<u8>> {
        let stream = self.request(called, request, false)?;
        loop {
            let frame = self.receive().map_err(unanswered)?;
            match frame.kind {
                Kind::Response if frame.stream == stream && frame.flags & frame::END != 0 => return ended(frame),
                Kind::Response if frame.stream == stream => break,
                _ => self.take(frame).map_err(unanswered)?,
            }
        }
        let mut granted = self.stream_credit;
        while let Some(body) = next()? {
            let length = body.len() as u64;
            if let Some(end) = self.room(stream, length, &mut granted).map_err(unanswered)? {
                return ended(end);
            }
            self.send(&Frame::new(Kind::Data, stream, body)).map_err(unanswered)?;
            self.credit -= length;
            granted -= length;
        }
        self.send(&Frame::new(Kind::Data, stream, Vec::new()).ending()).map_err(unanswered)?;
        loop {
            let frame = self.receive().map_err(unanswered)?;
            if frame.stream == stream && frame.flags & frame::END != 0 {
                return ended(frame);
            }
            self.take_granted(frame, stream, &mut granted).map_err(unanswered)?;
        }
    }

    /// Sends a REQUEST once the connection's credit takes it, ending its
    /// stream when `ends`, and gives the stream's number.
    fn request(&mut self, called: u16, request: &impl Message, ends: bool) -> Result<u32> {
        let body = request.encode();
        if body.len() > self.max_body {
            return Err(Error::limit(format!(
                "a request of {} bytes, past the {} the server takes",
                body.len(),
                self.max_body
            )));
        }
        let length = body.len() as u64;
        while self.credit < length {
            let frame = self.receive().map_err(unreachable)?;
            self.take(frame).map_err(unreachable)?;
        }
        self.last_stream = self.last_stream.checked_add(1).unwrap_or(1);
        let stream = self.last_stream;
        let frame = Frame { method: called, ..Frame::new(Kind::Request, stream, body) };
        self.send(&if ends { frame.ending() } else { frame }).map_err(unreachable)?;
        self.credit -= length;
        Ok(stream)
    }

    /// Reads frames until the connection and `stream` take `length` more
    /// bytes; the server's end of the stream, should it come first.
    #[cfg(any(feature = "records", feature = "metrics", feature = "blobs"))]
    fn room(&mut self, stream: u32, length: u64, granted: &mut u64) -> io::Result<Option<Frame>> {
        while self.credit < length || *granted < length {
            let frame = self.receive()?;
            if frame.stream == stream && frame.flags & frame::END != 0 {
                return Ok(Some(frame));
            }
            self.take_granted(frame, stream, granted)?;
        }
        Ok(None)
    }

    /// `take`, and credit granted to `stream` counted.
    #[cfg(any(feature = "records", feature = "metrics", feature = "blobs"))]
    fn take_granted(&mut self, frame: Frame, stream: u32, granted: &mut u64) -> io::Result<()> {
        if frame.kind == Kind::Credit && frame.stream == stream {
            *granted += credit_of(&frame.body);
            return Ok(());
        }
        self.take(frame)
    }

    /// What a frame that answers no call does: credit is counted, a PING
    /// answered. A GOAWAY lets the call already sent finish, and a frame of a
    /// stream no call waits on is a call given up.
    fn take(&mut self, frame: Frame) -> io::Result<()> {
        match frame.kind {
            Kind::Credit if frame.stream == 0 => {
                self.credit += credit_of(&frame.body);
                Ok(())
            }
            Kind::Ping => self.send(&Frame::new(Kind::Pong, 0, frame.body)),
            _ => Ok(()),
        }
    }

    fn send(&mut self, frame: &Frame) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(frame::HEADER + frame.body.len());
        frame.encode_into(&mut bytes);
        self.stream.write_all(&bytes)?;
        self.stream.flush()
    }

    fn receive(&mut self) -> io::Result<Frame> {
        loop {
            match self.frames.next(self.max_body) {
                Ok(Some(frame)) => return Ok(frame),
                Ok(None) => {}
                Err(refused) => return Err(io::Error::new(io::ErrorKind::InvalidData, refused.to_string())),
            }
            let read = self.stream.read(&mut self.read)?;
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.frames.push(&self.read[..read]);
        }
    }
}

/// The bytes a CREDIT grants.
fn credit_of(body: &[u8]) -> u64 {
    let granted: [u8; 4] = body.get(..4).and_then(|bytes| bytes.try_into().ok()).unwrap_or_default();
    u64::from(u32::from_le_bytes(granted))
}

/// A message the server answered with, read.
fn decoded<M: Message>(body: &[u8]) -> Result<M> {
    M::decode(body).map_err(|failure| error_of(failure, "the server's answer"))
}

/// A span as the protocol carries one, in milliseconds.
fn millis_of(span: std::time::Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

/// The answer a frame that ends its stream gives: its body, or the failure
/// it says.
fn ended(frame: Frame) -> Result<Vec<u8>> {
    if frame.flags & frame::ERROR != 0 { Err(failed(&frame.body)) } else { Ok(frame.body) }
}

/// A series as `metrics.ingest` carries it: its samples as two columns of
/// eight bytes a sample.
#[cfg(feature = "metrics")]
fn new_series(series: &crate::metrics::Series) -> wire::MetricsNewSeries {
    let samples = series.samples();
    let times = samples.iter().flat_map(|sample| crate::unix_millis(sample.at).to_le_bytes()).collect();
    let values = samples.iter().flat_map(|sample| sample.value.to_le_bytes()).collect();
    wire::MetricsNewSeries {
        name: series.name().to_owned(),
        kind: series.kind().as_str().to_owned(),
        labels: series.labels().clone(),
        times,
        values,
        keep: series.keep_given().map(millis_of),
    }
}

/// An event as `records.append` carries it.
#[cfg(feature = "records")]
fn new_event(event: &Event) -> wire::RecordsNewEvent {
    wire::RecordsNewEvent {
        name: event.name.clone(),
        at: event.at.map(crate::unix_nanos),
        level: event.level,
        fields: event.fields.clone(),
        id: event.id.clone(),
        ..wire::RecordsNewEvent::default()
    }
}

fn unreachable(error: io::Error) -> Error {
    Error::new(ErrorKind::Unavailable, "the server's connection").with_source(error)
}

fn unanswered(error: io::Error) -> Error {
    Error::new(ErrorKind::OutcomeUnknown, "the server's connection, after the call was sent").with_source(error)
}

/// The error a stream's failure says.
fn failed(body: &[u8]) -> Error {
    match Failure::decode(body) {
        Ok(failure) => error_of(failure, ""),
        Err(_) => Error::corrupt("a failure the server sent that does not read as one"),
    }
}

/// The error a GOAWAY says, the server not taking the connection.
fn gone(body: &[u8]) -> Error {
    match GoAway::decode(body) {
        Ok(away) => error_of(Failure { code: away.code, message: away.message, what: None }, "the server"),
        Err(_) => Error::corrupt("a GOAWAY the server sent that does not read as one"),
    }
}

/// The error a failure names, said as the server said it: its kind by its
/// code, its text without the kind the error's own text ends with, and its
/// facts. A code that names no kind of the core's is an internal error that
/// keeps the code in its text.
fn error_of(failure: Failure, within: &str) -> Error {
    let Failure { code, message, what } = failure;
    let error = match kind_of(&code) {
        Some(kind) if message == kind.as_str() => Error::new(kind, ""),
        Some(kind) => Error::new(kind, message.strip_suffix(&format!(": {}", kind.as_str())).unwrap_or(&message)),
        None => Error::internal(format!("{message} ({code})")),
    };
    let error = what.unwrap_or_default().into_iter().fold(error, |error, (name, value)| error.naming(name, value));
    if within.is_empty() { error } else { error.within(within) }
}

#[cfg(all(test, feature = "metrics"))]
#[path = "client_tests.rs"]
mod tests;
