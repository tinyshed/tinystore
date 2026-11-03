//! The server: JSON-RPC 2.0 a line on stdin, an answer a line on stdout,
//! flushed at once, since an agent's host waits for each. It answers
//! `initialize`, `ping`, `tools/list` and `tools/call`, and a notification
//! with nothing. A tool that writes or removes is offered, and runs, only
//! when the allow list names it.
//!
//! What each message asks and what is answered is the session's. Here each
//! call runs on a thread of its own, scoped to `serve`, so that a `ping` or
//! another call is answered while a slow one runs, and a cancel reaches it.

use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;
use std::thread;

use serde_json::Value;

use super::session::{Asked, Call, Session};
use super::tool::{Answer, Tool};
use crate::sync::lock;
use crate::text::panic_message;
use crate::{Error, Result};

/// An MCP server being built: its name, its tools and its allow list; it
/// answers with [`serve`](Server::serve).
#[derive(Debug)]
#[must_use = "a server answers with serve()"]
pub struct Server {
    pub(super) name: String,
    pub(super) version: String,
    pub(super) instructions: Option<String>,
    pub(super) tools: Vec<Tool>,
    pub(super) allowed: Option<String>,
}

impl Server {
    pub(super) fn new(name: &str, version: &str) -> Server {
        Server {
            name: name.to_owned(),
            version: version.to_owned(),
            instructions: None,
            tools: Vec::new(),
            allowed: None,
        }
    }

    /// What the agent is told the server is for. The server adds a sentence
    /// of its own: which tools write, or that nothing does.
    pub fn instructions(mut self, text: &str) -> Server {
        self.instructions = Some(text.to_owned());
        self
    }

    pub fn tool(mut self, tool: Tool) -> Server {
        self.tools.push(tool);
        self
    }

    pub fn tools(mut self, tools: impl IntoIterator<Item = Tool>) -> Server {
        self.tools.extend(tools);
        self
    }

    /// The tools that write or remove which the server offers, as a
    /// program's `--write` gives them: none, `all`, or names with commas
    /// between, as `dashboard_save,dashboard_delete`.
    pub fn allow<'a>(mut self, given: impl Into<Option<&'a str>>) -> Server {
        self.allowed = given.into().map(str::to_owned);
        self
    }

    /// Answers stdin until it closes, or until stdout's reader leaves. Two
    /// tools of one name, a schema that is not an object's, or an allow list
    /// that names no tool that writes is `Invalid`, before anything is read.
    ///
    /// Each call runs on a thread of its own, so the server answers other
    /// messages while it runs. A call its host cancels is told through its
    /// token and gets no answer. Once stdin closes, `serve` waits for the
    /// calls under way and writes their answers.
    pub fn serve(self) -> Result<()> {
        // std's Stdout, since its lock is not Send and each call's thread writes
        self.serve_on(io::stdin().lock(), io::stdout())
    }

    /// [`serve`](Server::serve) on any input and output, as a test drives it.
    #[doc(hidden)]
    pub fn serve_on(self, input: impl BufRead, output: impl Write + Send) -> Result<()> {
        let session = Session::of(self)?;
        let answers = Lines::to(output);
        let (session, lines) = (&session, &answers);
        let read = thread::scope(|calls| {
            for line in input.lines() {
                let line = line.map_err(|error| Error::io("mcp: stdin", error))?;
                match session.received(&line) {
                    Asked::Answer(answer) => answer.iter().for_each(|answer| lines.write(answer)),
                    Asked::Call(call) => {
                        calls.spawn(move || {
                            let answered = ran(&call);
                            session.finished(call, answered).iter().for_each(|answer| lines.write(answer));
                        });
                    }
                }
                if lines.stopped() {
                    break;
                }
            }
            if lines.stopped() {
                // the host has gone: nobody reads what the calls under way would answer
                session.cancel_all();
            }
            Ok(())
        });
        read.and(answers.ended())
    }
}

/// Runs `call`'s handler on this thread under the call's token, so that every
/// read it makes through a store stops once the call is cancelled. A handler
/// that panics answers its call as the tool's error, and the server goes on.
fn ran(call: &Call<'_>) -> Answer {
    // the server keeps nothing a handler's panic could leave half-changed
    let handled = AssertUnwindSafe(|| call.token.run(|| call.tool.call(&call.arguments, &call.token)));
    panic::catch_unwind(handled)
        .unwrap_or_else(|panicked| Err(format!("{} panicked: {}", call.tool.name, panic_message(&*panicked)).into()))
}

/// Where the answers go, each a line written whole under one lock and flushed
/// at once, in the order they are ready. The first failure stops it.
struct Lines<W> {
    output: Mutex<Output<W>>,
}

struct Output<W> {
    to: W,
    /// Why writing stopped: the host's leaving, or a failure `serve` returns.
    failed: Option<io::Error>,
}

impl<W: Write> Lines<W> {
    fn to(output: W) -> Lines<W> {
        Lines { output: Mutex::new(Output { to: output, failed: None }) }
    }

    fn write(&self, answer: &Value) {
        let mut output = lock(&self.output);
        if output.failed.is_some() {
            return;
        }
        let line = format!("{answer}\n");
        let to = &mut output.to;
        if let Err(error) = to.write_all(line.as_bytes()).and_then(|()| to.flush()) {
            output.failed = Some(error);
        }
    }

    fn stopped(&self) -> bool {
        lock(&self.output).failed.is_some()
    }

    /// What `serve` returns of its writing: a host that has gone is no
    /// failure, since nobody is left to answer.
    fn ended(&self) -> Result<()> {
        match lock(&self.output).failed.take() {
            Some(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(Error::io("mcp: stdout", error)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
