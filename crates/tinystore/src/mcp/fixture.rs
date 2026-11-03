//! What the server's tests share: the messages an agent's host sends, a
//! server answering a whole input at once, and one answering on a thread of
//! its own as messages arrive.

use std::io::{self, BufReader, Cursor, PipeWriter, Write};
use std::sync::mpsc;
use std::thread;

use serde_json::{Value, json};

use super::Server;
use crate::testing::{PATIENCE, holds_within};

pub(crate) fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub(crate) fn call(id: u64, name: &str, arguments: Value) -> Value {
    request(id, "tools/call", json!({ "name": name, "arguments": arguments }))
}

/// The host giving up on the call `id`.
pub(crate) fn cancelled(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": id, "reason": "the person moved on" } })
}

/// What `server` answers to `messages`, in the order of their ids: calls run
/// side by side, and their answers are written as each finishes.
pub(crate) fn answers(server: Server, messages: &[Value]) -> Vec<Value> {
    let input: String = messages.iter().map(|message| format!("{message}\n")).collect();
    let mut output = Vec::new();
    server.serve_on(Cursor::new(input), &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    let mut read: Vec<Value> = output.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    read.sort_by_key(|answer| answer["id"].as_u64());
    read
}

/// A server answering on a thread of its own, as an agent's host runs one:
/// each message goes in when the test sends it, and each answer comes out as
/// it is written.
pub(crate) struct Running {
    input: Option<PipeWriter>,
    answers: mpsc::Receiver<Value>,
    served: thread::JoinHandle<crate::Result<()>>,
}

impl Running {
    pub(crate) fn start(server: Server) -> Running {
        let (reader, writer) = io::pipe().unwrap();
        let (answer, answers) = mpsc::channel();
        // a test that stopped reading has failed already
        let output = answering(move |line| {
            answer.send(line).ok();
        });
        let served = thread::spawn(move || server.serve_on(BufReader::new(reader), output));
        Running { input: Some(writer), answers, served }
    }

    pub(crate) fn send(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("stdin is open");
        input.write_all(format!("{message}\n").as_bytes()).unwrap();
    }

    /// The next answer written.
    pub(crate) fn next(&self) -> Value {
        self.answers.recv_timeout(PATIENCE).expect("an answer in time")
    }

    /// Closes stdin, and once `serve` has returned, the answers written since
    /// the last one read.
    pub(crate) fn close(mut self) -> Vec<Value> {
        drop(self.input.take());
        holds_within(PATIENCE, "serve returned once stdin closed", || self.served.is_finished());
        self.served.join().unwrap().unwrap();
        self.answers.try_iter().collect()
    }
}

/// An output that hands each line to `answer` as it is written, read as JSON.
pub(crate) fn answering(answer: impl FnMut(Value) + Send + 'static) -> impl Write + Send + 'static {
    Answers { line: Vec::new(), answer }
}

struct Answers<A> {
    line: Vec<u8>,
    answer: A,
}

impl<A: FnMut(Value)> Write for Answers<A> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.line.extend_from_slice(bytes);
        while let Some(end) = self.line.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.line.drain(..=end).collect();
            (self.answer)(serde_json::from_slice(&line).unwrap());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
