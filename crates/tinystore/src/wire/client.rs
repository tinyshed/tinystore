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
    /// The REQUEST bytes the server takes before it gives credit back.
    credit: u64,
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
    /// the text gives no time at `at`, each name kept `keep` when given.
    #[cfg(feature = "metrics")]
    pub fn ingest_prometheus(
        &mut self,
        text: &[u8],
        labels: &crate::metrics::Labels,
        at: std::time::SystemTime,
        keep: Option<std::time::Duration>,
    ) -> Result<()> {
        let exposition = wire::MetricsExposition {
            text: text.to_vec(),
            labels: labels.clone(),
            at: Some(crate::unix_millis(at)),
            keep: keep.map(|keep| u64::try_from(keep.as_millis()).unwrap_or(u64::MAX)),
        };
        self.call(method::METRICS_INGEST_PROMETHEUS, &exposition).map(drop)
    }

    /// Sends one call and waits for its answer. A connection lost before the
    /// request went is `unavailable`; after, `outcome_unknown`, since the
    /// server may have run it.
    fn call(&mut self, called: u16, request: &impl Message) -> Result<Vec<u8>> {
        let body = request.encode();
        if body.len() > self.max_body {
            return Err(Error::limit(format!(
                "a request of {} bytes, past the {} the server takes",
                body.len(),
                self.max_body
            )));
        }
        let length = body.len() as u64;
        self.wait_for_credit(length).map_err(unreachable)?;
        self.last_stream = self.last_stream.checked_add(1).unwrap_or(1);
        let stream = self.last_stream;
        let request = Frame { method: called, ..Frame::new(Kind::Request, stream, body) }.ending();
        self.send(&request).map_err(unreachable)?;
        self.credit -= length;
        loop {
            let frame = self.receive().map_err(unanswered)?;
            match frame.kind {
                Kind::Response if frame.stream == stream && frame.flags & frame::ERROR != 0 => {
                    return Err(failed(&frame.body));
                }
                Kind::Response if frame.stream == stream => return Ok(frame.body),
                _ => self.take(frame).map_err(unanswered)?,
            }
        }
    }

    /// Reads frames until the connection's credit covers `length` bytes.
    fn wait_for_credit(&mut self, length: u64) -> io::Result<()> {
        while self.credit < length {
            let frame = self.receive()?;
            self.take(frame)?;
        }
        Ok(())
    }

    /// What a frame that answers no call does: credit is counted, a PING
    /// answered. A GOAWAY lets the call already sent finish, and a frame of a
    /// stream no call waits on is a call given up.
    fn take(&mut self, frame: Frame) -> io::Result<()> {
        match frame.kind {
            Kind::Credit if frame.stream == 0 => {
                let granted: [u8; 4] = frame.body.get(..4).and_then(|bytes| bytes.try_into().ok()).unwrap_or_default();
                self.credit += u64::from(u32::from_le_bytes(granted));
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
        keep: series.keep_given().map(|keep| u64::try_from(keep.as_millis()).unwrap_or(u64::MAX)),
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
