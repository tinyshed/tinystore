//! A program's own MCP server, awaited on the program's runtime: the
//! protocol, the marks and the allow list of [`crate::mcp`], with tools whose
//! handlers await. A handler takes the call's arguments and its token, and
//! returns the future of its answer, which owns what it awaits with:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # struct Pool;
//! # impl Pool { async fn rows(&self, sql: &str) -> std::io::Result<Vec<String>> { Ok(vec![sql.to_owned()]) } }
//! use serde_json::json;
//! use tinystore::aio::mcp::{self, Tool};
//!
//! # async fn serve(pool: Arc<Pool>, store: tinystore::aio::Store) -> Result<(), Box<dyn std::error::Error>> {
//! let takes = json!({ "type": "object", "properties": { "sql": { "type": "string" } }, "required": ["sql"] });
//! mcp::server("dashbin", env!("CARGO_PKG_VERSION"))
//!     .tools(mcp::read_tools(&store).prefix("store_")) // blocking, each call on a thread of its own
//!     .tool(Tool::reads("query", "Rows of a read-only statement.", takes, move |given, _| {
//!         let pool = Arc::clone(&pool);
//!         async move {
//!             let sql = given["sql"].as_str().ok_or("query takes sql")?;
//!             Ok(json!(pool.rows(sql).await?))
//!         }
//!     }))
//!     .serve()
//!     .await?; // answers stdin until it closes
//! # Ok(())
//! # }
//! ```
//!
//! Every call runs beside the others. One whose handler awaits is a future
//! that `serve`'s own future polls, so every such call runs on the task that
//! awaits `serve`, and nothing is spawned; a blocking [`crate::mcp::Tool`],
//! [`read_tools`] among them, runs on a thread of its own, so that it holds
//! up no other call.
//!
//! A call its host cancels gets no answer: its token is cancelled and its
//! future dropped, which stops what it awaits where it is, a read through
//! the store among it. The future is polled under the call's token, so a
//! read of the store whose future the handler made and handed elsewhere, as
//! to `tokio::spawn`, stops too, `Cancelled`. A blocking tool is told
//! through its token, as [`crate::mcp::Tool`] says.
//!
//! The server reads stdin on a thread of its own, since std reads it only
//! blocking and TinyStore brings no runtime, and writes stdout on another,
//! so that a host slow to read holds up no poll.

use std::fmt;
use std::future::{Future, poll_fn};
use std::io::{self, BufRead, BufReader, Write};
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll};
use std::thread;

use serde_json::Value;

use super::handlers::{self, Handlers};
use super::oneshot;
use crate::mcp::server::{Lines, ran};
use crate::mcp::session::{Asked, Call, Session, Setup};
use crate::mcp::tool::{Answer, Blocking, Card, Mark};
use crate::{CancellationToken, Error, Result};

/// The future of a handler's answer.
type Answering = handlers::Handler<Answer>;

type Awaited = Arc<dyn Fn(Value, CancellationToken) -> Answering + Send + Sync>;

/// How a tool's handler runs: on a thread of its own, or as a future that
/// `serve`'s polls.
#[derive(Clone)]
enum Handler {
    Blocking(Blocking),
    Awaited(Awaited),
}

/// A server named `name` at `version`, as the agent's host lists it, with no
/// tool yet.
pub fn server(name: &str, version: &str) -> Server {
    Server { setup: Setup::new(name, version), tools: Vec::new() }
}

/// tinystore's tools that read `store`, as [`crate::mcp::read_tools`] gives
/// them. Their reads block, so each call runs on a thread of its own.
#[cfg(feature = "store")]
pub fn read_tools(store: &crate::aio::Store) -> crate::mcp::Tools {
    crate::mcp::read_tools(store.blocking())
}

/// An MCP server being built: its name, its tools and its allow list; it
/// answers with [`serve`](Server::serve), awaited.
#[derive(Debug)]
#[must_use = "a server answers with serve().await"]
pub struct Server {
    setup: Setup,
    tools: Vec<Tool>,
}

impl Server {
    /// What the agent is told the server is for. The server adds a sentence
    /// of its own: which tools write, or that nothing does.
    pub fn instructions(mut self, text: &str) -> Server {
        self.setup.instructions = Some(text.to_owned());
        self
    }

    /// A tool whose handler awaits, or a blocking [`crate::mcp::Tool`].
    pub fn tool(mut self, tool: impl Into<Tool>) -> Server {
        self.tools.push(tool.into());
        self
    }

    pub fn tools(mut self, tools: impl IntoIterator<Item = impl Into<Tool>>) -> Server {
        self.tools.extend(tools.into_iter().map(Into::into));
        self
    }

    /// The tools that write or remove which the server offers, as a
    /// program's `--write` gives them: none, `all`, or names with commas
    /// between, as `dashboard_save,dashboard_delete`.
    pub fn allow<'a>(mut self, given: impl Into<Option<&'a str>>) -> Server {
        self.setup.allowed = given.into().map(str::to_owned);
        self
    }

    /// Answers stdin until it closes, or until stdout's reader leaves, as
    /// [`crate::mcp::Server::serve`] does: once stdin closes, it waits for
    /// the calls under way and writes their answers. Two tools of one name, a
    /// schema that is not an object's, or an allow list that names no tool
    /// that writes is `Invalid`, before anything is read.
    ///
    /// Its future dropped, the calls under way are cancelled and their
    /// futures dropped. Its thread that reads stdin waits in its read until
    /// the next line or stdin's end, holding stdin's lock until then: a
    /// program that drops the future while stdin stays open, and reads
    /// stdin itself, waits for that line.
    pub async fn serve(self) -> Result<()> {
        self.serve_on(BufReader::new(io::stdin()), io::stdout()).await
    }

    /// [`serve`](Server::serve) on any input and output, as a test drives it.
    #[doc(hidden)]
    pub async fn serve_on(
        self,
        input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
    ) -> Result<()> {
        let (cards, handlers): (Vec<Card>, Vec<Handler>) =
            self.tools.into_iter().map(|tool| (tool.card, tool.handler)).unzip();
        let session = Session::of(self.setup, cards)?;
        let shared = Arc::new(Shared::default());
        let answers = writing(output, &shared)?;
        reading(input, &shared)?;
        let mut serving =
            Serving { session, handlers, shared, answers: Some(answers), calls: Handlers::default(), read: None };
        poll_fn(|context| serving.poll(context)).await
    }
}

/// A tool an agent may call, whose handler awaits. It takes the call's
/// arguments and its token, and returns the future of its answer: JSON,
/// which the agent reads as text, or an error, which it reads as the tool's
/// error with its message. The future owns what it awaits with, so a
/// handler clones into it what its calls share, as a pool of connections.
#[derive(Clone)]
pub struct Tool {
    card: Card,
    handler: Handler,
}

impl Tool {
    /// A tool that only reads, offered always.
    pub fn reads<F>(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(Value, CancellationToken) -> F + Send + Sync + 'static,
    ) -> Tool
    where
        F: Future<Output = Answer> + Send + 'static,
    {
        Tool::awaiting(Card::new(Mark::Reads, name, description, schema), handler)
    }

    /// A tool that writes and removes nothing, offered only when the allow
    /// list names it.
    pub fn writes<F>(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(Value, CancellationToken) -> F + Send + Sync + 'static,
    ) -> Tool
    where
        F: Future<Output = Answer> + Send + 'static,
    {
        Tool::awaiting(Card::new(Mark::Writes, name, description, schema), handler)
    }

    /// A tool that deletes something or replaces what was there, offered only
    /// when the allow list names it.
    pub fn removes<F>(
        name: &str,
        description: &str,
        schema: Value,
        handler: impl Fn(Value, CancellationToken) -> F + Send + Sync + 'static,
    ) -> Tool
    where
        F: Future<Output = Answer> + Send + 'static,
    {
        Tool::awaiting(Card::new(Mark::Removes, name, description, schema), handler)
    }

    fn awaiting<F>(card: Card, handler: impl Fn(Value, CancellationToken) -> F + Send + Sync + 'static) -> Tool
    where
        F: Future<Output = Answer> + Send + 'static,
    {
        let boxed = move |given, token| -> Answering { Box::pin(handler(given, token)) };
        Tool { card, handler: Handler::Awaited(Arc::new(boxed)) }
    }
}

impl From<crate::mcp::Tool> for Tool {
    /// A blocking tool, whose every call runs on a thread of its own.
    fn from(tool: crate::mcp::Tool) -> Tool {
        Tool { card: tool.card, handler: Handler::Blocking(tool.handler) }
    }
}

impl fmt::Debug for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool").field("name", &self.card.name).field("mark", &self.card.mark).finish_non_exhaustive()
    }
}

/// A server answering: `serve`'s future, polled by the program's runtime.
struct Serving {
    session: Session,
    handlers: Vec<Handler>,
    shared: Arc<Shared>,
    /// Where answers go to be written; dropped once the server is done,
    /// which ends the writing once what was sent is written.
    answers: Option<mpsc::Sender<Value>>,
    /// The calls under way, each with its handler's future: an awaited one's,
    /// or the answer to come of a blocking one running on a thread of its own.
    calls: Handlers<Call, Answer>,
    /// How stdin ended, once it has.
    read: Option<Result<()>>,
}

/// What `serve`'s future shares with the threads that read stdin and write
/// stdout, beside the calls woken and the task to wake.
type Shared = handlers::Shared<Mail>;

#[derive(Default)]
struct Mail {
    lines: Vec<String>,
    /// How stdin ended: at its end, or failing.
    read: Option<Result<()>>,
    /// How the writing ended: every answer written, or stdout's reader gone.
    written: Option<Result<()>>,
    /// `serve`'s future has ended or was dropped: a thread that posts stops.
    gone: bool,
}

impl Serving {
    fn poll(&mut self, context: &mut Context<'_>) -> Poll<Result<()>> {
        let (woken, (lines, read, written)) = self
            .shared
            .look(context.waker(), |mail| (mem::take(&mut mail.lines), mail.read.take(), mail.written.take()));
        for line in &lines {
            match self.session.received(line) {
                Asked::Answer(answer) => self.answer(answer),
                Asked::Call(call) => self.start(call),
            }
        }
        // a cancelled call's future dropped stops what it awaits where it is, and it gets no answer
        for call in self.calls.stop(|call| call.token.is_cancelled()) {
            self.session.dropped(&call);
        }
        for number in woken {
            if let Some((call, ran)) = self.calls.poll(number) {
                self.answer(self.session.finished(call, ran));
            }
        }
        if let Some(read) = read {
            self.read = Some(read);
        }
        if let Some(written) = written {
            return Poll::Ready(self.ended(written));
        }
        if self.read.is_some() && self.calls.is_empty() {
            // the writing thread writes what was sent, then posts that it is done
            self.answers = None;
        }
        Poll::Pending
    }

    /// What `serve` returns once the writing ended: every answer written, or
    /// stdout's reader gone, which leaves nobody to read the calls under way.
    fn ended(&mut self, written: Result<()>) -> Result<()> {
        self.session.cancel_all();
        self.calls.stop(|_| true);
        self.read.take().unwrap_or(Ok(())).and(written)
    }

    fn answer(&self, answer: Option<Value>) {
        if let (Some(answer), Some(answers)) = (answer, &self.answers) {
            // a writer that stopped posts why, and the server ends at its next poll
            answers.send(answer).ok();
        }
    }

    /// Begins a call: an awaited handler's future made and polled under the
    /// call's token, or a blocking handler on a thread of its own.
    fn start(&mut self, mut call: Call) {
        let (arguments, token) = (mem::take(&mut call.arguments), call.token.clone());
        let ended = match self.handlers[call.tool].clone() {
            Handler::Awaited(handler) => {
                let make = || token.run(|| scoped(handler(arguments, token.clone()), token.clone()));
                self.calls.begin(call, make, &self.shared)
            }
            Handler::Blocking(handler) => match on_a_thread(handler, arguments, token) {
                Ok(answering) => self.calls.begin(call, || answering, &self.shared),
                Err(error) => Some((call, Ok(Err(format!("no thread to run it on: {error}").into())))),
            },
        };
        if let Some((call, ran)) = ended {
            self.answer(self.session.finished(call, ran));
        }
    }
}

impl Drop for Serving {
    /// `serve`'s future ended, or was dropped: the calls under way are
    /// cancelled, a blocking one told through its token, and the threads
    /// stop at their next post.
    fn drop(&mut self) {
        self.session.cancel_all();
        self.shared.with(|mail| mail.gone = true);
        self.shared.forget_task();
    }
}

/// A call's future, each poll of it under the call's token, so that a read
/// of the store made in it stops once the call is cancelled, wherever that
/// read is awaited.
fn scoped(future: Answering, token: CancellationToken) -> Answering {
    Box::pin(Scoped { future, token })
}

struct Scoped {
    future: Answering,
    token: CancellationToken,
}

impl Future for Scoped {
    type Output = Answer;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Answer> {
        let Scoped { future, token } = &mut *self;
        token.run(|| future.as_mut().poll(context))
    }
}

/// Runs a blocking handler on a thread of its own, so that it holds up no
/// other call: the future of its answer, which resumes the handler's panic
/// where it is polled.
fn on_a_thread(handler: Blocking, arguments: Value, token: CancellationToken) -> io::Result<Answering> {
    let (answer, answered) = oneshot::pending::<Answer>("an mcp tool's call");
    let run = move || match ran(&handler, &arguments, &token) {
        Ok(answering) => answer.send(Ok(answering)),
        Err(panic) => answer.panicked(panic),
    };
    thread::Builder::new().name("mcp call".to_owned()).spawn(run)?;
    Ok(Box::pin(async move { answered.await.unwrap_or_else(|closed| Err(closed.into())) }))
}

/// Starts the thread that writes each answer sent to what this returns, a
/// line each, and posts how the writing ended: once every answer was
/// written and the sender dropped, or once stdout's reader has gone.
fn writing(output: impl Write + Send + 'static, shared: &Arc<Shared>) -> Result<mpsc::Sender<Value>> {
    let (answers, sent) = mpsc::channel::<Value>();
    let shared = Arc::clone(shared);
    let write = move || {
        let lines = Lines::to(output);
        for answer in sent {
            lines.write(&answer);
            if lines.stopped() {
                break;
            }
        }
        let written = lines.ended();
        shared.post(|mail| mail.written = Some(written));
    };
    let started = thread::Builder::new().name("mcp stdout".to_owned()).spawn(write);
    started.map_err(|error| Error::io("mcp: a thread to write stdout on", error))?;
    Ok(answers)
}

/// Starts the thread that reads stdin and posts each line, then how stdin
/// ended. Once `serve`'s future is gone the thread ends at its next line, or
/// at stdin's end: std's read of stdin cannot be interrupted.
fn reading(input: impl BufRead + Send + 'static, shared: &Arc<Shared>) -> Result<()> {
    let shared = Arc::clone(shared);
    let read = move || {
        for line in input.lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    shared.post(|mail| mail.read = Some(Err(Error::io("mcp: stdin", error))));
                    return;
                }
            };
            let posted = shared.post(|mail| {
                mail.lines.push(line);
                !mail.gone
            });
            if !posted {
                return;
            }
        }
        shared.post(|mail| mail.read = Some(Ok(())));
    };
    let started = thread::Builder::new().name("mcp stdin".to_owned()).spawn(read);
    started.map(drop).map_err(|error| Error::io("mcp: a thread to read stdin on", error))
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
